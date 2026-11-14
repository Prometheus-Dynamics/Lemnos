//! lemnosd's boot-file check over real files in a temporary directory: a manifest per slot, the
//! slots' boot files under the boot directory, the state, and the boot record (`default.txt`).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use lemnos_slot_store::bootcheck::sha256_hex;
use lemnos_slot_store::manifest::{BLOCK, tree_bytes};
use lemnos_slot_store::store::{Layout, Meta, State};

use crate::boot::{BootRecord, Check, Restarter, Tryboot};
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

fn hash(id: &str) -> String {
    let digit = id.chars().last().unwrap();
    std::iter::repeat_n(digit, 64).collect()
}

/// A slot installed on the board: its manifest, which lists its bay files (the roles' paths) by the
/// hash of their payloads. The payloads go into a bay through the record ([`Board::new`]).
fn install(layout: &Layout, id: &str) {
    let payloads = crate::bay_fixture::payloads(id);
    let listed: Vec<String> = lemnos_slot_store::bays::ROLES
        .iter()
        .zip(payloads.iter())
        .map(|(role, bytes)| format!("\"{}\":\"{}\"", role.path, sha256_hex(bytes)))
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
}

/// Two installed slots, `os-2` the default and `os-1` the previous, and the record over them.
struct Board {
    _dir: Dir,
    layout: Layout,
    record: Tryboot,
    node: std::path::PathBuf,
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
        let node = layout.boot.join("boot.img");
        // The image builder's image with os-1 in bay 0; os-2 goes into the next bay and becomes the
        // default, so os-1 is the previous (the pointers say so).
        fs::write(&node, crate::bay_fixture::tryboot_image("os-1")).unwrap();
        let mut record = Tryboot {
            boot: layout.boot.clone(),
            node: Ok(node.clone()),
            restarter: Restarter {
                systemctl: "/nonexistent/systemctl".into(),
                check: None,
            },
        };
        let p = crate::bay_fixture::payloads("os-2");
        record
            .install("os-2", &crate::bay_fixture::refs(&p))
            .unwrap();
        record.set_default("os-2").unwrap();
        Self {
            _dir: dir,
            layout,
            record,
            node,
        }
    }

    /// Appends one argument to `id`'s command line in its bay, in place (one sector written over the
    /// start of the file). The bay's record keeps the stored length and hash, so the file no longer
    /// matches what was installed.
    fn edit(&self, id: &str) {
        use lemnos_slot_store::twocopy::{FileSectors, Sectors};
        let mut raw = lemnos_slot_store::boot_raw::Boot::open(&self.node).unwrap();
        let (dir, _) = lemnos_slot_store::boot_raw::BootRaw::slot_bay(&mut raw, id).unwrap();
        let handle = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.node)
            .unwrap();
        let mut dev = FileSectors::new(handle, 0);
        let found =
            lemnos_slot_store::bootfat::locate_path(&mut dev, &format!("{dir}/cmdline.txt"), 4096)
                .unwrap();
        let text = String::from_utf8(crate::bay_fixture::payloads(id)[3].clone()).unwrap();
        let mut sector = [0u8; 512];
        let edited = text.replace("panic=5", "panic=5 mem=128M");
        sector[..edited.len()].copy_from_slice(edited.as_bytes());
        dev.write(found.first_lba, &sector).unwrap();
    }

    fn state(&self) -> State {
        State::load(&self.layout)
    }

    /// The slot the default pointer names, read from the boot partition's node.
    fn default_txt(&self) -> String {
        let mut raw = lemnos_slot_store::boot_raw::Boot::open(&self.node).unwrap();
        let which = lemnos_slot_store::boot_raw::Pointer::Default;
        lemnos_slot_store::boot_raw::BootRaw::pointer(&mut raw, which)
            .unwrap()
            .unwrap_or_default()
    }

    fn check(&mut self, restart: bool) -> Result<Option<String>, String> {
        // The files are read raw from the image, as lemnosd reads the boot partition's node.
        let node = self.node.clone();
        boot_guard::check_with(&self.layout, &mut self.record, restart, &mut |path| {
            boot_guard::read_raw(&Ok(node.clone()), path)
        })
    }
}

#[test]
fn an_untouched_default_passes_before_a_restart_and_nothing_changes() {
    let mut board = Board::new("untouched");
    assert_eq!(board.check(true), Ok(None));
    assert_eq!(board.state().default, "os-2");
    assert_eq!(board.default_txt(), "os-2");
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
    assert_eq!(board.default_txt(), "os-1");
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
    assert_eq!(board.default_txt(), "os-2");
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

/// One subdirectory of a test image: its name, its cluster, and its files.
type TestDir<'a> = (&'a [u8; 11], u16, Vec<(&'a [u8; 11], Vec<u8>, Vec<u16>)>);

/// A FAT16 boot partition image with fragmented files: each file's clusters are given in order and
/// need not be contiguous, and the chain runs through them. The boot sector, the FAT's reserved
/// entries and the root directory's layout come from `fatimage::fat16`, so only the chains and the
/// data are this function's. `dirs` are subdirectories (name, its cluster, its entries).
fn fragmented_image(files: &[(&[u8; 11], Vec<u8>, Vec<u16>)], dirs: &[TestDir<'_>]) -> Vec<u8> {
    const DATA_START: usize = 1 + 64 + 32;
    let mut image = lemnos_slot_store::fatimage::fat16(&[]);
    let fat = 512;
    let root = (1 + 64) * 512;
    let mut root_at = root;
    // Chain entries and data for one file (at `root_at` or in a directory's `at`).
    let put = |image: &mut Vec<u8>, bytes: &[u8], clusters: &[u16]| {
        for (i, &c) in clusters.iter().enumerate() {
            let value = clusters.get(i + 1).copied().unwrap_or(0xFFFF);
            let at = fat + c as usize * 2;
            image[at..at + 2].copy_from_slice(&value.to_le_bytes());
            let chunk = &bytes[(i * 512).min(bytes.len())..((i + 1) * 512).min(bytes.len())];
            let data = (DATA_START + c as usize - 2) * 512;
            image[data..data + chunk.len()].copy_from_slice(chunk);
        }
    };
    let entry =
        |image: &mut Vec<u8>, at: usize, name: &[u8; 11], attr: u8, first: u16, len: usize| {
            image[at..at + 11].copy_from_slice(name);
            image[at + 11] = attr;
            image[at + 26..at + 28].copy_from_slice(&first.to_le_bytes());
            image[at + 28..at + 32].copy_from_slice(&(len as u32).to_le_bytes());
        };
    for (name, bytes, clusters) in files {
        put(&mut image, bytes, clusters);
        entry(&mut image, root_at, name, 0x20, clusters[0], bytes.len());
        root_at += 32;
    }
    for (name, dir_cluster, entries) in dirs {
        // The directory's own chain holds its entries, one per 32 bytes from its cluster's start.
        let dir_at = (DATA_START + *dir_cluster as usize - 2) * 512;
        let mut at = dir_at;
        for (ename, bytes, clusters) in entries {
            put(&mut image, bytes, clusters);
            entry(&mut image, at, ename, 0x20, clusters[0], bytes.len());
            at += 32;
        }
        put(&mut image, &[], &[*dir_cluster]);
        entry(&mut image, root_at, name, 0x10, *dir_cluster, 0);
        root_at += 32;
    }
    image
}

/// The bytes a file of `len` has: distinct per file, so a read from the wrong place differs.
fn bytes_of(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(7).wrapping_add(seed))
        .collect()
}

/// Every boot file the slot's directory has, split across clusters, reads back exactly through the
/// raw reader (`read_raw`): the kernel, the initramfs, the device tree, the command line, and the
/// overlay directory's README and board overlay (a subdirectory's fragmented file).
#[test]
fn every_boot_file_kind_reads_raw_through_a_fragmented_fat_image() {
    let dir = crate::tests::TempDir::new("read-raw");
    let kernel = bytes_of(1300, 1);
    let cmdline = bytes_of(40, 2);
    let initramfs = bytes_of(900, 3);
    let dtb = bytes_of(600, 4);
    let readme = bytes_of(10, 5);
    let overlay = bytes_of(1100, 6);
    let image = fragmented_image(
        &[
            (b"KERNEL8 IMG", kernel.clone(), vec![2, 4, 6]),
            (b"CMDLINE TXT", cmdline.clone(), vec![3]),
            (b"INITRAMFCPI", initramfs.clone(), vec![5, 7]),
            (b"BOARD   DTB", dtb.clone(), vec![8, 15]),
        ],
        &[(
            b"OVERLAYS   ",
            9,
            vec![
                (b"README     ", readme.clone(), vec![11]),
                (b"LBOARD  DTB", overlay.clone(), vec![12, 14, 16]),
            ],
        )],
    );
    let node = dir.0.join("boot.img");
    fs::write(&node, image).unwrap();
    let read = |path: &str| boot_guard::read_raw(&Ok(node.clone()), path);
    assert_eq!(read("KERNEL8.IMG").unwrap(), Some(kernel));
    assert_eq!(read("CMDLINE.TXT").unwrap(), Some(cmdline));
    assert_eq!(read("INITRAMF.CPI").unwrap(), Some(initramfs));
    assert_eq!(read("BOARD.DTB").unwrap(), Some(dtb));
    assert_eq!(read("OVERLAYS/README").unwrap(), Some(readme));
    assert_eq!(read("OVERLAYS/LBOARD.DTB").unwrap(), Some(overlay));
    assert_eq!(read("MISSING.TXT").unwrap(), None);
}

/// A boot partition whose device is not known is refused by name, not read as empty.
#[test]
fn no_device_is_refused_by_name() {
    let why = boot_guard::read_raw(&Err("no source".into()), "CMDLINE.TXT").unwrap_err();
    assert!(why.contains("device isn't known"), "{why}");
}
