//! The driver over temporary directories: a data partition and a boot partition that are only
//! folders, and a boot record that writes down what it was told.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use ed25519_dalek::{Signer, SigningKey};
use lemnos_contract_system_slots::Slots;
use lemnos_core::{
    BoardDevice, BoardView, CallerId, Health, JobId, JobState, Lifecycle, Origin, Reply, Status,
};
use sha2::{Digest, Sha256};

use crate::boot::{BootRecord, hex};
use crate::json::Json;
use crate::manifest::BLOCK;
use crate::store::{self, Layout, Meta, State};
use crate::tar::tests::archive;
use crate::trial::{Log, Resolved, Source};
use crate::{SystemReboot, SystemSlots, TrustedKey};
use lemnos_slot_store::security;
use lemnos_slot_store::ubi::{Fake, Ubi, UbiWrite, image_volume};

pub struct TempDir(pub PathBuf);

impl TempDir {
    pub fn new(label: &str) -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "lemnos-slots-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// What the boot loader was told, in order, and what it holds: the default, a trial and the
/// trial's attempts. [`Told::boot`] starts a boot the way the loader does: a trial with attempts
/// left takes one before it starts.
#[derive(Clone, Default)]
struct Told(Arc<Mutex<Loader>>);

#[derive(Default)]
struct Loader {
    told: Vec<String>,
    /// The slots whose payloads were installed into a bay, in order.
    installed: Vec<String>,
    /// The slots whose bay was freed, in order.
    freed: Vec<String>,
    default: String,
    trial: String,
    attempts: u8,
}

impl Told {
    fn take(&self) -> Vec<String> {
        core::mem::take(&mut self.0.lock().unwrap().told)
    }

    /// The slot the next boot starts. A trial with attempts left takes one.
    fn boot(&self) -> String {
        let mut loader = self.0.lock().unwrap();
        if !loader.trial.is_empty() && loader.attempts > 0 {
            loader.attempts -= 1;
            return loader.trial.clone();
        }
        loader.default.clone()
    }

    /// The slots whose bay was freed so far.
    fn freed(&self) -> Vec<String> {
        self.0.lock().unwrap().freed.clone()
    }

    /// The slots installed into a bay so far.
    fn installed(&self) -> Vec<String> {
        self.0.lock().unwrap().installed.clone()
    }

    /// The loader's trial and its attempts left.
    fn trial(&self) -> (String, u8) {
        let loader = self.0.lock().unwrap();
        (loader.trial.clone(), loader.attempts)
    }
}

impl BootRecord for Told {
    fn set_default(&mut self, id: &str) -> std::io::Result<()> {
        let mut loader = self.0.lock().unwrap();
        loader.default = id.to_string();
        loader.told.push(format!("default {id}"));
        Ok(())
    }
    fn set_trial(&mut self, id: &str, attempts: u8) -> std::io::Result<()> {
        let mut loader = self.0.lock().unwrap();
        loader.trial = id.to_string();
        loader.attempts = attempts;
        loader.told.push(format!("trial {id} {attempts}"));
        Ok(())
    }
    fn pending(&self) -> std::io::Result<Option<(String, u8)>> {
        let loader = self.0.lock().unwrap();
        Ok((!loader.trial.is_empty()).then(|| (loader.trial.clone(), loader.attempts)))
    }
    fn set_attempts(&mut self, attempts: u8) -> std::io::Result<()> {
        let mut loader = self.0.lock().unwrap();
        if !loader.trial.is_empty() {
            loader.attempts = attempts;
        }
        Ok(())
    }
    fn clear_trial(&mut self) -> std::io::Result<()> {
        let mut loader = self.0.lock().unwrap();
        loader.trial.clear();
        loader.attempts = 0;
        loader.told.push("clear trial".into());
        Ok(())
    }
    fn install(&self, slot: &str, _payloads: &[&[u8]; 5]) -> std::io::Result<()> {
        self.0.lock().unwrap().installed.push(slot.to_string());
        Ok(())
    }

    fn free(&self, slot: &str) -> std::io::Result<()> {
        self.0.lock().unwrap().freed.push(slot.to_string());
        Ok(())
    }

    fn restart(&mut self) -> std::io::Result<()> {
        self.0.lock().unwrap().told.push("restart".into());
        Ok(())
    }
}

fn key() -> SigningKey {
    SigningKey::from_bytes(&[7; 32])
}

fn sha(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

const ROOT_HASH: &str = "5d6f2c1f0f4b3c9a8e7d6c5b4a39281706f5e4d3c2b1a09f8e7d6c5b4a392817";

/// A bundle's parts, to be changed by a test before it is packed.
struct Bundle {
    id: String,
    /// The board it is built for.
    board: String,
    /// Data blocks the image's hash tree covers.
    data_blocks: u64,
    image: Vec<u8>,
    boot: Vec<(String, Vec<u8>)>,
    /// Files in boot/ the manifest doesn't list.
    extra: Vec<(String, Vec<u8>)>,
    manifest_image_sha: Option<String>,
    signer: SigningKey,
    sign: bool,
    /// The key the record names, when not the signer.
    named: Option<SigningKey>,
    /// What `slot.sig` holds instead of the record, when set.
    sig_file: Option<Vec<u8>>,
}

/// The board the test boards are.
const BOARD: &str = "slots-test";

/// An image of `data_blocks` blocks of data and room for their hash tree after them.
fn image_of(id: &str, data_blocks: u64) -> Vec<u8> {
    let seed = format!("erofs image of {id} ");
    let len = data_blocks * BLOCK + crate::manifest::tree_bytes(data_blocks);
    seed.bytes().cycle().take(len as usize).collect()
}

impl Bundle {
    fn new(id: &str) -> Self {
        Self {
            id: id.into(),
            board: BOARD.into(),
            data_blocks: 2,
            image: image_of(id, 2),
            boot: vec![
                ("kernel8.img".into(), format!("kernel of {id}").into_bytes()),
                (
                    "cmdline.txt".into(),
                    format!(
                        "console=tty1 rootwait board.slot={id} board.roothash={ROOT_HASH} board.src./soc/mmc@7e202000=/soc/mmc@7e202000 panic=5\n"
                    )
                    .into_bytes(),
                ),
                ("initramfs.cpio".into(), b"initramfs".to_vec()),
                ("board.dtb".into(), b"device tree".to_vec()),
                ("overlays/lemnos-board.dtbo".into(), b"overlay".to_vec()),
            ],
            extra: Vec::new(),
            manifest_image_sha: None,
            signer: key(),
            sign: true,
            named: None,
            sig_file: None,
        }
    }

    fn manifest(&self) -> String {
        let files: Vec<String> = self
            .boot
            .iter()
            .map(|(n, b)| format!("\"{n}\":\"{}\"", sha(b)))
            .collect();
        format!(
            "{{\"schema\":3,\"id\":\"{}\",\"product\":\"an-os\",\"version\":\"1.0\",\"variant\":\"v\",\
             \"board\":\"{}\",\"kernel\":\"6.12\",\"display_name\":\"An OS\",\
             \"verity\":{{\"root_hash\":\"{ROOT_HASH}\",\"salt\":\"{ROOT_HASH}\",\"data_blocks\":{},\"hash_offset\":{}}},\
             \"image\":{{\"size\":{},\"sha256\":\"{}\"}},\"boot\":{{\"files\":{{{}}}}}}}",
            self.id,
            self.board,
            self.data_blocks,
            self.data_blocks * BLOCK,
            self.image.len(),
            self.manifest_image_sha
                .clone()
                .unwrap_or_else(|| sha(&self.image)),
            files.join(",")
        )
    }

    fn pack(&self) -> Vec<u8> {
        self.pack_with(&self.manifest())
    }

    /// The signature record this bundle's manifest gets: the named key's id and the signature.
    fn pack_record(&self) -> Vec<u8> {
        let manifest = self.manifest();
        let signature = self
            .signer
            .sign(&lemnos_slot_store::signing::slot_bundle_message(
                manifest.as_bytes(),
            ))
            .to_bytes();
        let named = self.named.as_ref().unwrap_or(&self.signer);
        let key_id: [u8; 32] = Sha256::digest(named.verifying_key().as_bytes()).into();
        lemnos_slot_store::signing::record_bytes(&key_id, &signature)
    }

    /// The bundle with `manifest` as its slot.json, signed as it is.
    fn pack_with(&self, manifest: &str) -> Vec<u8> {
        let manifest = manifest.to_string();
        let signature = self
            .signer
            .sign(&lemnos_slot_store::signing::slot_bundle_message(
                manifest.as_bytes(),
            ))
            .to_bytes();
        let mut files: Vec<(String, Vec<u8>)> = vec![("slot.json".into(), manifest.into_bytes())];
        if self.sign {
            let named = self.named.as_ref().unwrap_or(&self.signer);
            let key_id = Sha256::digest(named.verifying_key().as_bytes()).into();
            let record = lemnos_slot_store::signing::record_bytes(&key_id, &signature);
            let sig = self.sig_file.clone().unwrap_or(record);
            files.push(("slot.sig".into(), sig));
        }
        files.push(("rootfs.erofs".into(), self.image.clone()));
        files.push(("boot/".into(), Vec::new()));
        for (name, bytes) in self.boot.iter().chain(&self.extra) {
            files.push((format!("boot/{name}"), bytes.clone()));
        }
        let borrowed: Vec<(&str, &[u8])> = files
            .iter()
            .map(|(n, b)| (n.as_str(), b.as_slice()))
            .collect();
        archive(&borrowed)
    }
}

struct Board {
    dir: TempDir,
    slots: SystemSlots,
    told: Told,
}

impl Board {
    /// A board with slot `first` installed, the default and running, as a fresh board is.
    fn new(first: &str) -> Self {
        Self::with(first, TempDir::new("board"), None)
    }

    /// The same board on raw NAND: its images in volumes of a fake UBI device with `free` bytes.
    fn on_nand(first: &str, free: u64) -> (Self, Arc<Fake>) {
        let dir = TempDir::new("nand");
        let fake = Arc::new(Fake::new(nand(&dir.0), free));
        (Self::with(first, dir, Some(fake.clone())), fake)
    }

    fn with(first: &str, dir: TempDir, fake: Option<Arc<Fake>>) -> Self {
        let mut layout = layout(&dir.0);
        layout.ubi = fake.as_ref().map(|f| f.ubi.clone());
        fs::create_dir_all(layout.images()).unwrap();
        fs::create_dir_all(layout.boot.join("slots")).unwrap();
        let told = Told::default();
        let mut board = Self {
            slots: SystemSlots::with_volumes(
                layout.clone(),
                Box::new(told.clone()),
                vec![TrustedKey {
                    key: key().verifying_key(),
                    owner: "the build key".into(),
                }],
                fake.map(|f| f as Arc<dyn UbiWrite>),
            ),
            dir,
            told,
        };
        board.slots.partuuid = "1234abcd-02".into();
        board.slots.boot_args = "board".into();
        board.slots.board_id = BOARD.into();
        board.slots.reserve = 0;
        // Put in place as a first image would be, with nothing running yet.
        board.slots.running = Some(String::new());
        board.install_ok(first);
        board.boot_into(first);
        board.slots.confirm().unwrap();
        board.told.take();
        // The loader holds the default the record was given: the first slot.
        board.told.0.lock().unwrap().default = first.to_string();
        board
    }

    fn layout(&self) -> &Layout {
        &self.slots.layout
    }

    /// Starts the next boot as the loader starts it, with a boot id of its own and the driver
    /// fresh, as a boot gives it. Returns the slot the loader started.
    fn next_boot(&mut self, n: u32) -> String {
        let slot = self.told.boot();
        self.boot_into(&slot);
        self.slots.boot = format!("boot-{n}");
        // The boot began at time 0: `settle_at` counts seconds from there.
        self.slots.started_ns = Some(0);
        self.slots.looked = false;
        self.slots.seen = Vec::new();
        self.slots.snapshot_written = false;
        self.slots.resolve_at_start();
        self.slots.record_boot();
        slot
    }

    /// The board looked at as `view`, and the driver's time at `secs` into the boot.
    fn settle_at(&mut self, secs: u64, view: &View) -> Option<u64> {
        self.slots.watch(view);
        self.slots.settle(secs * 1_000_000_000)
    }

    /// Writes slot `id`'s device baseline as a good boot of it would have.
    fn baseline(&self, id: &str, text: &str) {
        store::replace(
            &self.layout().marker(&format!("{id}.devices")),
            text.as_bytes(),
        )
        .unwrap();
    }

    fn trial_slot(&mut self, id: &str) {
        self.slots.trial(id, false, false, false).unwrap();
        self.told.take();
    }

    fn boot_into(&mut self, id: &str) {
        self.slots.running = Some(id.into());
    }

    fn state(&self) -> State {
        State::load(self.layout())
    }

    fn meta(&self, id: &str) -> Meta {
        Meta::load(self.layout(), id)
    }

    /// Streams `bytes` in as `caller`, in pieces, and returns the job's end.
    fn stream(&mut self, caller: u64, bytes: &[u8], piece: usize) -> (Status, Vec<u8>, u64) {
        self.slots.caller(CallerId(caller));
        let job = self
            .slots
            .install(bytes.len() as u64, false, false)
            .unwrap();
        let mut last_progress = 0;
        let mut offset = 0u64;
        for chunk in bytes.chunks(piece) {
            self.slots.caller(CallerId(caller));
            if self.slots.write(offset, chunk).is_err() {
                break;
            }
            offset += chunk.len() as u64;
            if let Some(JobState::Running { done, total }) = self.poll(job) {
                assert_eq!(total, bytes.len() as u64);
                assert!(done >= last_progress);
                last_progress = done;
            }
        }
        let mut out = [0u8; 128];
        let mut reply = Reply::new(&mut out);
        match self.slots.job(job, &mut reply) {
            JobState::Finished(status) => {
                let len = reply.len();
                (status, out[..len].to_vec(), last_progress)
            }
            JobState::Running { .. } => panic!("the install never ended"),
        }
    }

    fn poll(&mut self, job: JobId) -> Option<JobState> {
        let install = self.slots.install.as_ref()?;
        if install.end.is_some() || install.job != job {
            return None;
        }
        let mut out = [0u8; 8];
        Some(self.slots.job(job, &mut Reply::new(&mut out)))
    }

    fn install_ok(&mut self, id: &str) {
        let (status, result, _) = self.stream(1, &Bundle::new(id).pack(), 4096);
        assert_eq!(status, Status::OK, "{}", self.state().error);
        assert_eq!(result, id.as_bytes());
    }

    fn work_left(&self) -> bool {
        fs::read_dir(self.layout().images())
            .unwrap()
            .flatten()
            .any(|e| {
                let name = e.file_name();
                let name = name.to_string_lossy();
                // `.incoming` stays; what counts is anything left inside it.
                if name == crate::install::INCOMING {
                    fs::read_dir(e.path()).unwrap().next().is_some()
                } else {
                    name.starts_with('.')
                }
            })
    }
}

fn layout(dir: &Path) -> Layout {
    Layout::new(dir.join("data"), dir.join("boot"))
}

fn nand(dir: &Path) -> Ubi {
    Ubi {
        sys: dir.join("sys"),
        dev: dir.join("dev"),
        device: "ubi0".into(),
    }
}

#[test]
fn a_signed_bundle_streams_in_and_is_put_where_the_boot_chain_looks() {
    let mut board = Board::new("os-1");
    let bundle = Bundle::new("os-2").pack();
    let (status, result, progress) = board.stream(1, &bundle, 1000);
    assert_eq!(status, Status::OK);
    assert_eq!(result, b"os-2");
    assert!(progress > 0);
    let layout = board.layout().clone();
    assert_eq!(
        fs::read(layout.image("os-2")).unwrap(),
        Bundle::new("os-2").image
    );
    assert_eq!(
        fs::read_to_string(layout.manifest("os-2")).unwrap(),
        Bundle::new("os-2").manifest()
    );
    let meta = board.meta("os-2");
    assert_eq!(meta.get("GOOD"), "untried");
    assert_eq!(meta.get("PINNED"), "no");
    assert_eq!(meta.seq(), 2);
    let state = board.state();
    assert_eq!(
        (state.staged.as_str(), state.state.as_str()),
        ("os-2", "staged")
    );
    // Its boot files are kept on the data partition; its command line is the bundle's, byte for
    // byte; nothing is on the boot partition until it is tried.
    let kept = crate::boot::kept(&layout, "os-2").unwrap();
    let cmdline = kept.iter().find(|(_, n)| n == "cmdline.txt").unwrap();
    assert_eq!(
        fs::read_to_string(layout.bootfiles().join(&cmdline.0)).unwrap(),
        format!(
            "console=tty1 rootwait board.slot=os-2 board.roothash={ROOT_HASH} board.src./soc/mmc@7e202000=/soc/mmc@7e202000 panic=5\n"
        )
    );
    assert!(kept.iter().any(|(_, n)| n == "slot.json"));
    assert!(!layout.boot_dir("os-2").exists());
    assert!(!board.work_left());
}

#[test]
fn with_the_switch_on_a_bundle_not_signed_by_a_trusted_key_is_refused_and_leaves_nothing() {
    let mut board = Board::new("os-1");
    security::set_view_for_thread(Some(view_file(&board.dir.0, true)));
    let mut bundle = Bundle::new("os-2");
    bundle.signer = SigningKey::from_bytes(&[9; 32]);
    let (status, _, _) = board.stream(1, &bundle.pack(), 4096);
    assert_eq!(status, Status::NOT_PERMITTED);
    let mut unsigned = Bundle::new("os-2");
    unsigned.sign = false;
    let (status, _, _) = board.stream(1, &unsigned.pack(), 4096);
    assert_eq!(status, Status::NOT_PERMITTED);
    assert!(!board.layout().installed("os-2"));
    assert!(!board.work_left());
    let state = board.state();
    assert_eq!(state.state, "error");
    assert!(state.error.contains("not signed"), "{}", state.error);
}

#[test]
fn an_image_or_boot_file_that_isnt_what_the_manifest_says_is_refused() {
    let mut board = Board::new("os-1");
    let mut wrong = Bundle::new("os-2");
    wrong.manifest_image_sha = Some(sha(b"something else"));
    assert_eq!(
        board.stream(1, &wrong.pack(), 4096).0,
        Status::INVALID_ARGUMENT
    );
    assert!(board.state().error.contains("sha256"));
    let mut extra = Bundle::new("os-2");
    extra.extra.push(("evil.dtbo".into(), b"x".to_vec()));
    assert_eq!(
        board.stream(1, &extra.pack(), 4096).0,
        Status::INVALID_ARGUMENT
    );
    assert!(board.state().error.contains("evil.dtbo"));
    let mut short = Bundle::new("os-2").pack();
    short.truncate(short.len() - 1024);
    assert_eq!(board.stream(1, &short, 4096).0, Status::INVALID_ARGUMENT);
    assert!(!board.layout().installed("os-2"));
    assert!(!board.work_left());
}

#[test]
fn a_bundle_for_the_running_or_default_slot_is_refused_before_its_image_comes() {
    let mut board = Board::new("os-1");
    let bundle = Bundle::new("os-1").pack();
    board.slots.caller(CallerId(1));
    let job = board
        .slots
        .install(bundle.len() as u64, false, false)
        .unwrap();
    // slot.json is the first member: its header and its bytes say what the bundle is.
    assert_eq!(board.slots.write(0, &bundle[..2048]), Err(Status::BUSY));
    let mut out = [0; 8];
    assert_eq!(
        board.slots.job(job, &mut Reply::new(&mut out)),
        JobState::Finished(Status::BUSY)
    );
    assert!(board.state().error.contains("running slot"));
    // The running slot is untouched.
    assert!(board.layout().installed("os-1"));
}

#[test]
fn only_the_starter_writes_and_only_where_the_install_is() {
    let mut board = Board::new("os-1");
    let bundle = Bundle::new("os-2").pack();
    board.slots.caller(CallerId(1));
    let job = board
        .slots
        .install(bundle.len() as u64, false, false)
        .unwrap();
    board.slots.caller(CallerId(2));
    assert_eq!(
        board.slots.write(0, &bundle[..512]),
        Err(Status::NOT_PERMITTED)
    );
    board.slots.caller(CallerId(1));
    // A second install waits for this one.
    assert_eq!(board.slots.install(10, false, false), Err(Status::BUSY));
    board.slots.write(0, &bundle[..512]).unwrap();
    assert_eq!(
        board.slots.write(1024, &bundle[1024..2048]),
        Err(Status::INVALID_ARGUMENT)
    );
    // Out of order changed nothing: it goes on from where it was.
    board.slots.write(512, &bundle[512..]).unwrap();
    let mut out = [0; 16];
    assert_eq!(
        board.slots.job(job, &mut Reply::new(&mut out)),
        JobState::Finished(Status::OK)
    );
}

#[test]
fn a_cancelled_install_ends_as_cancelled_and_leaves_nothing() {
    let mut board = Board::new("os-1");
    let bundle = Bundle::new("os-2").pack();
    board.slots.caller(CallerId(1));
    let job = board
        .slots
        .install(bundle.len() as u64, false, false)
        .unwrap();
    board.slots.write(0, &bundle[..3000]).unwrap();
    assert!(board.work_left());
    board.slots.job_cancel(job);
    let mut out = [0; 8];
    assert_eq!(
        board.slots.job(job, &mut Reply::new(&mut out)),
        JobState::Finished(Status::CANCELLED)
    );
    assert!(!board.work_left());
    assert_eq!(board.state().state, "cancelled");
    // The next install runs.
    board.install_ok("os-2");
}

#[test]
fn an_install_the_service_restart_cut_short_is_cleared_and_reported() {
    let mut board = Board::new("os-1");
    let bundle = Bundle::new("os-2").pack();
    board.slots.caller(CallerId(1));
    board
        .slots
        .install(bundle.len() as u64, false, false)
        .unwrap();
    board.slots.write(0, &bundle[..3000]).unwrap();
    // The service stops; a new driver starts on the same board.
    let layout = board.layout().clone();
    let again = SystemSlots::new(layout, Box::new(Told::default()), Vec::new());
    assert!(!board.work_left());
    let state = State::load(&again.layout);
    assert_eq!(state.state, "error");
    assert!(state.error.contains("interrupted"));
    assert!(again.install.is_none());
}

/// A boot is good only when the slot's boot files match its manifest, read raw from the boot disk.
/// With a firmware and no boot disk the confirm still happens (the trial worked), the boot count is
/// kept, and the state says why: nothing clears a count for a boot that was not verified.
#[test]
fn a_confirm_on_a_firmware_board_keeps_the_count_when_the_boot_files_cannot_be_read() {
    let mut board = Board::new("os-1");
    board.install_ok("os-2");
    board.slots.trial("os-2", false, false, false).unwrap();
    board.told.take();
    board.boot_into("os-2");
    let plain = board.dir.0.join("not-a-mailbox");
    fs::write(&plain, b"plain").unwrap();
    board.slots.firmware = Some(crate::count::Firmware {
        device_tree: board.dir.0.join("device-tree"),
        vcio: plain,
    });
    board.slots.confirm().unwrap();
    assert_eq!(board.meta("os-2").get("GOOD"), "yes");
    let error = board.state().error;
    assert!(error.contains("os-2 is not good yet"), "{error}");
    assert!(error.contains("the count is kept"), "{error}");
}

#[test]
fn a_confirmed_trial_becomes_the_default_and_old_slots_past_keep_go() {
    let mut board = Board::new("os-1");
    board.slots.set_keep(2).unwrap();
    for id in ["os-2", "os-3"] {
        board.install_ok(id);
        board.slots.trial(id, false, false, false).unwrap();
        assert_eq!(
            board.told.take(),
            [format!("trial {id} 3"), "restart".to_string()]
        );
        // Its payloads went into a bay before the trial pointer named it (the install, before the
        // `trial` call that the record was told about).
        assert!(board.told.installed().contains(&id.to_string()));
        assert_eq!(board.state().trial, id);
        board.boot_into(id);
        board.slots.confirm().unwrap();
        assert_eq!(
            board.told.take(),
            [format!("default {id}"), "clear trial".to_string()]
        );
        assert_eq!(board.meta(id).get("GOOD"), "yes");
    }
    let state = board.state();
    assert_eq!(
        (
            state.default.as_str(),
            state.previous.as_str(),
            state.trial.as_str()
        ),
        ("os-3", "os-2", "")
    );
    assert_eq!(state.state, "confirmed");
    // Keep 2: the oldest went, with its files.
    assert!(!board.layout().installed("os-1"));
    assert!(!board.layout().boot_dir("os-1").exists());
    assert!(board.layout().installed("os-2"));
    // Its bay was freed by the record (the install stages into bays, nothing is a file on the volume).
    assert_eq!(board.told.freed(), ["os-1"]);
}

#[test]
fn keep_old_pinned_and_keep_zero_hold_slots_back_from_housekeeping() {
    let mut board = Board::new("os-1");
    board.slots.set_keep(1).unwrap();
    board.slots.pin("os-1", true).unwrap();
    board.install_ok("os-2");
    board.slots.trial("os-2", false, false, false).unwrap();
    board.boot_into("os-2");
    board.slots.confirm().unwrap();
    // Pinned: it stays.
    assert!(board.layout().installed("os-1"));
    board.install_ok("os-3");
    board.slots.trial("os-3", false, true, false).unwrap();
    board.boot_into("os-3");
    board.slots.confirm().unwrap();
    // keep_old: nothing goes this time.
    assert!(board.layout().installed("os-2"));
    board.slots.set_keep(0).unwrap();
    board.install_ok("os-4");
    board.slots.trial("os-4", false, false, false).unwrap();
    board.boot_into("os-4");
    board.slots.confirm().unwrap();
    assert_eq!(board.layout().slots(), ["os-1", "os-2", "os-3", "os-4"]);
}

#[test]
fn a_trial_on_a_single_slot_board_falls_back_to_the_one_slot() {
    let mut board = Board::new("os-1");
    assert_eq!(board.state().previous, "");
    board.install_ok("os-2");
    board.slots.trial("os-2", false, false, false).unwrap();
    // The trial never came up: the firmware's next ordinary boot starts the default.
    board.boot_into("os-1");
    board.slots.confirm().unwrap();
    let state = board.state();
    assert_eq!(
        (
            state.default.as_str(),
            state.previous.as_str(),
            state.trial.as_str()
        ),
        ("os-1", "", "")
    );
    assert_eq!(state.state, "rolled-back");
    assert!(state.error.contains("back on os-1"));
    assert!(board.told.take().iter().all(|t| !t.starts_with("default")));
    let meta = board.meta("os-2");
    assert_eq!((meta.get("GOOD"), meta.failed()), ("no", true));
    // A failed slot is tried again, or made the default, only when forced.
    assert_eq!(
        board.slots.trial("os-2", false, false, false),
        Err(Status::NOT_PERMITTED)
    );
    board.slots.trial("os-2", false, false, true).unwrap();
}

#[test]
fn a_trial_that_keeps_the_default_leaves_it() {
    let mut board = Board::new("os-1");
    board.install_ok("os-2");
    board.slots.trial("os-2", true, false, false).unwrap();
    board.boot_into("os-2");
    board.slots.confirm().unwrap();
    board.told.take();
    assert_eq!(board.state().default, "os-1");
    assert_eq!(board.meta("os-2").get("GOOD"), "yes");
    // Booted well: it may now be made the default directly.
    board.slots.set_default("os-2", false).unwrap();
    assert_eq!(board.told.take(), ["default os-2"]);
    let state = board.state();
    assert_eq!(
        (state.default.as_str(), state.previous.as_str()),
        ("os-2", "os-1")
    );
}

#[test]
fn set_default_wants_a_slot_that_booted_well() {
    let mut board = Board::new("os-1");
    board.install_ok("os-2");
    assert_eq!(
        board.slots.set_default("os-2", false),
        Err(Status::NOT_PERMITTED)
    );
    assert_eq!(
        board.slots.set_default("nope", false),
        Err(Status::NOT_FOUND)
    );
    assert!(board.told.take().is_empty());
}

#[test]
fn remove_never_takes_a_slot_the_board_needs() {
    let mut board = Board::new("os-1");
    assert_eq!(board.slots.remove("os-1"), Err(Status::BUSY));
    board.install_ok("os-2");
    board.install_ok("os-3");
    board.slots.trial("os-2", false, false, false).unwrap();
    assert_eq!(board.slots.remove("os-2"), Err(Status::BUSY));
    board.slots.pin("os-3", true).unwrap();
    assert_eq!(board.slots.remove("os-3"), Err(Status::NOT_PERMITTED));
    board.slots.pin("os-3", false).unwrap();
    board.slots.remove("os-3").unwrap();
    assert!(!board.layout().installed("os-3"));
    assert!(board.meta("os-3").0.is_empty());
    assert_eq!(board.slots.remove("os-3"), Err(Status::NOT_FOUND));
    assert_eq!(board.slots.remove("../x"), Err(Status::INVALID_ARGUMENT));
}

#[test]
fn the_list_says_what_is_installed_and_which_is_which() {
    let mut board = Board::new("os-1");
    board.install_ok("os-2");
    let mut out = vec![0u8; 64 * 1024];
    let len = board.slots.list(&mut out).unwrap();
    let json = Json::parse(core::str::from_utf8(&out[..len]).unwrap()).unwrap();
    assert_eq!(json.get("slot_active").unwrap().text(), Some("os-1"));
    assert_eq!(json.get("slot_default").unwrap().text(), Some("os-1"));
    assert_eq!(json.get("keep").unwrap().u64(), Some(3));
    let Some(Json::List(slots)) = json.get("slots") else {
        panic!("no slots")
    };
    assert_eq!(slots.len(), 2);
    // Newest first.
    assert_eq!(slots[0].get("id").unwrap().text(), Some("os-2"));
    assert_eq!(slots[0].get("good").unwrap().text(), Some("untried"));
    assert_eq!(slots[0].get("display_name").unwrap().text(), Some("An OS"));
    assert_eq!(slots[0].get("intact"), Some(&Json::Bool(true)));
    assert_eq!(
        slots[1].get("flags"),
        Some(&Json::List(vec![
            Json::Text("running".into()),
            Json::Text("default".into())
        ]))
    );
    assert_eq!(
        board.slots.list(&mut out[..10]),
        Err(Status::BUFFER_TOO_SMALL)
    );
    let _ = &board.dir;
}

/// The security switch's view file in `dir`: `on` or `off`, generation 1.
fn view_file(dir: &std::path::Path, on: bool) -> std::path::PathBuf {
    let path = dir.join("security");
    std::fs::write(
        &path,
        format!("{}\ngeneration 1\n", if on { "on" } else { "off" }),
    )
    .unwrap();
    path
}

#[test]
fn a_board_that_requires_signing_and_trusts_no_key_installs_nothing() {
    let dir = TempDir::new("nokeys");
    let mut slots = SystemSlots::new(layout(&dir.0), Box::new(Told::default()), Vec::new());
    security::set_view_for_thread(Some(view_file(&dir.0, true)));
    assert_eq!(slots.install(10, false, false), Err(Status::NOT_PERMITTED));
    // Off, the default: it installs, and the slot shows as unverified.
    security::set_view_for_thread(Some(view_file(&dir.0, false)));
    assert!(slots.install(10, false, false).is_ok());
}

/// Signing off (the default): a bundle signed by another key, or not signed, installs, and
/// `list` says which; a trusted one names its owner. A signature that is there is still checked.
#[test]
fn without_require_signed_any_bundle_installs_and_the_list_says_how_it_was_signed() {
    let mut board = Board::new("os-1");
    let mut untrusted = Bundle::new("os-2");
    untrusted.signer = SigningKey::from_bytes(&[9; 32]);
    let (status, _, _) = board.stream(1, &untrusted.pack(), 4096);
    assert_eq!(status, Status::OK, "{}", board.state().error);
    let mut unsigned = Bundle::new("os-3");
    unsigned.sign = false;
    let (status, _, _) = board.stream(1, &unsigned.pack(), 4096);
    assert_eq!(status, Status::OK, "{}", board.state().error);
    let mut out = vec![0u8; 64 * 1024];
    let len = board.slots.list(&mut out).unwrap();
    let json = Json::parse(core::str::from_utf8(&out[..len]).unwrap()).unwrap();
    let Some(Json::List(slots)) = json.get("slots") else {
        panic!("no slots")
    };
    let signed: Vec<(Option<&str>, Option<&str>, Option<&str>)> = slots
        .iter()
        .map(|slot| {
            (
                slot.get("id").and_then(Json::text),
                slot.get("signed").and_then(Json::text),
                slot.get("signer").and_then(Json::text),
            )
        })
        .collect();
    assert_eq!(
        signed,
        [
            (Some("os-3"), Some("unsigned"), Some("")),
            (Some("os-2"), Some("untrusted"), Some("")),
            (Some("os-1"), Some("trusted"), Some("the build key")),
        ]
    );
    // A signature of the wrong length is no signature at all: the bundle is malformed.
    let mut torn = Bundle::new("os-4").pack();
    let at = torn.windows(8).position(|w| w == b"slot.sig").unwrap();
    // The size field of the slot.sig header (octal, at offset 124 of the header).
    torn[at + 124..at + 135].copy_from_slice(b"00000000077");
    let (status, _, _) = board.stream(1, &torn, 4096);
    assert_eq!(status, Status::INVALID_ARGUMENT);
}

/// The `signed` and `signature` words `list` gives a slot.
fn listed(board: &mut Board, id: &str) -> (String, String) {
    let mut out = vec![0u8; 64 * 1024];
    let len = board.slots.list(&mut out).unwrap();
    let json = Json::parse(core::str::from_utf8(&out[..len]).unwrap()).unwrap();
    let Some(Json::List(slots)) = json.get("slots") else {
        panic!("no slots")
    };
    let slot = slots
        .iter()
        .find(|slot| slot.get("id").and_then(Json::text) == Some(id))
        .unwrap_or_else(|| panic!("{id} is not listed"));
    let word = |key: &str| slot.get(key).and_then(Json::text).unwrap().to_string();
    (word("signed"), word("signature"))
}

/// What `info` says about the last install, as the JSON's `last_install` fields.
fn last_install(board: &mut Board) -> (String, String, String) {
    let mut out = vec![0u8; 4096];
    let len = board.slots.info(&mut out).unwrap();
    let json = Json::parse(core::str::from_utf8(&out[..len]).unwrap()).unwrap();
    let last = json.get("last_install").expect("last_install");
    let word = |key: &str| last.get(key).and_then(Json::text).unwrap().to_string();
    (word("slot"), word("signature"), word("reason"))
}

/// The signature policy on the install path, row by row. Each accepted bundle is checked for
/// what `list` and `info` say; each refused one for its status, the reason, and that nothing is
/// installed or left behind.
#[test]
fn the_policy_with_require_signed_off_accepts_every_row_and_says_how_it_was_checked() {
    let mut board = Board::new("os-1");
    let mut unsigned = Bundle::new("os-2");
    unsigned.sign = false;
    let (status, _, _) = board.stream(1, &unsigned.pack(), 4096);
    assert_eq!(status, Status::OK, "{}", board.state().error);
    assert_eq!(
        listed(&mut board, "os-2"),
        ("unsigned".into(), "not_checked_unsigned".into())
    );

    let (status, _, _) = board.stream(1, &Bundle::new("os-3").pack(), 4096);
    assert_eq!(status, Status::OK, "{}", board.state().error);
    assert_eq!(
        listed(&mut board, "os-3"),
        ("trusted".into(), "verified".into())
    );
    assert_eq!(
        last_install(&mut board),
        (
            "os-3".into(),
            "verified".into(),
            "signature verified with a key on this board's list (the build key)".into()
        )
    );

    let mut unknown = Bundle::new("os-4");
    unknown.signer = SigningKey::from_bytes(&[9; 32]);
    let (status, _, _) = board.stream(1, &unknown.pack(), 4096);
    assert_eq!(status, Status::OK, "{}", board.state().error);
    assert_eq!(
        listed(&mut board, "os-4"),
        ("untrusted".into(), "not_checked_unknown_key".into())
    );
    assert_eq!(last_install(&mut board).1, "not_checked_unknown_key");

    // A board with no key list at all: every key is unknown, so the same bundle is not checked.
    board.slots.keys.clear();
    let (status, _, _) = board.stream(1, &Bundle::new("os-5").pack(), 4096);
    assert_eq!(status, Status::OK, "{}", board.state().error);
    assert_eq!(
        listed(&mut board, "os-5"),
        ("untrusted".into(), "not_checked_unknown_key".into())
    );
}

#[test]
fn with_require_signed_off_a_bad_signature_under_a_known_key_is_refused_before_anything_is_installed()
 {
    let mut board = Board::new("os-1");
    let mut forged = Bundle::new("os-6");
    // Named as the build key, signed by another key: the record is corrupted or forged.
    forged.signer = SigningKey::from_bytes(&[9; 32]);
    forged.named = Some(key());
    let (status, _, _) = board.stream(1, &forged.pack(), 4096);
    assert_eq!(status, Status::NOT_PERMITTED, "{}", board.state().error);
    assert!(
        board.state().error.contains("does not verify"),
        "{}",
        board.state().error
    );
    assert!(!board.layout().installed("os-6"));
    assert!(!board.work_left());
    let (slot, signature, _) = last_install(&mut board);
    assert_eq!((slot.as_str(), signature.as_str()), ("", "refused"));
}

#[test]
fn with_require_signed_on_the_table_refuses_three_rows_and_takes_a_verified_bundle() {
    let mut board = Board::new("os-1");
    security::set_view_for_thread(Some(view_file(&board.dir.0, true)));
    let mut unsigned = Bundle::new("os-2");
    unsigned.sign = false;
    let (status, _, _) = board.stream(1, &unsigned.pack(), 4096);
    assert_eq!(status, Status::NOT_PERMITTED);
    assert!(
        board.state().error.contains("not signed"),
        "{}",
        board.state().error
    );

    let mut unknown = Bundle::new("os-3");
    unknown.signer = SigningKey::from_bytes(&[9; 32]);
    let (status, _, _) = board.stream(1, &unknown.pack(), 4096);
    assert_eq!(status, Status::NOT_PERMITTED);
    assert!(
        board.state().error.contains("does not trust"),
        "{}",
        board.state().error
    );

    let mut forged = Bundle::new("os-4");
    forged.signer = SigningKey::from_bytes(&[9; 32]);
    forged.named = Some(key());
    let (status, _, _) = board.stream(1, &forged.pack(), 4096);
    assert_eq!(status, Status::NOT_PERMITTED);
    assert!(
        board.state().error.contains("does not verify"),
        "{}",
        board.state().error
    );

    for id in ["os-2", "os-3", "os-4"] {
        assert!(!board.layout().installed(id), "{id} was installed");
    }
    assert!(!board.work_left());

    let (status, _, _) = board.stream(1, &Bundle::new("os-5").pack(), 4096);
    assert_eq!(status, Status::OK, "{}", board.state().error);
    assert_eq!(
        listed(&mut board, "os-5"),
        ("trusted".into(), "verified".into())
    );
}

#[test]
fn a_board_with_require_signed_on_and_no_key_list_refuses_every_bundle() {
    let mut board = Board::new("os-1");
    security::set_view_for_thread(Some(view_file(&board.dir.0, true)));
    board.slots.keys.clear();
    // Refused when the install starts: no job, nothing staged.
    assert_eq!(
        board.slots.install(10, false, false),
        Err(Status::NOT_PERMITTED)
    );
    assert!(!board.layout().installed("os-2"));
}

/// A malformed signature is refused in every row, whatever `require_signed` says: as an invalid
/// argument, with nothing installed. A bare 64-byte signature, the first version's form, is one.
#[test]
fn a_malformed_signature_is_refused_in_every_row() {
    let record = |f: fn(&mut Vec<u8>)| {
        let mut bytes = Bundle::new("os-2").pack_record();
        f(&mut bytes);
        bytes
    };
    let malformed: Vec<(&str, Bundle)> = vec![
        ("a bare 64-byte signature", {
            let mut b = Bundle::new("os-2");
            b.sig_file = Some(key().sign(b"x").to_bytes().to_vec());
            b
        }),
        ("a record of 107 bytes", {
            let mut b = Bundle::new("os-2");
            b.sig_file = Some(record(|r| r.truncate(107)));
            b
        }),
        ("a record with another magic", {
            let mut b = Bundle::new("os-2");
            b.sig_file = Some(record(|r| r[0] = b'X'));
            b
        }),
        ("a record with another algorithm", {
            let mut b = Bundle::new("os-2");
            b.sig_file = Some(record(|r| r[8] = 2));
            b
        }),
        ("a record with nonzero padding", {
            let mut b = Bundle::new("os-2");
            b.sig_file = Some(record(|r| r[10] = 1));
            b
        }),
        ("an empty slot.sig", {
            let mut b = Bundle::new("os-2");
            b.sig_file = Some(Vec::new());
            b
        }),
    ];
    for require in [false, true] {
        for (what, bundle) in &malformed {
            let mut board = Board::new("os-1");
            security::set_view_for_thread(Some(view_file(&board.dir.0, require)));
            let (status, _, _) = board.stream(1, &bundle.pack(), 4096);
            assert_eq!(
                status,
                Status::INVALID_ARGUMENT,
                "{what}, require_signed {require}"
            );
            assert!(
                !board.layout().installed("os-2"),
                "{what}, require_signed {require}"
            );
            assert!(!board.work_left(), "{what}, require_signed {require}");
        }
    }
}

/// A slot installed before the outcome words existed has only its `SIGNED` label; `list` reads
/// it as before, and `signature` says what that label means.
#[test]
fn a_slot_recorded_under_the_old_labels_is_listed_as_before() {
    let mut board = Board::new("os-1");
    let mut untrusted = Bundle::new("os-2");
    untrusted.signer = SigningKey::from_bytes(&[9; 32]);
    let (status, _, _) = board.stream(1, &untrusted.pack(), 4096);
    assert_eq!(status, Status::OK, "{}", board.state().error);
    let mut meta = board.meta("os-2");
    meta.set("SIGNATURE", "");
    meta.save(board.layout(), "os-2").unwrap();
    assert_eq!(
        listed(&mut board, "os-2"),
        ("untrusted".into(), "not_checked_unknown_key".into())
    );
}

/// With `require_signed` on, a slot whose signature did not verify is never set as the default
/// nor tried, even though it was installed while signing was off.
#[test]
fn a_slot_whose_signature_did_not_verify_is_not_tried_once_signing_is_required() {
    let mut board = Board::new("os-1");
    let mut unknown = Bundle::new("os-2");
    unknown.signer = SigningKey::from_bytes(&[9; 32]);
    let (status, _, _) = board.stream(1, &unknown.pack(), 4096);
    assert_eq!(status, Status::OK, "{}", board.state().error);
    security::set_view_for_thread(Some(view_file(&board.dir.0, true)));
    assert_eq!(
        board.slots.trial("os-2", false, false, false),
        Err(Status::NOT_PERMITTED)
    );
    assert!(
        board.state().error.contains("not signed by a key"),
        "{}",
        board.state().error
    );
    assert_eq!(
        board.slots.set_default("os-2", false),
        Err(Status::NOT_PERMITTED)
    );
}

/// No forced backup: the previous slot may be removed, and a board then runs one slot; a
/// trial that never confirms still falls back to it.
#[test]
fn the_previous_slot_may_be_removed_and_a_trial_still_falls_back() {
    let mut board = Board::new("os-1");
    board.install_ok("os-2");
    board.slots.trial("os-2", false, false, false).unwrap();
    board.boot_into("os-2");
    board.slots.confirm().unwrap();
    assert_eq!(board.state().previous, "os-1");
    board.slots.remove("os-1").unwrap();
    let state = board.state();
    assert_eq!(
        (state.default.as_str(), state.previous.as_str()),
        ("os-2", "")
    );
    assert_eq!(board.layout().slots(), ["os-2"]);
    assert!(!board.layout().boot_dir("os-1").exists());
    // The only slot left can't go.
    assert_eq!(board.slots.remove("os-2"), Err(Status::BUSY));
    board.install_ok("os-3");
    board.slots.trial("os-3", false, false, false).unwrap();
    board.told.take();
    // Never confirmed: the firmware boots the default again.
    board.boot_into("os-2");
    board.slots.confirm().unwrap();
    let state = board.state();
    assert_eq!(
        (
            state.default.as_str(),
            state.previous.as_str(),
            state.trial.as_str()
        ),
        ("os-2", "", "")
    );
    assert_eq!(state.state, "rolled-back");
    assert!(board.meta("os-3").failed());
    // The trial is out of the record too, so the loader never starts it again.
    assert_eq!(board.told.take(), ["clear trial"]);
}

/// With no setting, housekeeping keeps three slots (pinned ones not counted).
#[test]
fn housekeeping_keeps_three_by_default() {
    let mut board = Board::new("os-1");
    for id in ["os-2", "os-3", "os-4"] {
        board.install_ok(id);
        board.slots.trial(id, false, false, false).unwrap();
        board.boot_into(id);
        board.slots.confirm().unwrap();
    }
    assert_eq!(board.layout().slots(), ["os-2", "os-3", "os-4"]);
    let state = board.state();
    assert_eq!(
        (state.default.as_str(), state.previous.as_str()),
        ("os-4", "os-3")
    );
}

#[test]
fn keys_are_read_as_the_package_keys_are() {
    let text = format!(
        "# the build key\n\n{} release builds, 2026\n",
        hex(key().verifying_key().as_bytes())
    );
    let keys = crate::parse_keys(&text).unwrap();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].owner, "release builds, 2026");
    assert!(
        crate::parse_keys("abc owner")
            .unwrap_err()
            .contains("line 1")
    );
    let bare = hex(key().verifying_key().as_bytes());
    assert!(crate::parse_keys(&bare).unwrap_err().contains("owner"));
}

/// The same through a host, as the service runs it: the install as a long job, the trial's and
/// the reboot's restarts through the program the board file names.
/// A fake board tree under `dir` for the boot disk: the SD controller's device-tree node, one whole
/// disk under it with its partition 1 (the boot partition, holding `image`), and the boot chain's
/// source. Returns the partition's device node.
fn boot_tree(dir: &Path, image: &[u8]) -> PathBuf {
    let sys = dir.join("sys");
    let node = sys.join("firmware/devicetree/base/soc/mmc@7e202000");
    fs::create_dir_all(&node).unwrap();
    let controller = sys.join("devices/sd-host");
    fs::create_dir_all(&controller).unwrap();
    std::os::unix::fs::symlink(&node, controller.join("of_node")).unwrap();
    let disk = controller.join("mmcblk0");
    fs::create_dir_all(&disk).unwrap();
    fs::create_dir_all(sys.join("class/block")).unwrap();
    std::os::unix::fs::symlink(&disk, sys.join("class/block/mmcblk0")).unwrap();
    let part = disk.join("mmcblk0p1");
    fs::create_dir_all(&part).unwrap();
    fs::write(part.join("partition"), "1\n").unwrap();
    std::os::unix::fs::symlink(&part, sys.join("class/block/mmcblk0p1")).unwrap();
    let chosen = dir.join("chosen");
    fs::create_dir_all(&chosen).unwrap();
    fs::write(chosen.join("lemnos,boot-source"), b"/soc/mmc@7e202000\0").unwrap();
    let dev = dir.join("dev");
    fs::create_dir_all(&dev).unwrap();
    fs::write(dev.join("mmcblk0p1"), image).unwrap();
    dev.join("mmcblk0p1")
}

/// The boot roots the driver reads while a test holds them: restored when dropped.
struct TestRoots;

impl TestRoots {
    fn set(dir: &Path) -> Self {
        *crate::TEST_ROOTS.lock().unwrap() = Some(crate::BootRoots {
            chosen: dir.join("chosen"),
            sys: dir.join("sys"),
            dev: dir.join("dev"),
        });
        Self
    }
}

impl Drop for TestRoots {
    fn drop(&mut self) {
        *crate::TEST_ROOTS.lock().unwrap() = None;
    }
}

#[test]
fn through_a_host_an_install_is_a_job_and_a_trial_restarts_with_tryboot() {
    use lemnos_core::{Encode, MethodId, Writer};
    use lemnos_host::board::parse;
    use lemnos_host::{Host, JobEvent, JobUpdate, Options, entry};
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new("host");
    let layout = layout(&dir.0);
    fs::create_dir_all(layout.state_dir()).unwrap();
    fs::create_dir_all(layout.boot.join("slots")).unwrap();
    fs::write(layout.marker("booted"), "os-1\n").unwrap();
    let node = boot_tree(&dir.0, &crate::bay_fixture::tryboot_image("os-1"));
    let _roots = TestRoots::set(&dir.0);
    let keys = dir.0.join("keys");
    fs::write(
        &keys,
        format!("{} the build key\n", hex(key().verifying_key().as_bytes())),
    )
    .unwrap();
    // Stands in for systemctl: writes down how it was run.
    let calls = dir.0.join("calls");
    let systemctl = dir.0.join("systemctl");
    fs::write(
        &systemctl,
        format!("#!/bin/sh\necho \"$@\" >> '{}'\n", calls.display()),
    )
    .unwrap();
    fs::set_permissions(&systemctl, fs::Permissions::from_mode(0o755)).unwrap();
    let text = format!(
        r#"
format = "lemnos.board"
schema_version = 1
[board]
id = "slots-test"

# A loaded test machine's disk can take longer than the default 2 s to flush.
[host]
call_deadline_ms = 30000

# One trust root per board: the host gives these keys to the slots driver.
[security]
record = "{record}"
keys = "{keys}"

[[devices]]
id = "slots"
driver = "system-slots"
config = {{ data = "{data}", boot = "{boot}", boot_sources = ["/soc/mmc@7e202000=/soc/mmc@7e202000"], systemctl = "{ctl}", partuuid = "abcd-02", boot_args = "board", reserve_mb = 0 }}

[[devices]]
id = "reboot"
driver = "system-reboot"
config = {{ systemctl = "{ctl}" }}
"#,
        data = layout.data.display(),
        boot = layout.boot.display(),
        keys = keys.display(),
        record = dir.0.join("security.rec").display(),
        ctl = systemctl.display(),
    );
    let sim = lemnos_sim::SimBoard::new();
    let clock = sim.clock();
    let host = Host::start(
        &parse(&text).unwrap(),
        &[entry::<SystemSlots>(), entry::<crate::SystemReboot>()],
        Box::new(sim),
        Arc::new(clock),
        Options::default(),
    )
    .unwrap();
    let slots = host.device("slots").unwrap().index;
    let method = |method| MethodId {
        contract: 0,
        method,
    };
    let bundle = Bundle::new("os-2").pack();
    let mut args = [0u8; 64];
    let len = {
        let mut w = Writer::new(&mut args);
        (bundle.len() as u64).encode(&mut w).unwrap();
        false.encode(&mut w).unwrap();
        false.encode(&mut w).unwrap();
        w.len()
    };
    let caller = CallerId(5);
    let job = host
        .start_job(caller, slots, method(2), &args[..len])
        .unwrap();
    let mut offset = 0u64;
    for chunk in bundle.chunks(64 * 1024) {
        let mut args = vec![0u8; chunk.len() + 16];
        let len = {
            let mut w = Writer::new(&mut args);
            offset.encode(&mut w).unwrap();
            chunk.encode(&mut w).unwrap();
            w.len()
        };
        host.call_as(caller, slots, method(3), &args[..len], &mut [])
            .unwrap();
        offset += chunk.len() as u64;
    }
    let mut updates: Vec<JobUpdate> = Vec::new();
    host.take_job_updates(&mut updates);
    let end = updates
        .iter()
        .find_map(|u| match &u.event {
            JobEvent::Finished { status, result } if u.job == job => {
                Some((*status, result.clone()))
            }
            _ => None,
        })
        .expect("the install ended");
    assert_eq!(end, (Status::OK, b"os-2".to_vec()));
    assert!(updates.iter().all(|u| u.owner == caller));

    // `info` through the host, as a session that started nothing reads it.
    let info = |host: &Host| {
        let mut out = vec![0u8; 4096];
        let n = host
            .call(slots, method(14), &4096u32.to_le_bytes(), &mut out)
            .unwrap();
        let json = Json::parse(core::str::from_utf8(&out[..n]).unwrap()).unwrap();
        let a = json.get("last_attempt").unwrap();
        let text = |key: &str| a.get(key).and_then(Json::text).unwrap().to_string();
        (
            text("state"),
            text("slot"),
            text("reason"),
            a.get("received").and_then(Json::u64),
        )
    };
    assert_eq!(
        info(&host),
        (
            "installed".into(),
            "os-2".into(),
            String::new(),
            Some(bundle.len() as u64)
        )
    );
    // A starter whose connection ends mid-install: the host cancels its job and tells the driver
    // it went, and the record says why. (Stands in for nothing here: the host is the real one;
    // what it does not share with lemnosd is the socket, whose end calls the same `caller_gone`.)
    let gone = CallerId(6);
    let bundle3 = Bundle::new("os-3").pack();
    let len = {
        let mut w = Writer::new(&mut args);
        (bundle3.len() as u64).encode(&mut w).unwrap();
        false.encode(&mut w).unwrap();
        false.encode(&mut w).unwrap();
        w.len()
    };
    host.start_job(gone, slots, method(2), &args[..len])
        .unwrap();
    let mut piece = vec![0u8; 3016];
    let len = {
        let mut w = Writer::new(&mut piece);
        0u64.encode(&mut w).unwrap();
        bundle3[..3000].encode(&mut w).unwrap();
        w.len()
    };
    host.call_as(gone, slots, method(3), &piece[..len], &mut [])
        .unwrap();
    assert_eq!(info(&host).0, "running");
    host.caller_gone(gone);
    assert_eq!(
        info(&host),
        (
            "cancelled".into(),
            "os-3".into(),
            crate::attempt::GONE.into(),
            Some(3000)
        )
    );

    let mut args = [0u8; 32];
    let len = {
        let mut w = Writer::new(&mut args);
        "os-2".encode(&mut w).unwrap();
        for flag in [false, false, false] {
            flag.encode(&mut w).unwrap();
        }
        w.len()
    };
    host.call(slots, method(5), &args[..len], &mut []).unwrap();
    assert_eq!(
        lemnos_slot_store::boot_raw::BootRaw::pointer(
            &mut lemnos_slot_store::boot_raw::Boot::open(&node).unwrap(),
            lemnos_slot_store::boot_raw::Pointer::Trial,
        )
        .unwrap()
        .as_deref(),
        Some("os-2")
    );
    let reboot = host.device("reboot").unwrap().index;
    host.call(reboot, method(1), &[], &mut []).unwrap();
    assert_eq!(fs::read_to_string(&calls).unwrap(), "reboot\nreboot\n");
}

/// A board whose record is the U-Boot environment, as the image builder leaves it: os-1 in bay 0 on the
/// boot partition (`boot.img`), and an environment (on a node of its own, the partition U-Boot keeps
/// it on) whose default is os-1 in bay 0. Returns the board, the boot partition's node, the
/// environment's node and its geometry.
fn uboot_board() -> (
    Board,
    PathBuf,
    PathBuf,
    lemnos_slot_store::uboot_env::Geometry,
) {
    let mut board = Board::new("os-1");
    let boot_node = use_uboot(&mut board);
    let node = board.layout().boot.join("uboot-env.img");
    let geometry = lemnos_slot_store::uboot_env::Geometry::new(1024, 0, 4096).unwrap();
    (board, boot_node, node, geometry)
}

/// With the `uboot` record, an install writes the slot's payloads and its composed loader
/// configuration into a bay (raw, on the boot partition's image), and a trial arms the environment.
#[test]
fn with_the_uboot_record_an_install_writes_the_slots_extlinux_and_a_trial_arms_it() {
    let (mut board, boot_node, node, geometry) = uboot_board();
    let boot = board.layout().boot.clone();
    board.install_ok("os-2");
    // os-2 took the bay the environment names nothing for, and os-1's was not touched.
    let (dir, lengths) = board.slots.record.slot_bay("os-2").unwrap();
    assert_eq!(dir, "slots/bay1");
    assert_eq!(lengths.len(), 5, "{lengths:?}");
    assert_eq!(board.slots.record.slot_bay("os-1").unwrap().0, "slots/bay0");
    board.slots.trial("os-2", false, false, false).unwrap();
    // The bay's composed extlinux file, read raw from the image: paths relative to the bay, and the
    // slot's own command line.
    let mut file = fs::File::open(&boot_node).unwrap();
    let conf =
        lemnos_slot_store::bootcheck::fat_read(&mut file, "slots/bay1/extlinux/extlinux.conf")
            .unwrap()
            .expect("the bay's extlinux file");
    let conf = String::from_utf8_lossy(&conf);
    let conf = conf.trim_end_matches('\0').trim_end_matches(' ');
    assert!(conf.contains("    KERNEL ../kernel8.img\n"), "{conf}");
    assert!(conf.contains("    INITRD ../initramfs.cpio\n"), "{conf}");
    assert!(
        conf.contains(&format!(
            "    APPEND console=tty1 rootwait board.slot=os-2 board.roothash={ROOT_HASH} board.src./soc/mmc@7e202000=/soc/mmc@7e202000 panic=5\n"
        )),
        "{conf}"
    );
    let mut dev = lemnos_slot_store::uboot_env::FileEnv::open(&node).unwrap();
    let (_, vars) = lemnos_slot_store::uboot_env::load(&mut dev, &geometry)
        .unwrap()
        .unwrap();
    for want in [
        ("trial", "os-2"),
        ("trial_bay", "bay1"),
        ("trial_attempts", "3"),
    ] {
        assert!(
            vars.contains(&(want.0.to_string(), want.1.to_string())),
            "{want:?} in {vars:?}"
        );
    }
    assert!(
        !boot.join(crate::uboot::RECORD_FILE).exists(),
        "no record file on the boot partition"
    );
    board.boot_into("os-2");
    board.slots.confirm().unwrap();
    let mut dev = lemnos_slot_store::uboot_env::FileEnv::open(&node).unwrap();
    let (_, vars) = lemnos_slot_store::uboot_env::load(&mut dev, &geometry)
        .unwrap()
        .unwrap();
    for want in [
        ("default", "os-2"),
        ("default_bay", "bay1"),
        ("previous_bay", "bay0"),
    ] {
        assert!(
            vars.contains(&(want.0.to_string(), want.1.to_string())),
            "{want:?} in {vars:?}"
        );
    }
    // A confirm rewrites nothing on the boot partition's files.
    assert!(!boot.join("extlinux/extlinux.conf").exists());
}

impl Board {
    fn list(&self) -> String {
        self.slots.list_json()
    }

    fn incoming(&self) -> PathBuf {
        self.layout().images().join(crate::install::INCOMING)
    }

    /// Ticks an install from a file to its end: its status, result and the progress seen.
    fn run_file_job(&mut self, job: JobId) -> (Status, Vec<u8>, Vec<u64>) {
        let mut seen = Vec::new();
        for _ in 0..10_000 {
            let next = self.slots.tick(0);
            match self.poll(job) {
                Some(JobState::Running { done, total }) => {
                    assert!(total > 0);
                    seen.push(done);
                }
                _ => {
                    let mut out = [0u8; 128];
                    let mut reply = Reply::new(&mut out);
                    let JobState::Finished(status) = self.slots.job(job, &mut reply) else {
                        panic!("the job is still running")
                    };
                    let len = reply.len();
                    return (status, out[..len].to_vec(), seen);
                }
            }
            assert_eq!(
                next,
                Some(0),
                "an install from a file asks for the next tick"
            );
        }
        panic!("the install never ended")
    }
}

/// A bundle big enough to take several ticks.
fn big_bundle(id: &str) -> Vec<u8> {
    let mut bundle = Bundle::new(id);
    bundle.data_blocks = 800;
    bundle.image = image_of(id, 800);
    assert!(bundle.image.len() > 3 * crate::install::READ_EVERY_TICK);
    bundle.pack()
}

#[test]
fn a_file_in_incoming_installs_as_a_job_and_is_deleted_after() {
    let mut board = Board::new("os-1");
    let incoming = board.incoming();
    let bundle = big_bundle("os-2");
    fs::write(incoming.join("os-2.slot.tar"), &bundle).unwrap();
    board.slots.caller(CallerId(3));
    let job = board
        .slots
        .install_file("os-2.slot.tar", false, false)
        .unwrap();
    // Only one install at a time, and nobody writes into a file's install.
    assert_eq!(board.slots.install(10, false, false), Err(Status::BUSY));
    assert_eq!(board.slots.accept(10), Err(Status::BUSY));
    assert_eq!(board.slots.write(0, b"x"), Err(Status::NOT_PERMITTED));
    let (status, result, seen) = board.run_file_job(job);
    assert_eq!(status, Status::OK, "{}", board.state().error);
    assert_eq!(result, b"os-2");
    assert!(seen.len() >= 3, "{seen:?}");
    assert!(seen.windows(2).all(|w| w[0] <= w[1]));
    assert!(board.layout().installed("os-2"));
    assert!(!incoming.join("os-2.slot.tar").exists());
    assert!(!board.work_left());
}

#[test]
fn a_file_that_fails_or_is_cancelled_is_deleted_too() {
    let mut board = Board::new("os-1");
    let incoming = board.incoming();
    // Fails: its image isn't what its manifest says.
    let mut wrong = Bundle::new("os-2");
    wrong.manifest_image_sha = Some("00".repeat(32));
    fs::write(incoming.join("bad"), wrong.pack()).unwrap();
    let job = board.slots.install_file("bad", false, false).unwrap();
    let (status, _, _) = board.run_file_job(job);
    assert_eq!(status, Status::INVALID_ARGUMENT);
    assert!(!incoming.join("bad").exists());
    // Cut short: the file ends before its tar does.
    let bundle = big_bundle("os-3");
    fs::write(incoming.join("short"), &bundle[..bundle.len() / 2]).unwrap();
    let job = board.slots.install_file("short", false, false).unwrap();
    let (status, _, _) = board.run_file_job(job);
    assert_eq!(status, Status::INVALID_ARGUMENT);
    assert!(!incoming.join("short").exists());
    // Cancelled half-way.
    fs::write(incoming.join("cancel"), &bundle).unwrap();
    let job = board.slots.install_file("cancel", false, false).unwrap();
    board.slots.tick(0);
    board.slots.job_cancel(job);
    let mut out = [0u8; 8];
    assert_eq!(
        board.slots.job(job, &mut Reply::new(&mut out)),
        JobState::Finished(Status::CANCELLED)
    );
    assert!(!incoming.join("cancel").exists());
    assert!(!board.layout().installed("os-3"));
    assert!(!board.work_left());
}

#[test]
fn a_file_removed_mid_install_still_installs_from_what_was_opened() {
    let mut board = Board::new("os-1");
    let incoming = board.incoming();
    fs::write(incoming.join("b"), big_bundle("os-2")).unwrap();
    let job = board.slots.install_file("b", false, false).unwrap();
    board.slots.tick(0);
    // Unlinked under the driver: the open file still reads whole.
    fs::remove_file(incoming.join("b")).unwrap();
    let (status, result, _) = board.run_file_job(job);
    assert_eq!(
        (status, result.as_slice()),
        (Status::OK, b"os-2".as_slice())
    );
    assert_eq!(fs::read_dir(&incoming).unwrap().count(), 0);
}

#[test]
fn only_a_plain_file_named_by_one_component_installs() {
    use std::os::unix::fs::symlink;
    let mut board = Board::new("os-1");
    let incoming = board.incoming();
    for name in ["", ".", "..", "../os-1.json", "a/b", "a b", "x\0"] {
        assert_eq!(
            board.slots.install_file(name, false, false),
            Err(Status::INVALID_ARGUMENT),
            "{name:?}"
        );
    }
    assert_eq!(
        board.slots.install_file("missing", false, false),
        Err(Status::NOT_FOUND)
    );
    // A link, even to a good bundle, is refused; so is a directory.
    let outside = board.dir.0.join("outside.tar");
    fs::write(&outside, Bundle::new("os-2").pack()).unwrap();
    symlink(&outside, incoming.join("link")).unwrap();
    assert_eq!(
        board.slots.install_file("link", false, false),
        Err(Status::INVALID_ARGUMENT)
    );
    fs::create_dir(incoming.join("dir")).unwrap();
    assert_eq!(
        board.slots.install_file("dir", false, false),
        Err(Status::INVALID_ARGUMENT)
    );
    assert!(outside.exists());
    assert!(!board.layout().installed("os-2"));
}

#[test]
fn accept_says_whether_a_bundle_fits_with_the_reserve() {
    let mut board = Board::new("os-1");
    let free = lemnos_linux_sys::statfs::free_bytes(&board.layout().images()).unwrap();
    board.slots.reserve = 0;
    assert_eq!(board.slots.accept(1 << 20), Ok(()));
    // Other writers move the free space on a shared disk, so the refusals ask far past it.
    assert_eq!(
        board.slots.accept(free.saturating_mul(4)),
        Err(crate::install::NO_SPACE)
    );
    board.slots.reserve = u64::MAX / 2;
    assert_eq!(board.slots.accept(1 << 20), Err(crate::install::NO_SPACE));
    assert!(
        board.state().error.contains("no room"),
        "{}",
        board.state().error
    );
    // install_file holds to the same rule, and deletes the file it refused.
    fs::write(board.incoming().join("b"), Bundle::new("os-2").pack()).unwrap();
    assert_eq!(
        board.slots.install_file("b", false, false),
        Err(crate::install::NO_SPACE)
    );
    assert!(!board.incoming().join("b").exists());
}

#[test]
fn incoming_is_emptied_and_group_writable_when_the_driver_starts() {
    use std::os::unix::fs::PermissionsExt;
    let board = Board::new("os-1");
    let incoming = board.incoming();
    fs::write(incoming.join("left"), "x").unwrap();
    fs::create_dir(incoming.join("partial.d")).unwrap();
    let again = SystemSlots::new(
        board.layout().clone(),
        Box::new(Told::default()),
        Vec::new(),
    );
    assert_eq!(fs::read_dir(&incoming).unwrap().count(), 0);
    let mode = fs::metadata(&incoming).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o770);
    assert_eq!(
        crate::install::group_id("root:x:0:\nlemnos:x:975:orion\n", "lemnos"),
        Some(975)
    );
    assert_eq!(crate::install::group_id("root:x:0:\n", "lemnos"), None);
    drop(again);
}

#[test]
fn a_slot_built_for_this_board_installs_and_one_for_another_is_refused() {
    let mut board = Board::new("os-1");
    // Match.
    board.install_ok("os-2");
    assert!(
        board
            .slots
            .list_json()
            .contains("\"product\":\"an-os\",\"board\":\"slots-test\"")
    );
    // Mismatch: the job ends NOT_SUPPORTED, with the reason, and leaves nothing.
    let mut other = Bundle::new("os-3");
    other.board = "another-board".into();
    let (status, _, _) = board.stream(1, &other.pack(), 4096);
    assert_eq!(status, Status::NOT_SUPPORTED);
    assert!(
        board
            .state()
            .error
            .contains("built for board another-board, and this is board slots-test"),
        "{}",
        board.state().error
    );
    assert!(!board.layout().installed("os-3"));
    assert!(!board.work_left());
    // No board in the manifest: refused as malformed.
    let missing = Bundle::new("os-4");
    let manifest = missing.manifest().replace("\"board\":\"slots-test\",", "");
    let (status, _, _) = board.stream(1, &missing.pack_with(&manifest), 4096);
    assert_eq!(status, Status::INVALID_ARGUMENT);
    assert_eq!(board.state().error, "slot.json has no board");
    assert!(!board.layout().installed("os-4"));
    // A schema 2 manifest, with its family key: refused with a reason that says so.
    let old = Bundle::new("os-5");
    let manifest = old.manifest().replace("\"schema\":3", "\"schema\":2");
    let (status, _, _) = board.stream(1, &old.pack_with(&manifest), 4096);
    assert_eq!(status, Status::INVALID_ARGUMENT);
    assert!(
        board.state().error.contains("schema 2"),
        "{}",
        board.state().error
    );
}

#[test]
fn a_slot_on_disk_for_another_board_is_listed_but_never_booted() {
    let mut board = Board::new("os-1");
    board.install_ok("os-2");
    // As if the disk came from another board: its manifest names that one.
    let path = board.layout().manifest("os-2");
    let text = fs::read_to_string(&path)
        .unwrap()
        .replace("slots-test", "another-board");
    fs::write(&path, text).unwrap();
    assert!(board.slots.list_json().contains("\"id\":\"os-2\""));
    assert_eq!(
        board.slots.trial("os-2", false, false, true),
        Err(Status::NOT_SUPPORTED)
    );
    assert!(
        board.state().error.contains("another-board"),
        "{}",
        board.state().error
    );
    assert_eq!(
        board.slots.set_default("os-2", true),
        Err(Status::NOT_SUPPORTED)
    );
    assert!(board.told.take().is_empty());
    // One whose manifest names no board at all is refused the same way.
    let text = fs::read_to_string(&path)
        .unwrap()
        .replace("\"board\":\"another-board\",", "");
    fs::write(&path, text).unwrap();
    assert_eq!(
        board.slots.trial("os-2", false, false, true),
        Err(Status::NOT_SUPPORTED)
    );
    assert!(
        board.state().error.contains("names no board"),
        "{}",
        board.state().error
    );
}

/// The hash tree as `veritysetup format --no-superblock` writes it and dm-verity reads it,
/// built here only to check the conventions the driver and `lemnos-slot-init` rely on: its size
/// ([`crate::manifest::tree_bytes`]), its levels stored top level first from the table's hash
/// start, and the root hash. Returns the hash area and the root.
fn verity_tree(data: &[u8], salt: &[u8]) -> (Vec<u8>, String) {
    let digest = |block: &[u8]| -> Vec<u8> {
        let mut h = Sha256::new();
        h.update(salt);
        h.update(block);
        h.finalize().to_vec()
    };
    let mut hashes: Vec<Vec<u8>> = data.chunks(BLOCK as usize).map(digest).collect();
    let mut levels: Vec<Vec<u8>> = Vec::new();
    while hashes.len() > 1 {
        let mut level = Vec::new();
        for group in hashes.chunks(128) {
            let mut block = group.concat();
            block.resize(BLOCK as usize, 0);
            level.push(block);
        }
        hashes = level.iter().map(|b| digest(b)).collect();
        levels.push(level.concat());
    }
    levels.reverse();
    (levels.concat(), hex(&hashes[0]))
}

#[test]
fn the_verity_conventions_match_veritysetup() {
    // veritysetup 2.8.7, `format --no-superblock --data-block-size=4096 --hash-block-size=4096
    // --hash=sha256 --salt=000102…1f --data-blocks=N` over N blocks, block i filled with the byte
    // i mod 251: N, the hash file's length, and the root hash it printed.
    let salt: Vec<u8> = (0..32).collect();
    for (blocks, tree, root) in [
        (
            1u64,
            0u64,
            "4ce3ecf32c133bf6321901b6092219474b6ac91a19d0304621d629e6bb9987dc",
        ),
        (
            2,
            4096,
            "bd780675f50010ed023fd1f07e98f029a6aaf25a8f6ee33ec94b4b4e847e4b2e",
        ),
        (
            128,
            4096,
            "acb37719c99f3ac554ecbbccb8910616915a08e5a67bd3b9778694c60e673da4",
        ),
        (
            129,
            12_288,
            "6093a2333523050b628581510028976d3d3e9c62458642a83727e6df641397a4",
        ),
        (
            16_384,
            528_384,
            "234a6801a6840c2defdf3a7c08060cf9874f5773c6360f4172b08ae891a611d5",
        ),
        (
            16_385,
            540_672,
            "f1acccee31dcae56f612342ed838ba4c1f375de2d0341ae0cddecd0b28ff8732",
        ),
        (
            38_400,
            1_245_184,
            "a73f8f9623fc3e3298871b6ef9de8ae4ba8867f23f5d8a92b87d97b0b5ddfa18",
        ),
    ] {
        let data: Vec<u8> = (0..blocks)
            .flat_map(|i| std::iter::repeat_n((i % 251) as u8, BLOCK as usize))
            .collect();
        let (area, got) = verity_tree(&data, &salt);
        assert_eq!(
            crate::manifest::tree_bytes(blocks),
            tree,
            "{blocks} blocks: tree size"
        );
        assert_eq!(area.len() as u64, tree, "{blocks} blocks: built tree");
        assert_eq!(got, root, "{blocks} blocks: root hash");
        // The image is the data and the tree after it; the table starts the hash area there,
        // and its first block is the top level, which hashes to the root.
        let manifest = crate::manifest::Verity {
            root_hash: root.into(),
            salt: hex(&salt),
            data_blocks: blocks,
            hash_offset: blocks * BLOCK,
        };
        let table = manifest.table("7:0");
        assert!(
            table.contains(&format!(" {blocks} {blocks} sha256 {root} ")),
            "{table}"
        );
        if tree > 0 {
            let mut h = Sha256::new();
            h.update(&salt);
            h.update(&area[..BLOCK as usize]);
            assert_eq!(hex(&h.finalize()), root, "{blocks} blocks: top level first");
        }
    }
}

/// The volume names on a NAND board's fake UBI device.
fn volumes(fake: &Fake) -> Vec<String> {
    fake.ubi.volumes().into_iter().map(|v| v.name).collect()
}

#[test]
fn on_nand_an_install_writes_its_image_into_a_volume_and_renames_it_into_place() {
    let (mut board, fake) = Board::on_nand("os-1", 64 << 20);
    board.install_ok("os-2");
    let image = Bundle::new("os-2").image;
    assert_eq!(fake.ubi.read(&image_volume("os-2")).unwrap(), image);
    assert_eq!(volumes(&fake), [image_volume("os-1"), image_volume("os-2")]);
    // No image file on the data side, and the slot counts as installed from its volume.
    assert!(!board.layout().image("os-2").exists());
    assert!(board.layout().installed("os-2"));
    assert_eq!(board.layout().image_size("os-2"), Some(image.len() as u64));
    // Its command line is the bundle's: the data volume is not named on it (slot-init finds it).
    let kept = crate::boot::kept(board.layout(), "os-2").unwrap();
    let cmdline = kept.iter().find(|(_, n)| n == "cmdline.txt").unwrap();
    let cmdline = fs::read_to_string(board.layout().bootfiles().join(&cmdline.0)).unwrap();
    assert!(!cmdline.contains("board.data="), "{cmdline}");
    let list = board.slots.list_json();
    assert!(
        list.contains(&format!(
            "\"size\":{},\"verity\":true,\"intact\":true",
            image.len()
        )),
        "{list}"
    );
    assert!(!board.work_left());
}

#[test]
fn on_nand_a_refused_or_cancelled_install_leaves_no_work_volume() {
    let (mut board, fake) = Board::on_nand("os-1", 64 << 20);
    let mut wrong = Bundle::new("os-2");
    wrong.manifest_image_sha = Some(sha(b"something else"));
    assert_eq!(
        board.stream(1, &wrong.pack(), 4096).0,
        Status::INVALID_ARGUMENT
    );
    assert_eq!(volumes(&fake), [image_volume("os-1")]);
    let bundle = Bundle::new("os-2").pack();
    board.slots.caller(CallerId(1));
    let job = board
        .slots
        .install(bundle.len() as u64, false, false)
        .unwrap();
    board
        .slots
        .write(0, &bundle[..bundle.len() - 2048])
        .unwrap();
    assert_eq!(volumes(&fake).len(), 2, "the work volume while it comes");
    board.slots.job_cancel(job);
    assert_eq!(volumes(&fake), [image_volume("os-1")]);
    assert!(!board.layout().installed("os-2"));
}

#[test]
fn on_nand_a_restart_mid_install_removes_the_work_volume() {
    let (mut board, fake) = Board::on_nand("os-1", 64 << 20);
    let bundle = Bundle::new("os-2").pack();
    board.slots.caller(CallerId(1));
    board
        .slots
        .install(bundle.len() as u64, false, false)
        .unwrap();
    board
        .slots
        .write(0, &bundle[..bundle.len() - 2048])
        .unwrap();
    assert!(volumes(&fake).iter().any(|v| v.starts_with("slot-new-")));
    let again = SystemSlots::with_volumes(
        board.layout().clone(),
        Box::new(Told::default()),
        Vec::new(),
        Some(fake.clone()),
    );
    assert_eq!(volumes(&fake), [image_volume("os-1")]);
    assert!(State::load(&again.layout).error.contains("interrupted"));
}

#[test]
fn on_nand_remove_takes_the_volume_and_gives_its_room_back() {
    let (mut board, fake) = Board::on_nand("os-1", 64 << 20);
    let before = fake.ubi.free().unwrap();
    board.install_ok("os-2");
    assert!(fake.ubi.free().unwrap() < before);
    board.slots.remove("os-2").unwrap();
    assert_eq!(fake.ubi.free().unwrap(), before);
    assert_eq!(volumes(&fake), [image_volume("os-1")]);
    assert!(!board.layout().installed("os-2"));
}

#[test]
fn on_nand_room_is_the_ubi_devices() {
    let (mut board, _fake) = Board::on_nand("os-1", 1 << 20);
    // 1 MiB of erase blocks, the first slot's volume in one of them.
    assert_eq!(board.slots.accept(512 << 10), Ok(()));
    assert_eq!(board.slots.accept(2 << 20), Err(crate::install::NO_SPACE));
    assert!(
        board.state().error.contains("no room"),
        "{}",
        board.state().error
    );
}

#[test]
fn on_nand_the_uboot_record_is_a_volume_replaced_whole() {
    let dir = TempDir::new("nand-record");
    let fake = Arc::new(Fake::new(nand(&dir.0), 1 << 20));
    let mut record = crate::uboot::Uboot {
        boot: dir.0.join("boot"),
        restarter: crate::boot::Restarter {
            check: None,
            systemctl: "/nonexistent/systemctl".into(),
        },
        volume: Some((fake.ubi.clone(), fake.clone() as Arc<dyn UbiWrite>)),
    };
    record.set_default("os-1").unwrap();
    record.set_trial("os-2", 3).unwrap();
    assert_eq!(
        fake.ubi.read("boot-record").unwrap(),
        b"default=os-1\ntrial=os-2\ntrial_attempts=3\n"
    );
    assert_eq!(volumes(&fake), ["boot-record"]);
    assert!(!dir.0.join("boot/boot-record.env").exists());
    // A replacement that can't finish leaves the record as it was.
    fake.refuse.lock().unwrap().push("rename");
    assert!(record.set_default("os-2").is_err());
    assert_eq!(record.read().default, "os-1");
}

/// A board view the test sets: each device's id, origin, whether it is bound, and its health.
#[derive(Default)]
struct View(Vec<(&'static str, Origin, bool, Health)>);

impl BoardView for View {
    fn each(&self, f: &mut dyn FnMut(BoardDevice<'_>)) {
        for (id, origin, bound, health) in &self.0 {
            f(BoardDevice {
                id,
                origin: *origin,
                bound: *bound,
                health: *health,
            });
        }
    }
}

const GOOD: Health = Health::Ok;

/// A board where `a` is the default and `b` is installed, with `b` on trial.
fn trial_board() -> Board {
    let mut board = Board::new("a");
    board.install_ok("b");
    board.trial_slot("b");
    board
}

fn resolved(board: &Board) -> Resolved {
    Resolved::load(board.layout()).expect("a resolution")
}

#[test]
fn a_healthy_trial_confirms_by_itself_and_is_recorded() {
    let mut board = trial_board();
    board.baseline("a", "dev builtin\n");
    assert_eq!(board.next_boot(1), "b");
    let view = View(vec![("dev", Origin::BuiltIn, true, GOOD)]);
    assert!(board.settle_at(0, &view).is_some(), "waits for the uptime");
    assert_eq!(board.state().trial, "b", "not before the uptime");
    assert_eq!(board.settle_at(31, &view), None);
    let state = board.state();
    assert_eq!((state.default.as_str(), state.trial.as_str()), ("b", ""));
    let record = resolved(&board);
    assert_eq!(
        (
            record.slot.as_str(),
            record.outcome.as_str(),
            record.by.as_str()
        ),
        ("b", "confirmed", "auto")
    );
    assert_eq!(record.caller, None);
    assert!(board.told.take().contains(&"clear trial".to_string()));
}

#[test]
fn a_built_in_device_that_worked_before_and_is_failed_now_blocks_the_confirm_and_is_named() {
    let mut board = trial_board();
    board.baseline("a", "dev builtin\n");
    board.next_boot(1);
    let view = View(vec![(
        "dev",
        Origin::BuiltIn,
        true,
        Health::Failed("broken"),
    )]);
    assert!(board.settle_at(31, &view).is_some());
    assert_eq!(board.state().trial, "b", "no confirm");
    // At the deadline the reason names the device, and the board restarts into the default.
    assert_eq!(board.settle_at(600, &view), None);
    let record = Log::load(board.layout()).expect("the trial's log");
    assert!(record.reason.contains("dev"), "{}", record.reason);
    assert!(record.reason.contains("broken"), "{}", record.reason);
    assert!(board.told.take().contains(&"restart".to_string()));
}

#[test]
fn a_built_in_device_that_didnt_bind_here_blocks_the_confirm() {
    let mut board = trial_board();
    board.baseline("a", "dev builtin\n");
    board.next_boot(1);
    let view = View(vec![(
        "dev",
        Origin::BuiltIn,
        false,
        Health::Failed("retrying"),
    )]);
    board.settle_at(31, &view);
    assert_eq!(board.state().trial, "b");
    assert!(
        Log::load(board.layout())
            .unwrap()
            .reason
            .contains("didn't bind")
    );
}

#[test]
fn a_built_in_device_that_worked_before_and_is_absent_now_blocks_the_confirm() {
    let mut board = trial_board();
    board.baseline("a", "dev builtin\n");
    board.next_boot(1);
    board.settle_at(31, &View(vec![]));
    assert_eq!(board.state().trial, "b", "no confirm");
    assert!(Log::load(board.layout()).unwrap().reason.contains("dev"));
}

#[test]
fn a_hotplug_device_that_worked_before_and_is_absent_in_the_trial_still_confirms() {
    let mut board = trial_board();
    board.baseline("a", "camera@usb:1-1 hotplug\n");
    board.next_boot(1);
    let view = View(vec![]);
    assert_eq!(board.settle_at(31, &view), None);
    assert_eq!(board.state().default, "b");
    assert_eq!(resolved(&board).outcome, "confirmed");
}

#[test]
fn a_hotplug_device_that_is_gone_is_not_a_regression() {
    let mut board = trial_board();
    board.baseline("a", "cam hotplug\n");
    board.next_boot(1);
    // Gone is not failed: the device's resource is dead, and nothing failed.
    let gone = View(vec![(
        "cam",
        Origin::Hotplug,
        false,
        Health::Gone("unplugged"),
    )]);
    assert_eq!(board.settle_at(31, &gone), None);
    assert_eq!(board.state().default, "b");
}

#[test]
fn a_health_source_not_good_by_the_deadline_restarts_the_board_and_names_it() {
    let mut board = trial_board();
    board.baseline("a", "dev builtin\n");
    board.slots.policy.health = vec!["device:sensor".into()];
    board.slots.sources = vec![Source::Device("sensor".into())];
    board.next_boot(1);
    let view = View(vec![
        ("dev", Origin::BuiltIn, true, GOOD),
        (
            "sensor",
            Origin::BuiltIn,
            true,
            Health::Degraded("no calibration"),
        ),
    ]);
    board.settle_at(31, &view);
    assert_eq!(board.state().trial, "b");
    assert_eq!(board.settle_at(600, &view), None);
    let reason = Log::load(board.layout()).unwrap().reason;
    assert!(reason.contains("device:sensor"), "{reason}");
    assert!(board.told.take().contains(&"restart".to_string()));
}

#[test]
fn with_reboot_at_deadline_off_the_reason_is_kept_and_the_board_is_not_restarted() {
    let mut board = trial_board();
    board.baseline("a", "dev builtin\n");
    board.slots.policy.reboot_at_deadline = false;
    board.slots.sources = vec![Source::Device("sensor".into())];
    board.next_boot(1);
    let view = View(vec![("dev", Origin::BuiltIn, true, GOOD)]);
    board.settle_at(600, &view);
    assert_eq!(board.state().trial, "b", "still waiting");
    assert!(Log::load(board.layout()).unwrap().reason.contains("sensor"));
    assert!(!board.told.take().contains(&"restart".to_string()));
}

#[test]
fn with_auto_confirm_off_nothing_confirms_or_restarts_by_itself() {
    let mut board = trial_board();
    board.baseline("a", "dev builtin\n");
    board.slots.policy.auto_confirm = false;
    board.next_boot(1);
    let view = View(vec![("dev", Origin::BuiltIn, true, GOOD)]);
    assert_eq!(board.settle_at(31, &view), None);
    assert_eq!(board.settle_at(900, &view), None);
    assert_eq!(board.state().trial, "b");
    assert!(!board.told.take().contains(&"restart".to_string()));
}

#[test]
fn an_explicit_confirm_during_the_wait_is_recorded_with_its_caller() {
    let mut board = trial_board();
    board.baseline("a", "dev builtin\n");
    board.slots.policy.auto_confirm = false;
    board.next_boot(1);
    board.slots.caller(CallerId(7));
    board.slots.confirm().unwrap();
    assert_eq!(board.state().default, "b");
    let record = resolved(&board);
    assert_eq!(
        (record.outcome.as_str(), record.by.as_str()),
        ("confirmed", "call")
    );
    assert_eq!(record.caller, Some(7));
}

#[test]
fn a_slot_state_that_cant_be_written_does_not_confirm() {
    use std::os::unix::fs::PermissionsExt;
    let mut board = trial_board();
    board.baseline("a", "dev builtin\n");
    board.next_boot(1);
    let dir = board.layout().state_dir();
    let view = View(vec![("dev", Origin::BuiltIn, true, GOOD)]);
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o500)).unwrap();
    let waiting = board.settle_at(31, &view);
    // Still read-only at the deadline: the reason is tried again with backoff, and the board
    // restarts when the tries run out, as the reason can't be written where nothing can be.
    let mut at = board.settle_at(600, &view);
    assert_eq!(at, Some(secs(601)));
    for now in [601, 603, 607, 615, 623] {
        at = board.settle_at(now, &view);
    }
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(waiting.is_some(), "waits while the state can't be written");
    assert_eq!(at, None);
    assert!(board.told.take().contains(&"restart".to_string()));
}

#[test]
fn a_power_cut_on_each_of_three_boots_is_still_tried_and_the_fourth_confirms() {
    let mut board = trial_board();
    board.baseline("a", "dev builtin\n");
    let view = View(vec![("dev", Origin::BuiltIn, true, GOOD)]);
    for n in 1..=3 {
        assert_eq!(board.next_boot(n), "b", "boot {n} is tried");
        board.settle_at(0, &view);
        board.settle_at(10, &view); // the power goes here, before the uptime
        assert_eq!(
            board.told.trial(),
            ("b".to_string(), 3),
            "attempt given back"
        );
        assert_eq!(board.state().trial, "b");
    }
    // Recorded: each boot is an attempt.
    let trial = board.slots.trial_json(&board.state());
    assert!(trial.contains("\"attempts_left\":3"), "{trial}");
    assert!(
        trial.contains("\"last\":\"daemon-reached-not-yet-confirmed\""),
        "{trial}"
    );
    assert_eq!(board.next_boot(4), "b");
    // A second start within the same boot gives nothing back twice.
    board.slots.resolve_at_start();
    assert_eq!(
        board.told.trial(),
        ("b".to_string(), 3),
        "the same boot gives nothing more back"
    );
    assert_eq!(board.settle_at(31, &view), None);
    assert_eq!(board.state().default, "b");
    assert_eq!(resolved(&board).by, "auto");
}

#[test]
fn three_boots_that_never_reach_the_daemon_fall_back_with_the_reason_and_it_is_recorded_on_the_next_boot()
 {
    let mut board = trial_board();
    board.baseline("a", "dev builtin\n");
    // Three trial boots, each cut before any driver starts.
    for _ in 0..3 {
        board.told.boot();
    }
    assert_eq!(board.told.trial().1, 0);
    assert_eq!(board.next_boot(4), "a", "the default starts");
    let record = resolved(&board);
    assert_eq!(
        (
            record.slot.as_str(),
            record.outcome.as_str(),
            record.by.as_str()
        ),
        ("b", "fell-back", "auto")
    );
    assert_eq!(
        record.reason,
        "slot b did not reach a running daemon in 3 boots"
    );
    assert_eq!(board.state().trial, "");
    assert_eq!(board.told.trial().0, "");
    // Recorded for the boots after it, too.
    board.next_boot(5);
    assert_eq!(resolved(&board).reason, record.reason);
}

#[test]
fn the_reason_the_trial_slot_recorded_is_the_one_that_is_given() {
    let mut board = trial_board();
    board.baseline("a", "dev builtin\n");
    board.told.boot();
    let mut meta = Meta::load(board.layout(), "b");
    meta.set("FAILED", "yes");
    meta.set("FAILURES", "1");
    meta.set("FAIL_REASON", "dm-verity refused the image");
    meta.save(board.layout(), "b").unwrap();
    for _ in 0..2 {
        board.told.boot();
    }
    board.next_boot(4);
    assert_eq!(resolved(&board).reason, "dm-verity refused the image");
}

#[test]
fn a_single_slot_board_has_no_trial_and_writes_its_baseline_once_it_is_up() {
    let mut board = Board::new("a");
    let view = View(vec![("dev", Origin::BuiltIn, true, GOOD)]);
    board.next_boot(1);
    board.settle_at(0, &view);
    assert_eq!(board.settle_at(31, &view), None);
    let baseline = fs::read_to_string(board.layout().marker("a.devices")).unwrap();
    assert_eq!(baseline, "dev builtin\n");
    let list = board.list();
    assert!(list.contains("\"trial\":null"), "{list}");
    assert!(list.contains("\"resolved\":null"), "{list}");
}

#[test]
fn a_trial_is_judged_after_the_board_loses_its_previous_slot() {
    let mut board = trial_board();
    board.baseline("a", "dev builtin\n");
    board.next_boot(1);
    let view = View(vec![("dev", Origin::BuiltIn, true, GOOD)]);
    board.settle_at(31, &view);
    assert_eq!(board.state().default, "b");
    // The board's baseline is `b`'s own now; `a` goes, and `c` is tried.
    board.slots.running = Some("b".into());
    board.slots.remove("a").unwrap();
    board.install_ok("c");
    board.trial_slot("c");
    board.baseline("b", "dev builtin\n");
    board.next_boot(2);
    assert_eq!(board.settle_at(31, &view), None);
    assert_eq!(board.state().default, "c");
    assert_eq!(board.state().previous, "b");
}

#[test]
fn the_list_has_the_trial_and_the_resolution_as_they_stand() {
    let mut board = trial_board();
    board.baseline("a", "dev builtin\n");
    let list = board.list();
    assert!(
        list.contains("\"trial\":{\"slot\":\"b\",\"attempts_left\":3"),
        "{list}"
    );
    assert!(list.ends_with("\"resolved\":null}"), "{list}");
    board.next_boot(1);
    board.settle_at(31, &View(vec![("dev", Origin::BuiltIn, true, GOOD)]));
    let list = board.list();
    assert!(list.contains("\"trial\":null"), "{list}");
    assert!(
        list.contains("\"outcome\":\"confirmed\",\"by\":\"auto\""),
        "{list}"
    );
}

/// A `indicator/1` device that records every state it is set to.
struct Recorder(std::sync::Mutex<Vec<(String, u16)>>);

impl lemnos_core::Router for Recorder {
    fn call(
        &self,
        _device: u32,
        method: lemnos_core::MethodId,
        args: &[u8],
        _reply: &mut [u8],
    ) -> Result<usize, Status> {
        use lemnos_core::Decode;
        assert_eq!(method.method, 1, "only set is called");
        let mut from = lemnos_core::Reader::new(args);
        let state = <&str>::decode(&mut from)?.to_string();
        let progress = u16::decode(&mut from)?;
        self.0.lock().unwrap().push((state, progress));
        Ok(0)
    }

    fn health(&self, _device: u32) -> lemnos_core::Health {
        lemnos_core::Health::Ok
    }
}

#[test]
fn the_indicator_shows_each_phase_of_an_install_and_only_changes() {
    use lemnos_contract_indicator::NO_PROGRESS;
    use lemnos_core::{Dev, LocalCaller};

    let recorder: &'static Recorder = Box::leak(Box::new(Recorder(Default::default())));
    let mut board = Board::new("os-1");
    board.slots.indicator = Some(Dev::new(LocalCaller::new(recorder, 0, 0)));
    let bundle = Bundle::new("os-2").pack();
    let (status, _, _) = board.stream(1, &bundle, 1000);
    assert_eq!(status, Status::OK);
    // A tick after the last method call shows the end; the phases came in order, once each.
    let shown = recorder.0.lock().unwrap().clone();
    let states: Vec<&str> = shown.iter().map(|(s, _)| s.as_str()).collect();
    // The phases come in order: receiving while the bytes arrive, verifying while `finish` checks the
    // hash, verity and signature (even within the one call that delivers the last bytes), then done.
    let verify = states
        .iter()
        .position(|s| *s == "verifying")
        .expect("the verify phase is shown");
    assert_eq!(states.first(), Some(&"receiving"));
    assert!(states[..verify].iter().all(|s| *s == "receiving"));
    assert_eq!(states.last(), Some(&"done"));
    assert_eq!(states.iter().filter(|s| **s == "done").count(), 1);
    assert_eq!(shown.last().unwrap().1, NO_PROGRESS);
    // Receiving is sent with progress that rises.
    let progress: Vec<u16> = shown
        .iter()
        .filter(|(s, _)| s == "receiving")
        .map(|(_, p)| *p)
        .collect();
    assert!(progress.windows(2).all(|w| w[0] <= w[1]), "{progress:?}");
    assert!(progress.iter().all(|p| *p <= 1000));
}

#[test]
fn a_cancelled_install_shows_failed() {
    use lemnos_core::{Dev, LocalCaller};

    let recorder: &'static Recorder = Box::leak(Box::new(Recorder(Default::default())));
    let mut board = Board::new("os-1");
    board.slots.indicator = Some(Dev::new(LocalCaller::new(recorder, 0, 0)));
    let bundle = Bundle::new("os-2").pack();
    board.slots.caller(CallerId(1));
    let job = board
        .slots
        .install(bundle.len() as u64, false, false)
        .unwrap();
    board.slots.write(0, &bundle[..3000]).unwrap();
    board.slots.tick(0);
    board.slots.job_cancel(job);
    board.slots.tick(0);
    let shown = recorder.0.lock().unwrap().clone();
    assert_eq!(shown.last().map(|(s, _)| s.as_str()), Some("failed"));
}

/// The boot partition's node for a board whose reason is kept raw: a FAT image with the records and
/// pointers pre-created, beside the board's boot directory.
fn reason_node(board: &Board) -> PathBuf {
    let node = board.layout().boot.with_file_name("reason.img");
    fs::write(&node, crate::bay_fixture::tryboot_image("os-1")).unwrap();
    node
}

fn secs(s: u64) -> u64 {
    s * 1_000_000_000
}

/// The trial's log can't be written: a directory stands where its file is, so the write fails
/// and every other state file still reads.
fn block_log(board: &Board) {
    let path = board.layout().marker("trial.env");
    let _ = fs::remove_file(&path);
    fs::create_dir_all(&path).unwrap();
}

fn unblock_log(board: &Board) {
    fs::remove_dir(board.layout().marker("trial.env")).unwrap();
}

fn failing() -> View {
    View(vec![(
        "dev",
        Origin::BuiltIn,
        true,
        Health::Failed("broken"),
    )])
}

#[test]
fn a_deadline_restart_retries_the_reason_with_capped_backoff_before_it_restarts() {
    let mut board = trial_board();
    board.slots.boot_node = Ok(reason_node(&board));
    board.baseline("a", "dev builtin\n");
    board.next_boot(1);
    let view = failing();
    assert!(board.settle_at(31, &view).is_some());
    block_log(&board);
    // At the deadline the reason can't be written: the tries come at 0, 1, 3, 7, 15 and 23 s
    // after it, the waits doubling and capped at 8 s, and the board restarts with the last.
    assert_eq!(board.settle_at(600, &view), Some(secs(601)));
    assert_eq!(board.settle_at(601, &view), Some(secs(603)));
    assert_eq!(board.settle_at(603, &view), Some(secs(607)));
    assert_eq!(board.settle_at(607, &view), Some(secs(615)));
    assert_eq!(board.settle_at(615, &view), Some(secs(623)));
    assert!(
        !board.told.take().contains(&"restart".to_string()),
        "no restart while the reason is still being tried"
    );
    assert_eq!(board.settle_at(623, &view), None);
    assert!(board.told.take().contains(&"restart".to_string()));
    // The default still comes back: the trial's attempts are used up.
    assert_eq!(board.slots.record.pending().unwrap(), Some(("b".into(), 0)));
    // `list` says the record could not be written.
    let list = board.slots.list_json();
    assert!(
        list.contains("\"record_error\":\"the trial's record can't be written"),
        "{list}"
    );
    // The reason is kept on the boot partition, where the next boot reads it.
    assert!(
        crate::boot::read_reason(&board.slots.boot_node)
            .unwrap()
            .contains("broken")
    );
}

#[test]
fn once_the_reason_can_be_written_the_deadline_restart_goes_and_nothing_is_reported() {
    let mut board = trial_board();
    board.slots.boot_node = Ok(reason_node(&board));
    board.baseline("a", "dev builtin\n");
    board.next_boot(1);
    let view = failing();
    board.settle_at(31, &view);
    block_log(&board);
    assert_eq!(board.settle_at(600, &view), Some(secs(601)));
    assert!(!board.told.take().contains(&"restart".to_string()));
    unblock_log(&board);
    assert_eq!(board.settle_at(601, &view), None);
    assert!(board.told.take().contains(&"restart".to_string()));
    let reason = Log::load(board.layout())
        .expect("the reason is written")
        .reason;
    assert!(reason.contains("broken"), "{reason}");
    assert!(board.slots.list_json().contains("\"record_error\":null"));
}

#[test]
fn a_reason_the_state_never_held_is_not_kept_on_the_boot_partition() {
    let mut board = trial_board();
    board.slots.boot_node = Ok(reason_node(&board));
    board.baseline("a", "dev builtin\n");
    board.next_boot(1);
    let view = failing();
    board.settle_at(31, &view);
    block_log(&board);
    for at in [600, 601, 603, 607, 615, 623] {
        board.settle_at(at, &view);
    }
    unblock_log(&board);
    assert!(board.told.take().contains(&"restart".to_string()));
    // The next boot is the default's, and the trial's record is judged from what was left.
    let slot = board.next_boot(2);
    assert_eq!(slot, "a");
    let record = resolved(&board);
    assert_eq!(
        (record.slot.as_str(), record.outcome.as_str()),
        ("b", "fell-back")
    );
    assert!(record.reason.contains("broken"), "{}", record.reason);
    assert!(
        crate::boot::read_reason(&board.slots.boot_node).is_none(),
        "the copy is cleared once the trial is resolved"
    );
}

#[test]
fn a_hot_pluggable_device_that_is_unbound_and_failed_is_not_a_regression() {
    // An unplugged USB device's driver is dropped and retries as failed: it is not there.
    let mut board = trial_board();
    board.baseline("a", "cam hotplug\n");
    board.next_boot(1);
    let unplugged = View(vec![(
        "cam",
        Origin::Hotplug,
        false,
        Health::Failed("retrying"),
    )]);
    assert_eq!(board.settle_at(31, &unplugged), None);
    assert_eq!(board.state().default, "b");
}

#[test]
fn a_built_in_device_that_is_gone_blocks_the_confirm_and_is_named() {
    let mut board = trial_board();
    board.baseline("a", "dev builtin\n");
    board.next_boot(1);
    let gone = View(vec![("dev", Origin::BuiltIn, true, Health::Gone("dead"))]);
    assert!(board.settle_at(31, &gone).is_some());
    assert_eq!(board.state().trial, "b", "no confirm");
    assert!(Log::load(board.layout()).unwrap().reason.contains("gone"));
}

#[test]
fn a_hot_pluggable_device_failed_while_bound_still_blocks_the_confirm() {
    let mut board = trial_board();
    board.baseline("a", "cam hotplug\n");
    board.next_boot(1);
    let sick = View(vec![("cam", Origin::Hotplug, true, Health::Failed("sick"))]);
    assert!(board.settle_at(31, &sick).is_some());
    assert_eq!(board.state().trial, "b", "no confirm");
    assert!(Log::load(board.layout()).unwrap().reason.contains("cam"));
}

/// A socket source whose listener's backlog is full: its check must come back at once, busy,
/// and only be not good at its deadline. A blocking connect would hang here.
#[test]
fn a_socket_whose_backlog_is_full_is_busy_and_never_blocks_a_look() {
    use std::os::unix::net::UnixListener;
    use std::sync::mpsc;
    use std::time::{Duration, SystemTime};

    use crate::trial::{self, Source};

    let dir = TempDir::new("socket");
    let path = dir.0.join("health.sock");
    let listener = UnixListener::bind(&path).unwrap();
    // Nothing accepts, so connections queue until the backlog is full.
    let mut queued = 0;
    while let Ok(lemnos_linux_sys::unix::Connect::Connected) =
        lemnos_linux_sys::unix::connect_nonblocking(&path)
    {
        queued += 1;
        assert!(queued < 100_000, "the backlog never filled");
    }
    let sources = vec![Source::Socket(path.clone())];
    let (tx, rx) = mpsc::channel();
    let looked = sources.clone();
    std::thread::spawn(move || {
        let mut probes = Vec::new();
        let busy = trial::source_unmet(&looked, &mut probes, &[], SystemTime::now(), 0);
        tx.send((busy, probes)).unwrap();
    });
    let (busy, mut probes) = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("a look at a full backlog returned: it did not block");
    let busy = busy.expect("busy is not good");
    assert!(busy.contains("busy"), "{busy}");
    assert_eq!(probes.len(), 1, "one probe per source, kept between looks");
    // Still busy before the deadline, and not good at it.
    let at = |secs: u64| secs * 1_000_000_000;
    let still = trial::source_unmet(&sources, &mut probes, &[], SystemTime::now(), at(1)).unwrap();
    assert!(still.contains("busy"), "{still}");
    let late = trial::source_unmet(&sources, &mut probes, &[], SystemTime::now(), at(2)).unwrap();
    assert!(late.contains("did not accept"), "{late}");
    // Once the listener takes its connections, the same source is good.
    listener.set_nonblocking(true).unwrap();
    while listener.accept().is_ok() {}
    assert_eq!(
        trial::source_unmet(&sources, &mut probes, &[], SystemTime::now(), at(3)),
        None
    );
}

/// The boot history's entries as `lemnos/boots` reads them.
fn history(board: &Board) -> Vec<lemnos_slot_store::boots::Entry> {
    lemnos_slot_store::boots::load(board.layout())
}

#[test]
fn the_boot_history_records_a_trial_as_on_trial_and_resolves_its_entry_once() {
    let mut board = trial_board();
    board.baseline("a", "dev builtin\n");
    board.next_boot(1);
    let last = history(&board).last().cloned().unwrap();
    assert_eq!(
        (last.slot.as_str(), last.outcome.as_str()),
        ("b", "on-trial")
    );
    let view = View(vec![("dev", Origin::BuiltIn, true, GOOD)]);
    board.settle_at(0, &view);
    assert_eq!(board.settle_at(31, &view), None, "confirmed");
    let last = history(&board).last().cloned().unwrap();
    assert_eq!(last.outcome, "confirmed");
}

#[test]
fn a_boot_writes_one_history_entry_even_when_the_driver_starts_again_within_it() {
    let mut board = trial_board();
    board.baseline("a", "dev builtin\n");
    board.next_boot(1);
    let before = history(&board).len();
    board.slots.record_boot();
    assert_eq!(history(&board).len(), before, "same boot: no second entry");
}

#[test]
fn a_fallen_back_trial_is_recorded_in_the_boot_history_with_its_reason() {
    let mut board = trial_board();
    board.baseline("a", "dev builtin\n");
    // The trial boots and its daemon starts (recorded on trial), then three boots cut before any
    // driver starts.
    board.next_boot(1);
    for _ in 0..3 {
        board.told.boot();
    }
    board.next_boot(4);
    let entries = history(&board);
    let trial = entries.iter().find(|e| e.slot == "b").unwrap();
    assert_eq!(trial.outcome, "fell-back");
    assert_eq!(
        entries.last().unwrap().outcome,
        "unknown",
        "the default's boot"
    );
}

#[test]
fn the_boot_history_carries_the_reset_reason_the_board_reports_and_the_request_before_it() {
    let mut board = trial_board();
    let reset = board.layout().boot.join("reset-reason");
    fs::write(&reset, "32\n").unwrap();
    board.slots.reset_reason = Some(reset);
    lemnos_slot_store::store::replace(
        &board.layout().state_dir().join("reboot-request"),
        b"by:7\n",
    )
    .unwrap();
    board.next_boot(1);
    let last = history(&board).last().cloned().unwrap();
    assert_eq!(
        (last.reset.as_str(), last.requested.as_str()),
        ("32", "by:7")
    );
    assert!(
        !board.layout().state_dir().join("reboot-request").exists(),
        "the request is taken by the boot it was for"
    );
}

#[test]
fn the_boot_history_keeps_only_the_newest_boots_kept() {
    let mut board = Board::new("a");
    board.slots.boots_keep = 2;
    for n in 1..=4 {
        board.next_boot(n);
    }
    let kept: Vec<u64> = history(&board).iter().map(|e| e.n).collect();
    assert_eq!(kept.len(), 2, "only the newest two are kept: {kept:?}");
    assert_eq!(
        kept.last().copied(),
        Some(4),
        "numbered across the trimming: {kept:?}"
    );
}

#[test]
fn the_boots_contract_method_writes_the_history_or_refuses_a_buffer_that_is_too_small() {
    let mut board = Board::new("a");
    board.next_boot(1);
    let mut big = vec![0u8; 4096];
    let len = board.slots.boots(&mut big).unwrap();
    let text = std::str::from_utf8(&big[..len]).unwrap();
    assert!(text.starts_with("1 "), "{text}");
    assert_eq!(
        board.slots.boots(&mut [0u8; 1]),
        Err(Status::BUFFER_TOO_SMALL)
    );
}

/// One page of `list_page`: its slots' ids, whether more follow, and the cursor for them.
fn page(board: &mut Board, after: &str, cap: usize) -> Result<(Vec<String>, bool, String), Status> {
    let mut out = vec![0u8; cap];
    let len = board.slots.list_page(after, &mut out)?;
    assert!(
        len <= cap,
        "a page of {len} bytes is over its {cap}-byte buffer"
    );
    let json = Json::parse(core::str::from_utf8(&out[..len]).unwrap()).unwrap();
    let Some(Json::List(slots)) = json.get("slots") else {
        panic!("no slots in the page")
    };
    let ids = slots
        .iter()
        .map(|s| s.get("id").unwrap().text().unwrap().to_string())
        .collect();
    let more = json.get("more") == Some(&Json::Bool(true));
    let next = json.get("next").unwrap().text().unwrap().to_string();
    Ok((ids, more, next))
}

/// Every page from the first, at `cap` bytes each, and the number of pages.
fn walk(board: &mut Board, cap: usize) -> (Vec<String>, usize) {
    let mut ids = Vec::new();
    let mut after = "-".to_string();
    let mut pages = 0;
    loop {
        let (more_ids, more, next) = page(board, &after, cap).unwrap();
        pages += 1;
        ids.extend(more_ids);
        if !more {
            return (ids, pages);
        }
        assert!(pages < 1000, "the walk never ends");
        after = next;
    }
}

/// The unpaged `list`'s slot ids, newest first.
fn listed_ids(board: &Board) -> Vec<String> {
    let json = Json::parse(&board.list()).unwrap();
    let Some(Json::List(slots)) = json.get("slots") else {
        panic!("no slots")
    };
    slots
        .iter()
        .map(|s| s.get("id").unwrap().text().unwrap().to_string())
        .collect()
}

#[test]
fn the_paged_list_walks_every_slot_once_in_the_order_of_the_list() {
    let mut board = Board::new("os-1");
    board.slots.set_keep(0).unwrap();
    for n in 2..=9 {
        board.install_ok(&format!("os-{n}"));
    }
    let all = listed_ids(&board);
    assert_eq!(all.len(), 9);
    // A page of about two slots, so the walk takes several.
    let (ids, pages) = walk(&mut board, 1100);
    assert_eq!(ids, all, "the pages, in order, are the list");
    assert!(pages >= 4, "{pages} pages");
    // A buffer that holds every slot: one page, no cursor.
    let (ids, more, next) = page(&mut board, "-", 64 * 1024).unwrap();
    assert_eq!(ids, all);
    assert!(!more);
    assert_eq!(next, "");
}

#[test]
fn a_slot_installed_between_pages_is_neither_lost_nor_repeated() {
    let mut board = Board::new("os-1");
    board.slots.set_keep(0).unwrap();
    for n in 2..=9 {
        board.install_ok(&format!("os-{n}"));
    }
    let before = listed_ids(&board);
    let (first, more, next) = page(&mut board, "-", 1100).unwrap();
    assert!(more);
    // Installed between the first and the second page: it is the newest, so it is not on a later
    // page, and nothing already there moves.
    board.install_ok("os-10");
    let mut after = next;
    let mut rest = Vec::new();
    loop {
        let (ids, more, next) = page(&mut board, &after, 1100).unwrap();
        rest.extend(ids);
        if !more {
            break;
        }
        after = next;
    }
    let mut seen = first.clone();
    seen.extend(rest);
    assert_eq!(seen, before, "every slot that existed once, in order");
    assert!(!seen.contains(&"os-10".to_string()));
    assert_eq!(listed_ids(&board).len(), 10);
    // A walk that starts again sees the new slot first.
    let (again, _) = walk(&mut board, 1100);
    assert_eq!(again[0], "os-10");
}

#[test]
fn a_bad_cursor_is_refused_and_a_buffer_too_small_for_a_slot_is_too_small() {
    let mut board = Board::new("os-1");
    board.install_ok("os-2");
    for bad in ["x", "x:os-2", "1x:os-2"] {
        assert_eq!(
            page(&mut board, bad, 64 * 1024),
            Err(Status::INVALID_ARGUMENT),
            "{bad}"
        );
    }
    assert_eq!(page(&mut board, "-", 100), Err(Status::BUFFER_TOO_SMALL));
    // The smallest cursor comes after every slot: an empty last page.
    let (ids, more, next) = page(&mut board, "0:", 64 * 1024).unwrap();
    assert!(ids.is_empty());
    assert!(!more);
    assert_eq!(next, "");
}

/// The signature member of a signed bundle is the shared 108-byte record: magic, algorithm 1,
/// zeros, the SHA-256 of the signing public key, then the signature over the slot bundle message of the manifest's bytes.
#[test]
fn the_signature_member_is_the_shared_record_and_names_the_key_by_its_id() {
    let bundle = Bundle::new("os-2");
    let record = bundle.pack_record();
    assert_eq!(record.len(), 108);
    assert_eq!(&record[..8], b"LMNSSIG1");
    assert_eq!(record[8], 1);
    assert_eq!(&record[9..12], &[0, 0, 0]);
    // The W1 test key (the seed is the byte 0x07 repeated 32 times): its key id.
    assert_eq!(
        hex(&record[12..44]),
        "fe812c12f3ab4ce6ac5db69ac352f906cb1b11ef43fb33e252ef7ff552263889"
    );
    let parsed = lemnos_slot_store::signing::parse_record(&record).unwrap();
    let signature = ed25519_dalek::Signature::from_bytes(&parsed.signature);
    assert!(
        key()
            .verifying_key()
            .verify_strict(
                &lemnos_slot_store::signing::slot_bundle_message(bundle.manifest().as_bytes()),
                &signature
            )
            .is_ok(),
        "the signed message is the domain-tagged slot.json bytes"
    );
}

/// With signing required and the default slot not verified, the start says so: the state's error
/// names the default and what to install, `info` shows its signature, and the driver keeps running.
#[test]
fn with_require_signed_an_unverified_default_is_reported_and_the_driver_keeps_running() {
    let mut board = Board::new("os-1");
    let mut state = board.state();
    state.default = "os-1".into();
    state.save(board.layout()).unwrap();
    // The default was installed under an unknown key, which the record says.
    let mut meta = board.meta("os-1");
    meta.set("SIGNATURE", "not_checked_unknown_key");
    meta.set("SIGNED", "untrusted");
    meta.save(board.layout(), "os-1").unwrap();
    security::set_view_for_thread(Some(view_file(&board.dir.0, true)));
    board.slots.report_policy();
    let error = board.state().error;
    assert!(error.contains("default slot os-1"), "{error}");
    assert!(error.contains("install a verified slot"), "{error}");
    let mut out = vec![0u8; 4096];
    let len = board.slots.info(&mut out).unwrap();
    let json = Json::parse(core::str::from_utf8(&out[..len]).unwrap()).unwrap();
    assert_eq!(
        json.get("default_signature").and_then(Json::text),
        Some("not_checked_unknown_key")
    );
    // The board still answers its calls.
    let mut list = vec![0u8; 64 * 1024];
    assert!(board.slots.list(&mut list).is_ok());
}

#[test]
fn check_config_names_the_bad_setting() {
    use lemnos_core::{Config, ConfigError, ConfigValue, Lifecycle};
    fn check(
        entries: &'static [(&'static str, ConfigValue<'static>)],
    ) -> core::result::Result<(), ConfigError<'static>> {
        <SystemSlots as Lifecycle>::check_config(&Config::new(entries))
    }
    let full = [
        ("record", ConfigValue::Str("uboot")),
        ("uboot_env_size", ConfigValue::Int(8192)),
        ("reserve_mb", ConfigValue::Int(64)),
        ("keep", ConfigValue::Int(3)),
        ("boots_keep", ConfigValue::Int(10)),
        ("auto_confirm", ConfigValue::Bool(true)),
        ("confirm_uptime_s", ConfigValue::Int(30)),
        ("confirm_deadline_s", ConfigValue::Int(30)),
        ("trial_attempts", ConfigValue::Int(3)),
    ];
    // Every disk boot names its boot sources; the raw NAND form does not.
    assert_eq!(check(&[]).unwrap_err().key, "boot_sources");
    assert!(
        check(&[
            ("record", ConfigValue::Str("ubi")),
            ("ubi", ConfigValue::Str("ubi0")),
        ])
        .is_ok()
    );
    let sourced = Config::new(&full)
        .with_lists(&[("boot_sources", &["/soc/mmc@7e202000=/soc/mmc@7e202000"])]);
    assert!(<SystemSlots as Lifecycle>::check_config(&sourced).is_ok());
    let listed = Config::new(&full).with_lists(&[
        ("health", &["device:x", "file:/run/ok"]),
        ("boot_sources", &["/soc/mmc@7e202000=/soc/mmc@7e202000"]),
    ]);
    assert!(<SystemSlots as Lifecycle>::check_config(&listed).is_ok());
    let malformed = Config::new(&full).with_lists(&[("boot_sources", &["mmc0"])]);
    assert_eq!(
        <SystemSlots as Lifecycle>::check_config(&malformed)
            .unwrap_err()
            .key,
        "boot_sources"
    );

    // The U-Boot environment's keys: the size is required, the partition is found by type (a
    // PARTUUID is refused by name), the fixed-offset form needs all of its facts and nothing else
    // takes them, and flash or UBI is not a block device this writer applies to.
    let env = |extra: &[(&'static str, ConfigValue<'static>)]| {
        let mut all = vec![("record", ConfigValue::Str("uboot"))];
        all.extend_from_slice(extra);
        let all: &'static [(&'static str, ConfigValue<'static>)] =
            Box::leak(all.into_boxed_slice());
        let cfg = Config::new(all)
            .with_lists(&[("boot_sources", &["/soc/mmc@7e202000=/soc/mmc@7e202000"])]);
        <SystemSlots as Lifecycle>::check_config(&cfg).map_err(|e| (e.key.to_string(), e.reason))
    };
    assert_eq!(env(&[]).unwrap_err().0, "uboot_env_size");
    assert!(env(&[("uboot_env_size", ConfigValue::Int(8192))]).is_ok());
    let err = env(&[
        ("uboot_env_size", ConfigValue::Int(8192)),
        (
            "uboot_env_partuuid",
            ConfigValue::Str("0a1b2c3d-0000-4000-8000-000000000042"),
        ),
    ])
    .unwrap_err();
    assert_eq!(err.0, "uboot_env_partuuid");
    assert!(
        err.1.contains("3de21764-95bd-54bd-a5c3-4abe786f38a8"),
        "{}",
        err.1
    );
    assert_eq!(
        env(&[("uboot_env_size", ConfigValue::Int(4))])
            .unwrap_err()
            .0,
        "uboot_env_size"
    );
    let device = [
        ("uboot_env_size", ConfigValue::Int(8192)),
        ("uboot_env_device", ConfigValue::Str("/dev/mmcblk0")),
        ("uboot_env_offset", ConfigValue::Int(0x400000)),
        ("uboot_env_offset_redund", ConfigValue::Int(0x410000)),
    ];
    assert!(env(&device).is_ok());
    assert_eq!(env(&device[..3]).unwrap_err().0, "uboot_env_offset_redund");
    assert_eq!(env(&device[..2]).unwrap_err().0, "uboot_env_offset");
    assert_eq!(
        env(&[
            ("uboot_env_size", ConfigValue::Int(8192)),
            ("uboot_env_offset", ConfigValue::Int(512)),
        ])
        .unwrap_err()
        .0,
        "uboot_env_offset"
    );
    assert_eq!(
        env(&[
            ("uboot_env_size", ConfigValue::Int(8192)),
            ("uboot_env_device", ConfigValue::Str("/dev/mtd3")),
            ("uboot_env_offset", ConfigValue::Int(0)),
            ("uboot_env_offset_redund", ConfigValue::Int(0x10000)),
        ])
        .unwrap_err()
        .0,
        "uboot_env_device"
    );

    let bogus = check(&[("bogus", ConfigValue::Int(1))]).unwrap_err();
    assert_eq!(bogus.key, "bogus");
    let near = check(&[("keeps", ConfigValue::Int(3))]).unwrap_err();
    assert_eq!((near.key, near.suggestion), ("keeps", Some("keep")));
    assert_eq!(
        check(&[("keep", ConfigValue::Str("3"))]).unwrap_err().key,
        "keep"
    );
    assert_eq!(
        check(&[("record", ConfigValue::Str("grub"))])
            .unwrap_err()
            .key,
        "record"
    );
    assert_eq!(
        check(&[("record", ConfigValue::Str("ubi"))])
            .unwrap_err()
            .key,
        "ubi"
    );
    assert_eq!(
        check(&[("reserve_mb", ConfigValue::Int(-1))])
            .unwrap_err()
            .key,
        "reserve_mb"
    );
    assert_eq!(
        check(&[("keep", ConfigValue::Int(-1))]).unwrap_err().key,
        "keep"
    );
    assert_eq!(
        check(&[("boots_keep", ConfigValue::Int(-1))])
            .unwrap_err()
            .key,
        "boots_keep"
    );
    assert_eq!(
        check(&[("confirm_uptime_s", ConfigValue::Int(-1))])
            .unwrap_err()
            .key,
        "confirm_uptime_s"
    );
    let order = check(&[
        ("confirm_uptime_s", ConfigValue::Int(60)),
        ("confirm_deadline_s", ConfigValue::Int(30)),
    ])
    .unwrap_err();
    assert_eq!(order.key, "confirm_deadline_s");
    assert_eq!(
        check(&[("trial_attempts", ConfigValue::Int(0))])
            .unwrap_err()
            .key,
        "trial_attempts"
    );
    assert_eq!(
        check(&[("trial_attempts", ConfigValue::Int(4))])
            .unwrap_err()
            .key,
        "trial_attempts"
    );
    let bad = <SystemSlots as Lifecycle>::check_config(
        &Config::new(&[("record", ConfigValue::Str("tryboot"))]).with_lists(&[
            ("health", &["file:relative/path"]),
            ("boot_sources", &["/soc/mmc@7e202000=/soc/mmc@7e202000"]),
        ]),
    );
    assert_eq!(bad.unwrap_err().key, "health");
    let reboot = <SystemReboot as Lifecycle>::check_config(&Config::new(&[(
        "systemctl",
        ConfigValue::Str("/bin/systemctl"),
    )]));
    assert!(reboot.is_ok());
    assert_eq!(
        <SystemReboot as Lifecycle>::check_config(&Config::new(&[("keys", ConfigValue::Str("x"))]))
            .unwrap_err()
            .key,
        "keys"
    );
}

// ---- I5g2: the signed message, the member order, and what the manifest binds ----

/// The record for `manifest` under the test key, signed over the slot bundle message.
fn record_for(manifest: &str) -> Vec<u8> {
    let key_id: [u8; 32] = Sha256::digest(key().verifying_key().as_bytes()).into();
    let signature = key()
        .sign(&lemnos_slot_store::signing::slot_bundle_message(
            manifest.as_bytes(),
        ))
        .to_bytes();
    lemnos_slot_store::signing::record_bytes(&key_id, &signature)
}

/// The record a first version signed: the bare manifest bytes, with no domain tag.
fn record_over_bare_manifest(manifest: &str) -> Vec<u8> {
    let key_id: [u8; 32] = Sha256::digest(key().verifying_key().as_bytes()).into();
    let signature = key().sign(manifest.as_bytes()).to_bytes();
    lemnos_slot_store::signing::record_bytes(&key_id, &signature)
}

/// The bundle's members in the order the rules give (slot.json, slot.sig, the root, boot/ and the
/// boot files), with `skip` left out and `boot_files` replacing the bundle's own boot files.
fn members(
    b: &Bundle,
    sig: Option<Vec<u8>>,
    skip: &[&str],
    boot_files: &[(&str, Vec<u8>)],
) -> Vec<u8> {
    let manifest = b.manifest();
    let mut files: Vec<(String, Vec<u8>)> =
        vec![("slot.json".into(), manifest.clone().into_bytes())];
    if let Some(sig) = sig {
        files.push(("slot.sig".into(), sig));
    }
    if !skip.contains(&"rootfs.erofs") {
        files.push(("rootfs.erofs".into(), b.image.clone()));
    }
    files.push(("boot/".into(), Vec::new()));
    for (name, bytes) in boot_files {
        if !skip.contains(name) {
            files.push((format!("boot/{name}"), bytes.clone()));
        }
    }
    let borrowed: Vec<(&str, &[u8])> = files
        .iter()
        .map(|(n, v)| (n.as_str(), v.as_slice()))
        .collect();
    archive(&borrowed)
}

/// The bundle's boot files as the bundle holds them.
fn boot_of(b: &Bundle) -> Vec<(&str, Vec<u8>)> {
    b.boot
        .iter()
        .map(|(n, v)| (n.as_str(), v.clone()))
        .collect()
}

/// The bytes of the archive up to the end of the header of the member `name`: a stream fed only
/// this far has given the install every chance to write that member, and none of its data.
fn up_to_header_of(bytes: &[u8], name: &str) -> Vec<u8> {
    let at = bytes
        .windows(name.len() + 1)
        .position(|w| w[..name.len()] == *name.as_bytes() && w[name.len()] == 0)
        .expect("the member is in the archive");
    bytes[..at + 512].to_vec()
}

/// An install fed `bytes` in one piece, as a board's stream would, and its outcome. The work
/// directory is returned as the install left it: nothing is discarded here.
fn feed_raw(board: &Board, bytes: &[u8], require: bool) -> (Result<(), (Status, String)>, PathBuf) {
    let guard = crate::install::Guard {
        running: "os-1".into(),
        default: "os-1".into(),
    };
    let admit = crate::install::Admit {
        keys: vec![TrustedKey {
            key: key().verifying_key(),
            owner: "the build key".into(),
        }],
        require_signed: require,
        board_id: BOARD.into(),
    };
    let job = JobId(77);
    let mut install = crate::install::Install::start(
        board.layout(),
        CallerId(1),
        job,
        bytes.len() as u64,
        false,
        false,
        guard,
        admit,
        None,
    )
    .unwrap_or_else(|f| panic!("{}", f.1));
    let got = install.take(0, bytes).map_err(|f| (f.0, f.1));
    (
        got,
        board.layout().images().join(format!(".install.{}", job.0)),
    )
}

/// The signed message: the record for a bundle's manifest verifies over the domain-tagged message
/// and not over the bare manifest, and the documented vector is what the code makes.
#[test]
fn the_slot_bundle_vector_is_the_documented_record() {
    let manifest = "{\"id\":\"vector\"}";
    let record = record_for(manifest);
    let expect = "4c4d4e535349473101000000fe812c12f3ab4ce6ac5db69ac352f906cb1b11ef43fb33e252ef7ff55226388956be579db020703eb207da579036a89f0321fcd8ce2bac78a9aa939a58217e265ad27a7e1fc05829b0db05d37ac4d8dba35263c7a1b2644322b45ada4a101402";
    assert_eq!(hex(record.as_slice()), expect);
    assert_eq!(
        hex(&lemnos_slot_store::signing::slot_bundle_message(
            manifest.as_bytes()
        )),
        "6c656d6e6f732d736c6f742d62756e646c6500010000000f0000007b226964223a22766563746f72227d"
    );
}

/// A slot signature is not a first-flash signature: the same key's signature over the bare
/// manifest is a known key with a bad signature, refused with the board holding that key.
#[test]
fn a_signature_over_the_bare_manifest_is_refused_as_a_bad_signature_under_a_known_key() {
    let mut board = Board::new("os-1");
    for require in [false, true] {
        security::set_view_for_thread(Some(view_file(&board.dir.0, require)));
        let mut b = Bundle::new("os-2");
        b.sig_file = Some(record_over_bare_manifest(&b.manifest()));
        let (status, _, _) = board.stream(1, &b.pack(), 4096);
        assert_eq!(status, Status::NOT_PERMITTED, "require_signed={require}");
        assert!(
            board.state().error.contains("does not verify"),
            "{}",
            board.state().error
        );
        assert!(!board.layout().installed("os-2"));
        assert!(!board.work_left());
        assert_eq!(last_install(&mut board).1, "refused");
    }
}

/// The policy table with `require_signed` off, through the record: a bundle with no key list
/// installs as not checked, a known good one as verified, and a malformed record is refused.
#[test]
fn one_bundle_installs_on_a_board_that_requires_signing_and_on_one_that_does_not() {
    let bundle = Bundle::new("os-2").pack();
    let mut off = Board::new("os-1");
    assert_eq!(
        off.stream(1, &bundle, 4096).0,
        Status::OK,
        "{}",
        off.state().error
    );
    assert_eq!(last_install(&mut off).1, "verified");
    let mut on = Board::new("os-1");
    security::set_view_for_thread(Some(view_file(&on.dir.0, true)));
    assert_eq!(
        on.stream(1, &bundle, 4096).0,
        Status::OK,
        "{}",
        on.state().error
    );
    assert_eq!(last_install(&mut on).1, "verified");
}

/// `slot.json` is the first member: a bundle that starts with anything else is refused by that
/// rule, and the root image is never written.
#[test]
fn slot_json_must_be_the_first_member() {
    let mut board = Board::new("os-1");
    let b = Bundle::new("os-2");
    let manifest = b.manifest();
    let bytes = archive(&[
        ("rootfs.erofs", b.image.as_slice()),
        ("slot.json", manifest.as_bytes()),
    ]);
    let (status, _, _) = board.stream(1, &bytes, 4096);
    assert_eq!(status, Status::INVALID_ARGUMENT);
    assert!(
        board
            .state()
            .error
            .contains("slot.json must be its first member"),
        "{}",
        board.state().error
    );
    assert!(!board.work_left());
}

/// `slot.sig`, when present, is the second member; a signature that comes later is refused by
/// that rule, before any payload is written.
#[test]
fn slot_sig_must_be_the_second_member() {
    let mut board = Board::new("os-1");
    let b = Bundle::new("os-2");
    let manifest = b.manifest();
    let record = record_for(&manifest);
    let bytes = archive(&[
        ("slot.json", manifest.as_bytes()),
        ("rootfs.erofs", b.image.as_slice()),
        ("slot.sig", record.as_slice()),
    ]);
    let (status, _, _) = board.stream(1, &bytes, 4096);
    assert_eq!(status, Status::INVALID_ARGUMENT);
    assert!(
        board
            .state()
            .error
            .contains("slot.sig must be the bundle's second member"),
        "{}",
        board.state().error
    );
    assert!(!board.work_left());
}

/// Each member at most once, and anything else refused by name.
#[test]
fn a_repeated_member_or_an_unknown_one_is_refused_by_name() {
    let mut board = Board::new("os-1");
    let b = Bundle::new("os-2");
    let manifest = b.manifest();
    let record = record_for(&manifest);
    let twice = archive(&[
        ("slot.json", manifest.as_bytes()),
        ("slot.sig", record.as_slice()),
        ("rootfs.erofs", b.image.as_slice()),
        ("rootfs.erofs", b.image.as_slice()),
    ]);
    assert_eq!(board.stream(1, &twice, 4096).0, Status::INVALID_ARGUMENT);
    assert!(
        board.state().error.contains("rootfs.erofs twice"),
        "{}",
        board.state().error
    );
    assert!(!board.work_left());
    let unknown = archive(&[
        ("slot.json", manifest.as_bytes()),
        ("slot.sig", record.as_slice()),
        ("evil.bin", &b"x"[..]),
    ]);
    assert_eq!(board.stream(1, &unknown, 4096).0, Status::INVALID_ARGUMENT);
    assert!(
        board.state().error.contains("evil.bin"),
        "{}",
        board.state().error
    );
    assert!(!board.work_left());
}

/// A bundle the policy refuses writes nothing: fed up to the header of its root image, the
/// install has the signature decided and refused, with nothing in its work directory and the root
/// image never begun. Covers an unsigned bundle on a board that requires signing, a bad
/// signature under a known key, and a bundle for another board.
#[test]
fn a_refused_bundle_writes_nothing_and_the_root_image_is_not_consumed() {
    let board = Board::new("os-1");
    let unsigned = Bundle::new("os-2");
    let mut bad_sig = Bundle::new("os-3");
    let mut record = record_for(&bad_sig.manifest());
    record[60] ^= 1;
    bad_sig.sig_file = Some(record);
    let mut other = Bundle::new("os-4");
    other.board = "another-board".into();
    let cases = [
        (
            "unsigned",
            members(&unsigned, None, &[], &boot_of(&unsigned)),
            true,
        ),
        (
            "bad signature",
            members(&bad_sig, bad_sig.sig_file.clone(), &[], &boot_of(&bad_sig)),
            false,
        ),
        (
            "another board",
            members(
                &other,
                Some(record_for(&other.manifest())),
                &[],
                &boot_of(&other),
            ),
            false,
        ),
    ];
    for (what, bytes, require) in cases {
        let prefix = up_to_header_of(&bytes, "rootfs.erofs");
        let (got, work) = feed_raw(&board, &prefix, require);
        assert!(got.is_err(), "{what}: refused");
        assert!(
            !work.exists() || fs::read_dir(&work).unwrap().next().is_none(),
            "{what}: the work directory is empty"
        );
        assert!(
            !work.join("rootfs.erofs").exists(),
            "{what}: the root image was not begun"
        );
    }
}

/// A boot file the manifest does not list is refused before its bytes are written, by name.
#[test]
fn an_unlisted_boot_file_is_refused_before_it_is_written() {
    let board = Board::new("os-1");
    let mut b = Bundle::new("os-2");
    b.extra.push(("evil.dtbo".into(), b"x".to_vec()));
    let bytes = members(&b, Some(b.pack_record()), &[], &{
        let mut files = boot_of(&b);
        files.push(("evil.dtbo", b"x".to_vec()));
        files
    });
    let prefix = up_to_header_of(&bytes, "boot/evil.dtbo");
    let (got, work) = feed_raw(&board, &prefix, false);
    let (status, text) = got.unwrap_err();
    assert_eq!(status, Status::INVALID_ARGUMENT);
    assert!(
        text.contains("boot file evil.dtbo isn't in slot.json"),
        "{text}"
    );
    assert!(!work.join("boot/evil.dtbo").exists());
}

/// A listed member that is missing is refused before the slot is put in place, and nothing is
/// left behind. Covers the root image and a boot file.
#[test]
fn a_listed_member_that_is_missing_is_refused() {
    for (skip, text) in [
        ("rootfs.erofs", "no rootfs.erofs"),
        ("initramfs.cpio", "no boot file initramfs.cpio"),
    ] {
        let mut board = Board::new("os-1");
        let b = Bundle::new("os-2");
        let bytes = members(&b, Some(b.pack_record()), &[skip], &boot_of(&b));
        let (status, _, _) = board.stream(1, &bytes, 4096);
        assert_eq!(status, Status::INVALID_ARGUMENT, "skipping {skip}");
        assert!(
            board.state().error.contains(text),
            "{}",
            board.state().error
        );
        assert!(!board.layout().installed("os-2"));
        assert!(!board.work_left());
    }
}

/// The root image's digest is the manifest's: an image that differs with the same length is
/// refused, under a valid signature, and nothing is installed.
#[test]
fn a_root_image_that_differs_from_the_manifest_digest_is_refused() {
    let mut board = Board::new("os-1");
    let mut b = Bundle::new("os-2");
    let good_sha = sha(b.image.as_slice());
    b.manifest_image_sha = Some(good_sha);
    b.image[100] ^= 0xff;
    let (status, _, _) = board.stream(1, &b.pack(), 4096);
    assert_eq!(status, Status::INVALID_ARGUMENT);
    assert!(
        board
            .state()
            .error
            .contains("doesn't match slot.json's sha256"),
        "{}",
        board.state().error
    );
    assert!(!board.layout().installed("os-2"));
    assert!(!board.work_left());
}

/// A boot file whose bytes differ from the manifest's digest is refused, under a valid signature.
/// The command line is one of those files, so a changed command line is refused the same way.
#[test]
fn a_boot_file_that_differs_from_the_manifest_digest_is_refused() {
    let mut board = Board::new("os-1");
    let b = Bundle::new("os-2");
    let mut files = boot_of(&b);
    for (name, bytes) in &mut files {
        if *name == "cmdline.txt" {
            *bytes = b"console=tty1 init=/bin/sh\n".to_vec();
        }
    }
    let bytes = members(&b, Some(b.pack_record()), &[], &files);
    let (status, _, _) = board.stream(1, &bytes, 4096);
    assert_eq!(status, Status::INVALID_ARGUMENT);
    assert!(
        board
            .state()
            .error
            .contains("boot file cmdline.txt doesn't match slot.json"),
        "{}",
        board.state().error
    );
    assert!(!board.layout().installed("os-2"));
    assert!(!board.work_left());
}

/// A slot id that is not a slot id is refused at slot.json, before any payload: it would name a
/// path, so the form is checked, not only the words.
#[test]
fn a_slot_id_of_the_wrong_form_is_refused_at_the_manifest() {
    let mut board = Board::new("os-1");
    for id in ["bad/id", "..", "has space"] {
        let mut b = Bundle::new("os-2");
        b.id = id.into();
        let (status, _, _) = board.stream(1, &b.pack(), 4096);
        assert_eq!(status, Status::INVALID_ARGUMENT, "{id}");
        assert!(
            board.state().error.contains("isn't a slot id"),
            "{}",
            board.state().error
        );
        assert!(!board.work_left());
    }
}

/// `info` gives the ids (SHA-256 of the public key) of the keys the board holds, with their owners.
#[test]
fn info_names_the_held_keys_by_id_and_owner() {
    let mut board = Board::new("os-1");
    let mut out = vec![0u8; 4096];
    let len = board.slots.info(&mut out).unwrap();
    let json = Json::parse(core::str::from_utf8(&out[..len]).unwrap()).unwrap();
    let Some(Json::List(keys)) = json.get("key_ids") else {
        panic!("no key_ids")
    };
    assert_eq!(keys.len(), 1);
    let id = keys[0].get("id").and_then(Json::text).unwrap();
    assert_eq!(id, hex(&Sha256::digest(key().verifying_key().as_bytes())));
    assert_eq!(
        keys[0].get("owner").and_then(Json::text),
        Some("the build key")
    );
}

#[test]
fn a_bundle_whose_command_line_lacks_its_own_slot_or_root_hash_is_refused_by_name() {
    // The command line is the bundle's, byte for byte: nothing is appended at install, so a
    // bundle that names another slot or another root hash is refused before anything is written.
    let mut board = Board::new("os-1");
    let mut wrong_slot = Bundle::new("os-2");
    wrong_slot.boot[1].1 =
        format!("console=tty1 board.slot=os-9 board.roothash={ROOT_HASH} panic=5\n").into_bytes();
    assert_eq!(
        board.stream(1, &wrong_slot.pack(), 4096).0,
        Status::INVALID_ARGUMENT
    );
    assert!(
        board.state().error.contains("board.slot=os-2"),
        "{}",
        board.state().error
    );
    let mut wrong_hash = Bundle::new("os-2");
    wrong_hash.boot[1].1 = format!(
        "console=tty1 board.slot=os-2 board.roothash={} panic=5\n",
        "0".repeat(64)
    )
    .into_bytes();
    assert_eq!(
        board.stream(1, &wrong_hash.pack(), 4096).0,
        Status::INVALID_ARGUMENT
    );
    assert!(
        board.state().error.contains("roothash"),
        "{}",
        board.state().error
    );
    assert!(!board.layout().installed("os-2"));
    assert!(!board.work_left());
}

#[test]
fn a_trial_is_not_armed_when_the_restart_it_needs_is_refused() {
    // A refused check (the default and previous both differ from their manifests) arms nothing:
    // the trial record is not written, nothing restarts, and the reason is the state's error.
    let mut board = Board::new("os-1");
    board.install_ok("os-2");
    board.told.take();
    board.slots.check = Some(crate::boot::Check(std::sync::Arc::new(|_| {
        Err("both slots' boot files differ from their manifests".to_string())
    })));
    assert_eq!(
        board.slots.trial("os-2", false, false, false),
        Err(Status::IO)
    );
    assert!(board.told.take().is_empty());
    assert_eq!(board.state().trial, "");
    assert!(
        board
            .state()
            .error
            .contains("both slots' boot files differ"),
        "{}",
        board.state().error
    );
}

/// The never-strand case: the view is malformed, so the switch reads on, on a board with no trust
/// root. Installs are refused, and `info` reports the switch the readers read (its `require_signed`),
/// not the record's; a valid view that says off is read as off again.
#[test]
fn a_malformed_view_reads_on_on_a_board_with_no_trust_root_and_info_says_so() {
    let dir = TempDir::new("corrupt-view");
    let mut slots = SystemSlots::new(layout(&dir.0), Box::new(Told::default()), Vec::new());
    let path = dir.0.join("security");
    fs::write(&path, b"on\ngeneration 1\nextra\n").unwrap();
    security::set_view_for_thread(Some(path.clone()));
    let mut out = vec![0u8; 4096];
    let len = slots.info(&mut out).unwrap();
    let json = Json::parse(core::str::from_utf8(&out[..len]).unwrap()).unwrap();
    assert_eq!(json.get("require_signed"), Some(&Json::Bool(true)));
    assert_eq!(slots.install(10, false, false), Err(Status::NOT_PERMITTED));
    // A valid view that says off: the readers read off, and the same board installs.
    fs::write(&path, b"off\ngeneration 2\n").unwrap();
    let len = slots.info(&mut out).unwrap();
    let json = Json::parse(core::str::from_utf8(&out[..len]).unwrap()).unwrap();
    assert_eq!(json.get("require_signed"), Some(&Json::Bool(false)));
    assert!(slots.install(10, false, false).is_ok());
}

/// The Pi firmware's record over a real boot partition image: os-1 in bay 0, as the image builder
/// writes it. Returns the image's path.
fn use_tryboot(board: &mut Board) -> PathBuf {
    let node = board.dir.0.join("boot.img");
    fs::write(&node, crate::bay_fixture::tryboot_image("os-1")).unwrap();
    board.slots.record = Box::new(crate::boot::Tryboot {
        boot: board.layout().boot.clone(),
        node: Ok(node.clone()),
        restarter: crate::boot::Restarter {
            check: None,
            systemctl: "/bin/true".into(),
        },
    });
    node
}

/// The extra file a bundle may try to carry in its boot files.
const EXTRA: &str = "overlays/extra.dtbo";

/// A bundle whose `boot.files` lists a sixth file, present in the bundle.
fn bundle_with_extra(id: &str) -> Bundle {
    let mut b = Bundle::new(id);
    b.boot.push((EXTRA.into(), b"a second overlay".to_vec()));
    b
}

/// The U-Boot record over a real boot partition image (os-1 in bay 0) and an environment whose
/// default is os-1. Returns the image's path.
fn use_uboot(board: &mut Board) -> PathBuf {
    // The boot partition, built by the image builder: os-1 in bay 0, no pointers or records.
    let boot_node = board.dir.0.join("boot.img");
    fs::write(&boot_node, crate::bay_fixture::uboot_image("os-1")).unwrap();
    // The environment: a valid copy on a node of its own (the partition U-Boot keeps it on), whose
    // default is os-1 in bay 0.
    let node = board.layout().boot.join("uboot-env.img");
    let geometry = lemnos_slot_store::uboot_env::Geometry::new(1024, 0, 4096).unwrap();
    let list = vec![
        ("default".to_string(), "os-1".to_string()),
        ("default_bay".to_string(), "bay0".to_string()),
    ];
    let data = lemnos_slot_store::uboot_env::encode_vars(&list, geometry.data_len()).unwrap();
    let mut image = vec![0u8; 8192];
    image[..1024].copy_from_slice(&lemnos_slot_store::uboot_env::encode_copy(&data, 1));
    fs::write(&node, &image).unwrap();
    board.slots.record = Box::new(crate::uboot::UbootEnv {
        disk: Err("the device is named, the disk is not needed".into()),
        place: lemnos_slot_store::uboot_env::Place::device(node.to_str().unwrap(), 1024, 0, 4096)
            .unwrap(),
        restarter: crate::boot::Restarter {
            check: None,
            systemctl: "/bin/true".into(),
        },
        boot_node: Ok(boot_node.clone()),
    });
    boot_node
}

/// The file form: one `boot-record` file, nothing of the slot's on the boot partition.
fn use_file_record(board: &mut Board) {
    board.slots.record = Box::new(crate::boot::FileRecord {
        boot: board.layout().boot.clone(),
        restarter: crate::boot::Restarter {
            check: None,
            systemctl: "/bin/true".into(),
        },
    });
}

/// What the refusal says for os-2 and the extra entry.
fn refusal_of_extra() -> String {
    format!(
        "os-2: boot.files lists \"{EXTRA}\", which this board's boot partition does not store (it stores: kernel8.img, initramfs.cpio, board.dtb, cmdline.txt, overlays/lemnos-board.dtbo): nothing was written, the default keeps booting"
    )
}

/// Each form, as a name, the board over it, and the boot partition image it writes (none for the
/// file form, which writes nothing there).
fn forms() -> Vec<(&'static str, Board, Option<PathBuf>)> {
    let mut tryboot = Board::new("os-1");
    let node = use_tryboot(&mut tryboot);
    let mut uboot = Board::new("os-1");
    let image = use_uboot(&mut uboot);
    let mut file = Board::new("os-1");
    use_file_record(&mut file);
    vec![
        ("tryboot", tryboot, Some(node)),
        ("uboot", uboot, Some(image)),
        ("file", file, None),
    ]
}

/// A boot files entry that is not one of the stored role files is refused, by name, on every form
/// and on both install paths (the stream and a file in incoming): the boot partition image is
/// byte for byte what it was, nothing is installed and no work is left.
#[test]
fn a_boot_file_the_boot_partition_does_not_store_is_refused_before_any_write() {
    for (form, mut board, image) in forms() {
        let before = image.as_ref().map(|p| fs::read(p).unwrap());
        let bundle = bundle_with_extra("os-2").pack();
        // The stream.
        let (status, _, _) = board.stream(1, &bundle, 4096);
        assert_eq!(status, Status::INVALID_ARGUMENT, "{form}: stream");
        assert_eq!(board.state().error, refusal_of_extra(), "{form}: stream");
        assert!(!board.layout().installed("os-2"), "{form}: stream");
        assert!(!board.work_left(), "{form}: stream");
        // A file in incoming.
        fs::write(board.incoming().join("os-2.slot.tar"), &bundle).unwrap();
        board.slots.caller(CallerId(3));
        let job = board
            .slots
            .install_file("os-2.slot.tar", false, false)
            .unwrap();
        let (status, _, _) = board.run_file_job(job);
        assert_eq!(status, Status::INVALID_ARGUMENT, "{form}: file");
        assert_eq!(board.state().error, refusal_of_extra(), "{form}: file");
        assert!(!board.layout().installed("os-2"), "{form}: file");
        assert!(!board.work_left(), "{form}: file");
        if let (Some(image), Some(before)) = (image, before) {
            assert_eq!(
                fs::read(image).unwrap(),
                before,
                "{form}: the boot partition was not written"
            );
        }
    }
}

/// The refusal comes with the manifest, before the root image is begun: a stream fed only as far as
/// the root image's header has been refused, and its work directory holds nothing.
#[test]
fn the_refusal_of_a_stored_file_comes_before_the_first_payload_byte() {
    let board = Board::new("os-1");
    let b = bundle_with_extra("os-2");
    let bytes = b.pack();
    let prefix = up_to_header_of(&bytes, "rootfs.erofs");
    let (got, work) = feed_raw(&board, &prefix, false);
    let (status, text) = got.unwrap_err();
    assert_eq!(status, Status::INVALID_ARGUMENT);
    assert_eq!(text, refusal_of_extra());
    assert!(!work.join("rootfs.erofs").exists());
    assert!(!work.join("boot").exists() || fs::read_dir(work.join("boot")).unwrap().count() == 0);
}

/// The five role files, listed and carried, install on every form, and the slot then passes its
/// own boot check where the form has a bay.
#[test]
fn the_five_role_files_install_and_the_slot_passes_its_boot_check() {
    for (form, mut board, image) in forms() {
        board.install_ok("os-2");
        if let Some(image) = image {
            let verdict = crate::boot_guard::verified(
                board.layout(),
                &Ok(image),
                board.slots.record.as_ref(),
                "os-2",
            );
            assert_eq!(verdict, Ok(()), "{form}");
        }
    }
}

/// The reverse: a role file the manifest leaves out is refused by name, before any write.
#[test]
fn a_role_file_the_manifest_leaves_out_is_refused_by_name() {
    let mut board = Board::new("os-1");
    let mut b = Bundle::new("os-2");
    b.boot.retain(|(name, _)| name != "board.dtb");
    let (status, _, _) = board.stream(1, &b.pack(), 4096);
    assert_eq!(status, Status::INVALID_ARGUMENT);
    assert_eq!(
        board.state().error,
        "os-2: boot.files does not list \"board.dtb\", which this board's boot partition stores (it stores: kernel8.img, initramfs.cpio, board.dtb, cmdline.txt, overlays/lemnos-board.dtbo): nothing was written, the default keeps booting"
    );
    assert!(!board.layout().installed("os-2"));
    assert!(!board.work_left());
}

/// What the loader script chooses over the environment the writers left in `node`: the three
/// `lemnos_dtb` values (trial, default, previous), and every variable the script read from it.
fn choice_over(
    node: &Path,
    geometry: &lemnos_slot_store::uboot_env::Geometry,
) -> ([String; 3], std::collections::BTreeSet<String>, Vec<String>) {
    let mut dev = lemnos_slot_store::uboot_env::FileEnv::open(node).unwrap();
    let (_, vars) = lemnos_slot_store::uboot_env::load(&mut dev, geometry)
        .unwrap()
        .unwrap();
    let names = vars.iter().map(|(k, _)| k.clone()).collect();
    let (dtbs, run) = crate::uboot::hush::choose(
        include_str!("../../../packaging/uboot/boot.cmd"),
        vars.into_iter().collect(),
    );
    (dtbs, run.reads, names)
}

/// THE ENVIRONMENT OUR WRITERS PRODUCE, run through the loader script's choice: image built, install
/// os-2, trial, confirm (the default switches, the old default becomes the previous), install os-3.
/// The previous step must be able to run on what the driver wrote, every variable the script reads must
/// be one the writers wrote, and every variable the writers wrote must be one the script reads (the
/// documented list, `uboot::VARIABLES`). The stand-in here is a file-backed environment and an
/// evaluator for the script's conditions: it shares no U-Boot code, but it does read the real
/// environment bytes the real writers made, so a variable nothing writes reads as empty here as it
/// does in U-Boot.
#[test]
fn the_loader_script_reads_only_what_the_writers_wrote_and_its_previous_step_can_run() {
    use std::collections::BTreeSet;
    let (mut board, _boot_node, node, geometry) = uboot_board();
    let mut written = BTreeSet::new();
    let mut reads = BTreeSet::new();
    let mut take = || {
        let (dtbs, read, names) = choice_over(&node, &geometry);
        written.extend(names);
        reads.extend(read);
        dtbs
    };
    // Image built: the default only.
    let dtbs = take();
    assert_eq!(dtbs, ["", "/slots/bay0/board.dtb", ""]);
    // Install os-2 (a free bay), then trial it: the trial starts first, the default is os-1, and
    // there is no previous yet.
    board.install_ok("os-2");
    board.slots.trial("os-2", false, false, false).unwrap();
    let dtbs = take();
    assert_eq!(dtbs, ["/slots/bay1/board.dtb", "/slots/bay0/board.dtb", ""]);
    // The trial boots and is confirmed: os-2 is the default, os-1 the previous.
    board.boot_into("os-2");
    board.slots.confirm().unwrap();
    let dtbs = take();
    assert_eq!(
        dtbs,
        ["", "/slots/bay1/board.dtb", "/slots/bay0/board.dtb"],
        "default failed to load, previous exists and differs: the previous step starts its bay"
    );
    // A third install takes the one free bay, and changes no choice.
    board.install_ok("os-3");
    let dtbs = take();
    assert_eq!(dtbs, ["", "/slots/bay1/board.dtb", "/slots/bay0/board.dtb"]);
    assert_eq!(board.slots.record.slot_bay("os-3").unwrap().0, "slots/bay2");
    let listed: BTreeSet<String> = crate::uboot::VARIABLES.map(String::from).into();
    assert_eq!(
        reads, listed,
        "what the script reads is the documented list"
    );
    assert_eq!(
        written, listed,
        "what the writers wrote is the documented list"
    );
}

// ---- The last install attempt (`info`'s `last_attempt`, docs/slots.md) ----

/// What `info` says about the last attempt.
#[derive(Debug)]
struct Seen {
    state: String,
    slot: String,
    reason: String,
    received: Option<u64>,
    total: Option<u64>,
    boot: String,
    started_ms: Option<u64>,
    ended_ms: Option<u64>,
    text: String,
}

/// Reads `info` as a session that started nothing (caller 9).
fn attempt_seen(slots: &mut SystemSlots) -> Seen {
    slots.caller(CallerId(9));
    let mut out = vec![0u8; 4096];
    let len = slots.info(&mut out).unwrap();
    let json = Json::parse(core::str::from_utf8(&out[..len]).unwrap()).unwrap();
    // Appended after `key_ids`, which is after everything `info` said before.
    let keys: Vec<&str> = json.members().iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(
        keys,
        [
            "require_signed",
            "last_install",
            "default_signature",
            "boot_unverified",
            "key_ids",
            "last_attempt"
        ]
    );
    let a = json.get("last_attempt").unwrap();
    let text = |key: &str| a.get(key).and_then(Json::text).unwrap().to_string();
    let num = |key: &str| a.get(key).and_then(Json::u64);
    Seen {
        state: text("state"),
        slot: text("slot"),
        reason: text("reason"),
        received: num("received"),
        total: num("total"),
        boot: text("boot"),
        started_ms: num("started_ms"),
        ended_ms: num("ended_ms"),
        text: text("text"),
    }
}

fn attempt_of(board: &mut Board) -> Seen {
    attempt_seen(&mut board.slots)
}

#[test]
fn with_no_attempt_ever_the_field_says_none() {
    let dir = TempDir::new("none");
    let mut slots = SystemSlots::new(layout(&dir.0), Box::new(Told::default()), Vec::new());
    slots.boot = "0123abcd-4567-89ab-cdef-000000000000".into();
    let seen = attempt_seen(&mut slots);
    assert_eq!(seen.state, "none");
    // `none` claims only what the daemon knows: nothing since it started in this boot.
    assert_eq!(
        seen.text,
        "last install: none since the service started (boot 0123abcd)"
    );
    assert_eq!(seen.boot, "0123abcd-4567-89ab-cdef-000000000000");
    assert_eq!((seen.slot.as_str(), seen.reason.as_str()), ("", ""));
    assert_eq!((seen.received, seen.total), (None, None));
}

#[test]
fn an_installed_slot_is_recorded_with_its_bytes_and_time_for_a_second_session() {
    let mut board = Board::new("os-1");
    board.slots.boot = "0123abcd-4567-89ab-cdef-000000000000".into();
    let bundle = Bundle::new("os-2").pack();
    let (status, _, _) = board.stream(1, &bundle, 4096);
    assert_eq!(status, Status::OK);
    // The starter's connection ends: the record stays.
    board.slots.caller_gone(CallerId(1));
    let seen = attempt_of(&mut board);
    assert_eq!(seen.state, "installed");
    assert_eq!(seen.slot, "os-2");
    assert_eq!(seen.reason, "");
    assert_eq!(seen.received, Some(bundle.len() as u64));
    assert_eq!(seen.total, Some(bundle.len() as u64));
    assert_eq!(seen.boot, "0123abcd-4567-89ab-cdef-000000000000");
    assert!(seen.ended_ms >= seen.started_ms && seen.started_ms.is_some());
    assert!(
        seen.text.starts_with(&format!(
            "last install: slot os-2 installed ({0} of {0} bytes; boot 0123abcd, +",
            bundle.len()
        )) && seen.text.ends_with(" s)"),
        "{}",
        seen.text
    );
}

#[test]
fn a_refusal_is_recorded_with_the_reason_the_starter_was_given() {
    let mut board = Board::new("os-1");
    security::set_view_for_thread(Some(view_file(&board.dir.0, true)));
    // Signed by a key the board does not hold, with the switch on: refused by the policy.
    let mut bundle = Bundle::new("os-2");
    bundle.signer = SigningKey::from_bytes(&[9; 32]);
    let pack = bundle.pack();
    let (status, _, _) = board.stream(1, &pack, 4096);
    assert_eq!(status, Status::NOT_PERMITTED);
    let given = board.state().error;
    let seen = attempt_of(&mut board);
    assert_eq!(seen.state, "refused");
    assert_eq!(seen.slot, "os-2");
    assert_eq!(seen.reason, given);
    assert!(seen.reason.contains("does not trust"), "{}", seen.reason);
    assert!(seen.received < seen.total, "{seen:?}");
    assert_eq!(seen.total, Some(pack.len() as u64));
    assert!(
        seen.text
            .starts_with(&format!("last install: slot os-2 refused: {}", seen.reason)),
        "{}",
        seen.text
    );
    security::set_view_for_thread(None);

    // For the running slot: refused before its image comes.
    let (status, _, _) = board.stream(1, &Bundle::new("os-1").pack(), 4096);
    assert_eq!(status, Status::BUSY);
    let seen = attempt_of(&mut board);
    assert_eq!(
        (seen.state.as_str(), seen.slot.as_str()),
        ("refused", "os-1")
    );
    assert_eq!(seen.reason, board.state().error);
    assert!(seen.reason.contains("running slot"));

    // A boot file the manifest doesn't list: refused once everything came.
    let mut extra = Bundle::new("os-2");
    extra.extra.push(("evil.dtbo".into(), b"x".to_vec()));
    let pack = extra.pack();
    let (status, _, _) = board.stream(1, &pack, 4096);
    assert_eq!(status, Status::INVALID_ARGUMENT);
    let seen = attempt_of(&mut board);
    assert_eq!(
        (seen.state.as_str(), seen.slot.as_str()),
        ("refused", "os-2")
    );
    assert_eq!(seen.reason, board.state().error);
    assert!(seen.reason.contains("evil.dtbo"));
    // Refused as its header came: the bytes taken are those before it.
    assert!(
        seen.received > Some(0) && seen.received < Some(pack.len() as u64),
        "{seen:?}"
    );
}

#[test]
fn a_refusal_before_the_install_starts_is_recorded_too() {
    let dir = TempDir::new("nokeys-attempt");
    let mut slots = SystemSlots::new(layout(&dir.0), Box::new(Told::default()), Vec::new());
    security::set_view_for_thread(Some(view_file(&dir.0, true)));
    slots.caller(CallerId(1));
    assert_eq!(slots.install(10, false, false), Err(Status::NOT_PERMITTED));
    security::set_view_for_thread(None);
    let seen = attempt_seen(&mut slots);
    assert_eq!(seen.state, "refused");
    assert_eq!(seen.reason, crate::attempt::NO_KEYS);
    assert_eq!((seen.received, seen.total), (Some(0), Some(10)));
    // No room: the reason is the board's.
    let mut board = Board::new("os-1");
    board.slots.reserve = u64::MAX / 2;
    board.slots.caller(CallerId(1));
    assert_eq!(
        board.slots.install(10, false, false),
        Err(crate::install::NO_SPACE)
    );
    let seen = attempt_of(&mut board);
    assert_eq!(seen.state, "refused");
    assert_eq!(seen.reason, board.state().error);
    assert!(seen.reason.contains("no room"), "{}", seen.reason);
    // A second install turned away as busy is no attempt: it leaves the record alone.
    board.slots.reserve = 0;
    let bundle = Bundle::new("os-2").pack();
    board.slots.caller(CallerId(1));
    board
        .slots
        .install(bundle.len() as u64, false, false)
        .unwrap();
    assert_eq!(board.slots.install(10, false, false), Err(Status::BUSY));
    assert_eq!(attempt_of(&mut board).state, "running");
}

#[test]
fn a_failure_in_the_work_is_recorded_as_failed_with_its_reason() {
    let mut board = Board::new("os-1");
    // The image isn't what the manifest says: found at the end.
    let mut wrong = Bundle::new("os-2");
    wrong.manifest_image_sha = Some(sha(b"something else"));
    let pack = wrong.pack();
    let (status, _, _) = board.stream(1, &pack, 4096);
    assert_eq!(status, Status::INVALID_ARGUMENT);
    let seen = attempt_of(&mut board);
    assert_eq!(
        (seen.state.as_str(), seen.slot.as_str()),
        ("failed", "os-2")
    );
    assert_eq!(seen.reason, board.state().error);
    assert!(seen.reason.contains("sha256"), "{}", seen.reason);
    assert_eq!(seen.received, Some(pack.len() as u64));
    assert!(
        seen.text.contains("failed: rootfs.erofs doesn't match"),
        "{}",
        seen.text
    );
    // Cut short: every announced byte came but the bundle is not whole.
    let mut short = Bundle::new("os-2").pack();
    short.truncate(short.len() - 1024);
    let (status, _, _) = board.stream(1, &short, 4096);
    assert_eq!(status, Status::INVALID_ARGUMENT);
    let seen = attempt_of(&mut board);
    assert_eq!(seen.state, "failed");
    assert_eq!(seen.reason, board.state().error);
    assert!(
        seen.reason.contains("ends in the middle"),
        "{}",
        seen.reason
    );
}

#[test]
fn a_cancel_by_the_starter_and_one_by_its_connection_ending_are_told_apart() {
    let mut board = Board::new("os-1");
    let bundle = Bundle::new("os-2").pack();
    board.slots.caller(CallerId(1));
    let job = board
        .slots
        .install(bundle.len() as u64, false, false)
        .unwrap();
    board.slots.write(0, &bundle[..3000]).unwrap();
    board.slots.job_cancel(job);
    let seen = attempt_of(&mut board);
    assert_eq!(
        (seen.state.as_str(), seen.slot.as_str()),
        ("cancelled", "os-2")
    );
    assert_eq!(seen.reason, crate::attempt::CANCELLED);
    assert_eq!(seen.received, Some(3000));
    assert_eq!(seen.total, Some(bundle.len() as u64));
    // Another session going away changes nothing.
    board.slots.caller_gone(CallerId(7));
    assert_eq!(attempt_of(&mut board).reason, crate::attempt::CANCELLED);
    let mut out = [0u8; 8];
    board.slots.job(job, &mut Reply::new(&mut out));

    // The starter's connection ends: the host cancels its job, then tells the driver it went.
    board.slots.caller(CallerId(2));
    let job = board
        .slots
        .install(bundle.len() as u64, false, false)
        .unwrap();
    board.slots.write(0, &bundle[..2000]).unwrap();
    board.slots.job_cancel(job);
    board.slots.caller_gone(CallerId(2));
    let seen = attempt_of(&mut board);
    assert_eq!(seen.state, "cancelled");
    assert_eq!(seen.reason, crate::attempt::GONE);
    assert_eq!(seen.received, Some(2000));
    assert!(
        seen.text
            .contains("cancelled: the caller that started it went away (2000 of "),
        "{}",
        seen.text
    );
    // The record outlives the job: the job's end is collected and it is still there.
    board.slots.job(job, &mut Reply::new(&mut out));
    assert_eq!(attempt_of(&mut board).reason, crate::attempt::GONE);
}

#[test]
fn a_session_that_did_not_start_it_sees_a_running_install_as_the_job_shows_it() {
    let mut board = Board::new("os-1");
    let bundle = Bundle::new("os-2").pack();
    board.slots.caller(CallerId(1));
    let job = board
        .slots
        .install(bundle.len() as u64, false, false)
        .unwrap();
    let seen = attempt_of(&mut board);
    assert_eq!(seen.state, "running");
    assert_eq!(
        (seen.received, seen.total),
        (Some(0), Some(bundle.len() as u64))
    );
    assert_eq!(seen.slot, "");
    assert_eq!(seen.ended_ms, None);
    board.slots.caller(CallerId(1));
    board.slots.write(0, &bundle[..3000]).unwrap();
    let seen = attempt_of(&mut board);
    assert_eq!(seen.state, "running");
    // The job's own figures, which `list` and `describe` show.
    let Some(JobState::Running { done, total }) = board.poll(job) else {
        panic!("not running")
    };
    assert_eq!((seen.received, seen.total), (Some(done), Some(total)));
    assert_eq!(seen.slot, "os-2");
    assert!(
        seen.text.contains("slot os-2 running (3000 of "),
        "{}",
        seen.text
    );
    board.slots.caller(CallerId(1));
    board.slots.write(3000, &bundle[3000..]).unwrap();
    assert_eq!(attempt_of(&mut board).state, "installed");
}

#[test]
fn a_new_attempt_replaces_the_record_when_it_starts() {
    let mut board = Board::new("os-1");
    let mut wrong = Bundle::new("os-2");
    wrong.manifest_image_sha = Some(sha(b"x"));
    board.stream(1, &wrong.pack(), 4096);
    assert_eq!(attempt_of(&mut board).state, "failed");
    let bundle = Bundle::new("os-3").pack();
    board.slots.caller(CallerId(1));
    board
        .slots
        .install(bundle.len() as u64, false, false)
        .unwrap();
    let seen = attempt_of(&mut board);
    assert_eq!(seen.state, "running");
    assert_eq!(seen.reason, "", "the earlier reason is gone");
    board.slots.caller(CallerId(1));
    board.slots.write(0, &bundle).unwrap();
    let seen = attempt_of(&mut board);
    assert_eq!(
        (seen.state.as_str(), seen.slot.as_str()),
        ("installed", "os-3")
    );
}

#[test]
fn an_install_from_a_file_is_recorded_as_a_streamed_one_is() {
    let mut board = Board::new("os-1");
    let incoming = board.incoming();
    let bundle = big_bundle("os-2");
    fs::write(incoming.join("a"), &bundle).unwrap();
    board.slots.caller(CallerId(3));
    let job = board.slots.install_file("a", false, false).unwrap();
    let (status, _, _) = board.run_file_job(job);
    assert_eq!(status, Status::OK);
    let seen = attempt_of(&mut board);
    assert_eq!(
        (seen.state.as_str(), seen.slot.as_str()),
        ("installed", "os-2")
    );
    assert_eq!(seen.received, Some(bundle.len() as u64));
    // Failed: the file ends before its tar does.
    let bundle = big_bundle("os-3");
    fs::write(incoming.join("b"), &bundle[..bundle.len() / 2]).unwrap();
    let job = board.slots.install_file("b", false, false).unwrap();
    let (status, _, _) = board.run_file_job(job);
    assert_eq!(status, Status::INVALID_ARGUMENT);
    let seen = attempt_of(&mut board);
    assert_eq!(seen.state, "failed");
    assert_eq!(seen.reason, board.state().error);
    // Cancelled by its starter's connection ending.
    fs::write(incoming.join("c"), &bundle).unwrap();
    board.slots.caller(CallerId(3));
    let job = board.slots.install_file("c", false, false).unwrap();
    board.slots.tick(0);
    board.slots.job_cancel(job);
    board.slots.caller_gone(CallerId(3));
    let seen = attempt_of(&mut board);
    assert_eq!(
        (seen.state.as_str(), seen.slot.as_str()),
        ("cancelled", "os-3")
    );
    assert_eq!(seen.reason, crate::attempt::GONE);
    assert!(
        seen.received > Some(0) && seen.received < seen.total,
        "{seen:?}"
    );
}

#[test]
fn an_install_a_restart_cut_short_is_known_after_it_and_nothing_else_is() {
    let mut board = Board::new("os-1");
    let bundle = Bundle::new("os-2").pack();
    board.slots.caller(CallerId(1));
    let job = board
        .slots
        .install(bundle.len() as u64, false, false)
        .unwrap();
    board.slots.write(0, &bundle[..3000]).unwrap();
    let layout = board.layout().clone();
    // The service stops and a new driver starts: the state record says an install was cut short.
    let mut again = SystemSlots::new(layout.clone(), Box::new(Told::default()), Vec::new());
    let seen = attempt_seen(&mut again);
    assert_eq!(seen.state, "interrupted");
    assert_eq!(seen.reason, crate::attempt::INTERRUPTED);
    assert_eq!(seen.reason, State::load(&layout).error);
    // The slot, the bytes and the time were never kept.
    assert_eq!(
        (seen.slot.as_str(), seen.received, seen.total),
        ("", None, None)
    );
    assert_eq!(
        (seen.started_ms, seen.ended_ms, seen.boot.as_str()),
        (None, None, "")
    );
    assert!(
        seen.text
            .contains("interrupted: an install was interrupted"),
        "{}",
        seen.text
    );
    // Another restart with the record untouched says the same.
    let mut third = SystemSlots::new(layout.clone(), Box::new(Told::default()), Vec::new());
    assert_eq!(attempt_seen(&mut third).state, "interrupted");
    // (The old driver's job is ended here, as the host ends it.)
    board.slots.job_cancel(job);
    board.slots.job(job, &mut Reply::new(&mut [0u8; 8]));
    // A failed attempt, a refused one and an installed one are not kept: after a restart, none.
    let mut wrong = Bundle::new("os-2");
    wrong.manifest_image_sha = Some(sha(b"x"));
    board.stream(1, &wrong.pack(), 4096);
    assert_eq!(attempt_of(&mut board).state, "failed");
    let mut fresh = SystemSlots::new(layout, Box::new(Told::default()), Vec::new());
    fresh.boot = "feedbeef-0000-0000-0000-000000000000".into();
    let seen = attempt_seen(&mut fresh);
    assert_eq!(seen.state, "none");
    assert_eq!(seen.boot, "feedbeef-0000-0000-0000-000000000000");
    assert!(
        seen.text
            .ends_with("since the service started (boot feedbeef)"),
        "{}",
        seen.text
    );
}

#[test]
fn a_reason_past_the_bound_is_cut_with_the_marker_and_the_state_keeps_it_whole() {
    let mut board = Board::new("os-1");
    let mut bundle = Bundle::new("os-2");
    bundle.id = "X".repeat(400);
    let (status, _, _) = board.stream(1, &bundle.pack(), 4096);
    assert_ne!(status, Status::OK);
    let whole = board.state().error;
    let seen = attempt_of(&mut board);
    assert!(whole.len() > crate::attempt::REASON_MAX, "{whole}");
    assert!(seen.reason.len() <= crate::attempt::REASON_MAX);
    assert!(seen.reason.ends_with(crate::attempt::MARKER));
    let keep = seen.reason.len() - crate::attempt::MARKER.len();
    assert_eq!(&seen.reason[..keep], &whole[..keep]);
    // The slot id is bounded the same way.
    assert!(seen.slot.len() <= crate::attempt::SLOT_MAX);
}
