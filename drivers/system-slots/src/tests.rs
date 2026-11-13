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
use crate::{SystemSlots, TrustedKey};
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
                ("cmdline.txt".into(), b"console=tty1  rootwait\n".to_vec()),
                ("initramfs".into(), b"initramfs".to_vec()),
                ("overlays/README".into(), b"readme".to_vec()),
            ],
            extra: Vec::new(),
            manifest_image_sha: None,
            signer: key(),
            sign: true,
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

    /// The bundle with `manifest` as its slot.json, signed as it is.
    fn pack_with(&self, manifest: &str) -> Vec<u8> {
        let manifest = manifest.to_string();
        let signature = self.signer.sign(manifest.as_bytes()).to_bytes();
        let mut files: Vec<(String, Vec<u8>)> = vec![("slot.json".into(), manifest.into_bytes())];
        if self.sign {
            files.push(("slot.sig".into(), signature.to_vec()));
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
    // Its boot files are kept on the data partition, its command line naming the slot, its
    // root hash and the data partition; nothing is on the boot partition until it is tried.
    let kept = crate::boot::kept(&layout, "os-2").unwrap();
    let cmdline = kept.iter().find(|(_, n)| n == "cmdline.txt").unwrap();
    assert_eq!(
        fs::read_to_string(layout.bootfiles().join(&cmdline.0)).unwrap(),
        format!(
            "console=tty1 rootwait board.slot=os-2 board.roothash={ROOT_HASH} board.data=PARTUUID=1234abcd-02 panic=5\n"
        )
    );
    assert!(kept.iter().any(|(_, n)| n == "slot.json"));
    assert!(!layout.boot_dir("os-2").exists());
    assert!(!board.work_left());
}

#[test]
fn with_require_signed_a_bundle_not_signed_by_a_trusted_key_is_refused_and_leaves_nothing() {
    let mut board = Board::new("os-1");
    board.slots.require_signed = true;
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
    assert!(state.error.contains("slot.sig"), "{}", state.error);
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
        // Its directory went onto the boot partition before the trial pointer named it.
        assert!(board.layout().boot_dir(id).join("kernel8.img").is_file());
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
    // The default's files are at the boot partition's root too.
    assert_eq!(
        fs::read(board.layout().boot.join("kernel8.img")).unwrap(),
        b"kernel of os-3"
    );
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

#[test]
fn a_board_that_requires_signing_and_trusts_no_key_installs_nothing() {
    let dir = TempDir::new("nokeys");
    let mut slots = SystemSlots::new(layout(&dir.0), Box::new(Told::default()), Vec::new());
    slots.require_signed = true;
    assert_eq!(slots.install(10, false, false), Err(Status::NOT_PERMITTED));
    // Off, the default: it installs, and the slot shows as unverified.
    slots.require_signed = false;
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

[[devices]]
id = "slots"
driver = "system-slots"
config = {{ data = "{data}", boot = "{boot}", keys = "{keys}", systemctl = "{ctl}", partuuid = "abcd-02", boot_args = "board", reserve_mb = 0 }}

[[devices]]
id = "reboot"
driver = "system-reboot"
config = {{ systemctl = "{ctl}" }}
"#,
        data = layout.data.display(),
        boot = layout.boot.display(),
        keys = keys.display(),
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
        fs::read_to_string(layout.boot.join("trial.txt")).unwrap(),
        "os_prefix=slots/os-2/\n"
    );
    let reboot = host.device("reboot").unwrap().index;
    host.call(reboot, method(1), &[], &mut []).unwrap();
    assert_eq!(fs::read_to_string(&calls).unwrap(), "reboot\nreboot\n");
}

/// With the `uboot` record, an install writes the slot's loader configuration from its own
/// files and command line, and a trial arms the record.
#[test]
fn with_the_uboot_record_an_install_writes_the_slots_extlinux_and_a_trial_arms_it() {
    let mut board = Board::new("os-1");
    let boot = board.layout().boot.clone();
    board.slots.record = Box::new(crate::uboot::Uboot {
        boot: boot.clone(),
        restarter: crate::boot::Restarter {
            systemctl: "/bin/true".into(),
        },
        volume: None,
    });
    board.install_ok("os-2");
    let kept = fs::read_to_string(board.layout().boot_list("os-2")).unwrap();
    assert!(kept.contains("  extlinux/extlinux.conf\n"), "{kept}");
    board.slots.trial("os-2", false, false, false).unwrap();
    let conf =
        fs::read_to_string(board.layout().boot_dir("os-2").join(crate::uboot::EXTLINUX)).unwrap();
    assert!(
        conf.contains("  kernel /slots/os-2/kernel8.img\n"),
        "{conf}"
    );
    assert!(conf.contains("  initrd /slots/os-2/initramfs\n"), "{conf}");
    assert!(
        conf.contains(&format!(
            "  append console=tty1 rootwait board.slot=os-2 board.roothash={ROOT_HASH} board.data=PARTUUID=1234abcd-02 panic=5\n"
        )),
        "{conf}"
    );
    assert_eq!(
        fs::read_to_string(boot.join(crate::uboot::RECORD_FILE)).unwrap(),
        "default=\ntrial=os-2\ntrial_attempts=3\n"
    );
    board.boot_into("os-2");
    board.slots.confirm().unwrap();
    let record = fs::read_to_string(boot.join(crate::uboot::RECORD_FILE)).unwrap();
    assert!(record.starts_with("default=os-2\n"), "{record}");
    assert_eq!(
        fs::read(boot.join("extlinux/extlinux.conf")).unwrap(),
        conf.into_bytes()
    );
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
    // Its command line finds the data volume, not a partition.
    let kept = crate::boot::kept(board.layout(), "os-2").unwrap();
    let cmdline = kept.iter().find(|(_, n)| n == "cmdline.txt").unwrap();
    let cmdline = fs::read_to_string(board.layout().bootfiles().join(&cmdline.0)).unwrap();
    assert!(cmdline.contains(" board.data=UBI=ubi0:data "), "{cmdline}");
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
        crate::boot::read_reason(&board.layout().boot)
            .unwrap()
            .contains("broken")
    );
}

#[test]
fn once_the_reason_can_be_written_the_deadline_restart_goes_and_nothing_is_reported() {
    let mut board = trial_board();
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
fn a_reason_the_state_never_held_is_read_from_the_boot_partition_at_the_next_start() {
    let mut board = trial_board();
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
        crate::boot::read_reason(&board.layout().boot).is_none(),
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
