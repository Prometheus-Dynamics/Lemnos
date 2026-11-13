//! lemnosd's boot-file check over real files in a temporary directory: a manifest per slot, the
//! slots' boot files under the boot directory, the state, and the boot record (`default.txt`).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use lemnos_slot_store::bootcheck::sha256_hex;
use lemnos_slot_store::manifest::{BLOCK, tree_bytes};
use lemnos_slot_store::store::{Layout, Meta, State};

use crate::boot::{Check, Restarter, Tryboot};
use crate::boot_guard;

struct Dir(PathBuf);

impl Dir {
    fn new(label: &str) -> Self {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "lemnos-boot-guard-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// The slot's boot files: a kernel, a DTB, an overlay, the command line and an initramfs.
fn boot_files(id: &str) -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("kernel8.img", format!("kernel of {id}").into_bytes()),
        ("bcm2712-rpi-5-b.dtb", format!("dtb of {id}").into_bytes()),
        (
            "overlays/vc4-kms.dtbo",
            format!("overlay of {id}").into_bytes(),
        ),
        ("initramfs", format!("initramfs of {id}").into_bytes()),
        (
            "cmdline.txt",
            format!(
                "console=tty1 os.slot={id} os.roothash={} panic=5\n",
                hash(id)
            )
            .into_bytes(),
        ),
    ]
}

fn hash(id: &str) -> String {
    let digit = id.chars().last().unwrap();
    std::iter::repeat_n(digit, 64).collect()
}

/// A slot installed on the board: its manifest (listing its boot files by hash) and its boot
/// directory.
fn install(layout: &Layout, id: &str) {
    let files = boot_files(id);
    let listed: Vec<String> = files
        .iter()
        .map(|(name, bytes)| format!("\"{name}\":\"{}\"", sha256_hex(bytes)))
        .collect();
    let text = format!(
        "{{\"schema\":3,\"id\":\"{id}\",\"product\":\"an-os\",\"version\":\"1.0\",\"board\":\"b\",\
         \"image\":{{\"size\":{},\"sha256\":\"{}\"}},\
         \"verity\":{{\"root_hash\":\"{}\",\"salt\":\"{}\",\"data_blocks\":1,\"hash_offset\":{BLOCK}}},\
         \"boot\":{{\"files\":{{{}}}}}}}",
        BLOCK + tree_bytes(1),
        hash("x0"),
        hash(id),
        hash("x0"),
        listed.join(",")
    );
    fs::create_dir_all(layout.images()).unwrap();
    fs::write(layout.manifest(id), text).unwrap();
    for (name, bytes) in files {
        let path = layout.boot_dir(id).join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
}

/// Two installed slots, `os-2` the default and `os-1` the previous, and the record over them.
struct Board {
    _dir: Dir,
    layout: Layout,
    record: Tryboot,
}

impl Board {
    fn new(label: &str) -> Self {
        let dir = Dir::new(label);
        let layout = Layout::new(dir.0.join("data"), dir.0.join("boot"));
        install(&layout, "os-1");
        install(&layout, "os-2");
        fs::create_dir_all(layout.state_dir()).unwrap();
        let state = State {
            default: "os-2".into(),
            previous: "os-1".into(),
            state: "confirmed".into(),
            ..State::default()
        };
        state.save(&layout).unwrap();
        fs::create_dir_all(&layout.boot).unwrap();
        fs::write(layout.boot.join("default.txt"), "os_prefix=slots/os-2/\n").unwrap();
        let record = Tryboot {
            boot: layout.boot.clone(),
            restarter: Restarter {
                systemctl: "/nonexistent/systemctl".into(),
                check: None,
            },
        };
        Self {
            _dir: dir,
            layout,
            record,
        }
    }

    /// Appends one argument to `id`'s command line on the boot partition, as an in-place edit did.
    fn edit(&self, id: &str) {
        let path = self.layout.boot_dir(id).join("cmdline.txt");
        let text = fs::read_to_string(&path).unwrap();
        fs::write(path, text.replace("panic=5", "panic=5 mem=128M")).unwrap();
    }

    fn state(&self) -> State {
        State::load(&self.layout)
    }

    fn default_txt(&self) -> String {
        fs::read_to_string(self.layout.boot.join("default.txt")).unwrap_or_default()
    }

    fn check(&mut self, restart: bool) -> Result<Option<String>, String> {
        boot_guard::check(&self.layout, &mut self.record, restart)
    }
}

#[test]
fn an_untouched_default_passes_before_a_restart_and_nothing_changes() {
    let mut board = Board::new("untouched");
    assert_eq!(board.check(true), Ok(None));
    assert_eq!(board.state().default, "os-2");
    assert_eq!(board.default_txt(), "os_prefix=slots/os-2/\n");
}

#[test]
fn lemnosd_repoints_default_to_verified_previous_before_restart() {
    let mut board = Board::new("repoint");
    board.edit("os-2");
    let note = board
        .check(true)
        .unwrap()
        .expect("the default was repointed");
    assert!(note.contains("os-1 is the default now"), "{note}");
    let state = board.state();
    assert_eq!(
        (state.default.as_str(), state.previous.as_str()),
        ("os-1", "os-2")
    );
    assert_eq!(board.default_txt(), "os_prefix=slots/os-1/\n");
    assert_eq!(Meta::load(&board.layout, "os-2").get("FAILED"), "yes");
    assert!(
        Meta::load(&board.layout, "os-2")
            .get("FAIL_REASON")
            .contains("cmdline.txt")
    );
}

#[test]
fn default_and_previous_both_bad_stay_and_report() {
    // Neither slot matches: the restart is refused, nothing is repointed, and the reason names both.
    let mut board = Board::new("both-bad");
    board.edit("os-2");
    board.edit("os-1");
    let why = board.check(true).unwrap_err();
    assert!(why.contains("os-2 doesn't match its manifest"), "{why}");
    assert!(why.contains("the restart is refused"), "{why}");
    assert_eq!(board.state().default, "os-2");
    assert_eq!(board.default_txt(), "os_prefix=slots/os-2/\n");
    assert_eq!(Meta::load(&board.layout, "os-2").get("FAILED"), "");
}

#[test]
fn lemnosd_slow_period_catches_an_edit_before_a_restart() {
    // The edit is caught on the slow period, so the restart that follows boots the verified slot.
    let mut board = Board::new("slow");
    board.edit("os-2");
    let note = board.check(false).unwrap();
    assert!(note.is_some());
    assert_eq!(board.state().default, "os-1");
    assert_eq!(board.check(true), Ok(None));
}

#[test]
fn a_slow_period_that_finds_both_bad_only_reports() {
    let mut board = Board::new("slow-both");
    board.edit("os-2");
    board.edit("os-1");
    let why = board.check(false).unwrap_err();
    assert!(!why.contains("refused"), "{why}");
    assert_eq!(board.state().default, "os-2");
}

#[test]
fn a_refused_check_stops_the_restart_before_systemctl_runs() {
    let ran = Arc::new(AtomicBool::new(false));
    let seen = ran.clone();
    let check = Check(Arc::new(move |restart| {
        assert!(restart);
        seen.store(true, Ordering::SeqCst);
        Err("the default doesn't match".to_string())
    }));
    let restarter = Restarter {
        systemctl: Path::new("/nonexistent/systemctl").into(),
        check: Some(check),
    };
    let err = restarter.run(None).unwrap_err();
    assert!(err.to_string().contains("doesn't match"), "{err}");
    assert!(ran.load(Ordering::SeqCst));
}
