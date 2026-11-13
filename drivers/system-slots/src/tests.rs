//! The driver over temporary directories: a data partition and a boot partition that are only
//! folders, and a boot record that writes down what it was told.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use ed25519_dalek::{Signer, SigningKey};
use lemnos_contract_system_slots::Slots;
use lemnos_core::{CallerId, JobId, JobState, Lifecycle, Reply, Status};
use sha2::{Digest, Sha256};

use crate::boot::{BootRecord, hex};
use crate::json::Json;
use crate::store::{Layout, Meta, State};
use crate::tar::tests::archive;
use crate::{SystemSlots, TrustedKey};

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

/// What the boot loader was told, in order.
#[derive(Clone, Default)]
struct Told(Arc<Mutex<Vec<String>>>);

impl Told {
    fn take(&self) -> Vec<String> {
        core::mem::take(&mut *self.0.lock().unwrap())
    }
}

impl BootRecord for Told {
    fn set_default(&mut self, id: &str) -> std::io::Result<()> {
        self.0.lock().unwrap().push(format!("default {id}"));
        Ok(())
    }
    fn set_trial(&mut self, id: &str) -> std::io::Result<()> {
        self.0.lock().unwrap().push(format!("trial {id}"));
        Ok(())
    }
    fn restart(&mut self, trial: bool) -> std::io::Result<()> {
        self.0
            .lock()
            .unwrap()
            .push(if trial { "restart trial" } else { "restart" }.into());
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
    image: Vec<u8>,
    boot: Vec<(String, Vec<u8>)>,
    /// Files in boot/ the manifest doesn't list.
    extra: Vec<(String, Vec<u8>)>,
    manifest_image_sha: Option<String>,
    signer: SigningKey,
    sign: bool,
}

impl Bundle {
    fn new(id: &str) -> Self {
        Self {
            id: id.into(),
            image: format!("erofs image of {id}").repeat(300).into_bytes(),
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
            "{{\"schema\":1,\"id\":\"{}\",\"family\":\"os\",\"version\":\"1.0\",\"variant\":\"v\",\
             \"kernel\":\"6.12\",\"display_name\":\"An OS\",\
             \"verity\":{{\"root_hash\":\"{ROOT_HASH}\",\"data_block_size\":4096}},\
             \"image\":{{\"size\":{},\"sha256\":\"{}\"}},\"boot\":{{\"files\":{{{}}}}}}}",
            self.id,
            self.image.len(),
            self.manifest_image_sha
                .clone()
                .unwrap_or_else(|| sha(&self.image)),
            files.join(",")
        )
    }

    fn pack(&self) -> Vec<u8> {
        let manifest = self.manifest();
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
        let dir = TempDir::new("board");
        let layout = layout(&dir.0);
        fs::create_dir_all(layout.images()).unwrap();
        fs::create_dir_all(layout.boot.join("slots")).unwrap();
        let told = Told::default();
        let mut board = Self {
            slots: SystemSlots::new(
                layout.clone(),
                Box::new(told.clone()),
                vec![TrustedKey {
                    key: key().verifying_key(),
                    owner: "the build key".into(),
                }],
            ),
            dir,
            told,
        };
        board.slots.partuuid = "1234abcd-02".into();
        board.slots.boot_args = "board".into();
        board.slots.reserve = 0;
        // Put in place as a first image would be, with nothing running yet.
        board.slots.running = Some(String::new());
        board.install_ok(first);
        board.boot_into(first);
        board.slots.confirm().unwrap();
        board.told.take();
        board
    }

    fn layout(&self) -> &Layout {
        &self.slots.layout
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
    Layout {
        data: dir.join("data"),
        boot: dir.join("boot"),
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
            [format!("trial {id}"), "restart trial".to_string()]
        );
        // Its directory went onto the boot partition before the trial pointer named it.
        assert!(board.layout().boot_dir(id).join("kernel8.img").is_file());
        assert_eq!(board.state().trial, id);
        board.boot_into(id);
        board.slots.confirm().unwrap();
        assert_eq!(board.told.take(), [format!("default {id}")]);
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
    assert!(board.told.take().is_empty());
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
    assert_eq!(
        fs::read_to_string(&calls).unwrap(),
        "reboot --reboot-argument=0 tryboot\nreboot\n"
    );
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
        "default=\ntrial=os-2\ntrial_armed=1\n"
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
    bundle.image = (0..3 * crate::install::READ_EVERY_TICK + 77)
        .map(|i| (i % 251) as u8)
        .collect();
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
