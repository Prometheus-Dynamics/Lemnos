//! The boot over a temporary directory standing in for `/`: sysfs, the device nodes, both
//! partitions and the kernel command line are files there, and a fake [`Ops`] writes down what it
//! was asked. Nothing touches a disk, a mount or the boot configuration.

use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use lemnos_slot_store::manifest::{BLOCK, Verity, tree_bytes};
use lemnos_slot_store::store::{Layout, Meta, State};
use lemnos_slot_store::ubi::{self, Ubi, UbiWrite};

use super::{BOOT_LOG, DATA_LOG, Env, boot};
use crate::decide::MAX_FALLBACKS;
use crate::ops::Ops;

struct Root(PathBuf);

impl Drop for Root {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn hash(id: &str) -> String {
    let digit = id.chars().last().unwrap();
    std::iter::repeat_n(digit, 64).collect()
}

/// What the fake was asked, and which calls it refuses (by prefix).
struct Fake {
    root: PathBuf,
    calls: RefCell<Vec<String>>,
    logs: RefCell<Vec<String>>,
    refuse: Vec<&'static str>,
    /// On a NAND board, the UBI device.
    nand: Option<ubi::Fake>,
}

impl Fake {
    fn short(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .map_or_else(|_| path.display().to_string(), |p| p.display().to_string())
    }

    fn call(&self, text: String) -> Result<(), String> {
        let refused = self.refuse.iter().any(|r| text.starts_with(r));
        self.calls.borrow_mut().push(text);
        if refused {
            Err("refused by the test".into())
        } else {
            Ok(())
        }
    }

    fn calls(&self) -> Vec<String> {
        self.calls.borrow().clone()
    }

    fn called(&self, prefix: &str) -> bool {
        self.calls.borrow().iter().any(|c| c.starts_with(prefix))
    }

    fn last(&self) -> String {
        self.calls.borrow().last().cloned().unwrap_or_default()
    }
}

impl Ops for Fake {
    fn log(&self, line: &str) {
        self.logs.borrow_mut().push(line.to_string());
    }

    fn mount(&self, fstype: &str, opts: &str, source: &Path, target: &Path) -> Result<(), String> {
        self.call(format!(
            "mount {fstype} {opts} {} {}",
            self.short(source),
            self.short(target)
        ))
    }

    fn move_mount(&self, source: &Path, target: &Path) -> Result<(), String> {
        self.call(format!(
            "move {} {}",
            self.short(source),
            self.short(target)
        ))
    }

    fn umount(&self, target: &Path) {
        let _ = self.call(format!("umount {}", self.short(target)));
    }

    fn load_module(&self, name: &str, release: &str) -> Result<(), String> {
        self.call(format!("module {name} {release}"))
    }

    fn open_verity(&self, image: &Path, name: &str, verity: &Verity) -> Result<String, String> {
        self.call(format!(
            "verity {} {name} {}",
            self.short(image),
            verity.table("loop")
        ))
        .map(|()| "/dev/dm-0".to_string())
    }

    fn ubi_block(&self, ubi: &Ubi, name: &str) -> Result<PathBuf, String> {
        self.call(format!("ubiblock {} {name}", ubi.device))?;
        let device = self.nand.as_ref().ok_or("no UBI device")?;
        device.block(name).map_err(|e| e.to_string())
    }

    fn ubi_replace(&self, ubi: &Ubi, name: &str, bytes: &[u8]) -> Result<(), String> {
        self.call(format!("ubi replace {name}"))?;
        let device = self.nand.as_ref().ok_or("no UBI device")?;
        ubi::replace(ubi, device, name, bytes).map_err(|e| e.to_string())
    }

    fn kernel_log(&self) -> String {
        "the kernel's log\n".into()
    }

    fn sync(&self) {}

    fn restart(&self, argument: Option<&str>) {
        let _ = self.call(match argument {
            Some(argument) => format!("restart {argument}"),
            None => "restart".into(),
        });
    }

    fn switch_root(&self, new_root: &Path) -> Result<(), String> {
        self.call(format!("switch_root {}", self.short(new_root)))
    }

    fn sleep(&self, time: Duration) {
        if time >= Duration::from_secs(1) {
            let _ = self.call(format!("sleep {}", time.as_secs()));
        }
    }
}

/// A board: one disk with a boot partition and a data partition (PARTUUID `1234abcd-02`,
/// labelled `data`), slots `os-1` (previous) and `os-2` (default), booting `os-2`.
struct Board {
    _dir: Root,
    env: Env,
    layout: Layout,
    /// On raw NAND: the UBI device's description.
    nand: Option<Ubi>,
}

impl Board {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let root = std::env::temp_dir().join(format!(
            "lemnos-slot-init-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&root);
        let env = Env {
            root: root.clone(),
            sys: root.join("sys"),
            dev: root.join("dev"),
            wait: Duration::from_millis(100),
            rescue_wait: Duration::from_secs(30),
        };
        let board = Self {
            _dir: Root(root.clone()),
            layout: Layout::new(root.join("data"), root.join("boot")),
            env,
            nand: None,
        };
        fs::create_dir_all(board.env.dev.clone()).unwrap();
        let mut mbr = vec![0u8; 512];
        mbr[440..444].copy_from_slice(&0x1234_abcdu32.to_le_bytes());
        mbr[450] = 0x0c;
        mbr[510..512].copy_from_slice(&[0x55, 0xaa]);
        board.write("dev/mmcblk0", &mbr);
        board.write("dev/mmcblk0p1", &[0u8; 2048]);
        board.write("dev/mmcblk0p2", &ext("data", false));
        for n in 1..=2 {
            let name = format!("mmcblk0p{n}");
            board.write(
                &format!("sys/devices/mmcblk0/{name}/partition"),
                format!("{n}\n").as_bytes(),
            );
            fs::create_dir_all(root.join("sys/class/block")).unwrap();
            std::os::unix::fs::symlink(
                format!("../../devices/mmcblk0/{name}"),
                root.join("sys/class/block").join(&name),
            )
            .unwrap();
        }
        board.cmdline("os-2", &hash("os-2"));
        board.write("proc/sys/kernel/osrelease", b"6.12.0-board\n");
        board.slot("os-1", 1);
        board.slot("os-2", 2);
        board.write("boot/default.txt", b"os_prefix=slots/os-2/\n");
        board.write(
            "data/board/slots/state.env",
            b"DEFAULT=os-2\nPREVIOUS=os-1\nSTATE=confirmed\n",
        );
        board
    }

    fn write(&self, path: &str, bytes: &[u8]) {
        let path = self.env.at(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    fn read(&self, path: &str) -> String {
        fs::read_to_string(self.env.at(path)).unwrap_or_default()
    }

    fn cmdline(&self, slot: &str, roothash: &str) {
        self.write(
            "proc/cmdline",
            format!(
                "console=tty1 rootwait os.slot={slot} os.roothash={roothash} os.data=PARTUUID=1234abcd-02 panic=5\n"
            )
            .as_bytes(),
        );
    }

    /// An installed slot: manifest, image, record, and boot directory.
    fn slot(&self, id: &str, seq: u64) {
        self.manifest(id, id, "");
        let size = BLOCK + tree_bytes(1);
        assert_eq!(tree_bytes(1), 0, "one data block has no tree");
        self.write(&format!("data/slots/{id}.erofs"), &vec![0u8; size as usize]);
        self.write(
            &format!("data/board/slots/{id}.meta"),
            format!("GOOD=yes\nSEQ={seq}\n").as_bytes(),
        );
        self.write(
            &format!("boot/slots/{id}/cmdline.txt"),
            format!(
                "console=tty1 os.slot={id} os.roothash={} os.data=PARTUUID=1234abcd-02\n",
                hash(id)
            )
            .as_bytes(),
        );
    }

    fn manifest(&self, file: &str, id: &str, extra: &str) {
        let sha = hash("x0");
        let text = format!(
            "{{\"schema\":3,\"id\":\"{id}\",\"product\":\"an-os\",\"version\":\"2.0\",\"board\":\"board-a\",{extra}\
             \"image\":{{\"size\":{},\"sha256\":\"{sha}\"}},\
             \"verity\":{{\"root_hash\":\"{}\",\"salt\":\"{sha}\",\"data_blocks\":1,\"hash_offset\":{BLOCK}}},\
             \"boot\":{{\"files\":{{\"cmdline.txt\":\"{sha}\"}}}}}}",
            BLOCK + tree_bytes(1),
            hash(id),
        );
        self.write(&format!("data/slots/{file}.json"), text.as_bytes());
    }

    fn remove_slot(&self, id: &str) {
        fs::remove_file(self.env.at(&format!("data/slots/{id}.json"))).unwrap();
        fs::remove_file(self.env.at(&format!("data/slots/{id}.erofs"))).unwrap();
    }

    fn boot(&self, refuse: &[&'static str]) -> (Fake, String) {
        let fake = Fake {
            root: self.env.root.clone(),
            calls: RefCell::new(Vec::new()),
            logs: RefCell::new(Vec::new()),
            refuse: refuse.to_vec(),
            nand: self.nand.clone().map(|ubi| ubi::Fake {
                ubi,
                refuse: std::sync::Mutex::new(Vec::new()),
            }),
        };
        let why = boot(&fake, &self.env);
        (fake, why)
    }

    /// The same board on raw NAND: the data and boot directories are UBIFS volumes (here plain
    /// directories), the images static volumes, and the record the volume `boot-record`.
    fn nand() -> Self {
        let mut board = Self::new();
        let ubi = Ubi {
            sys: board.env.sys.clone(),
            dev: board.env.dev.clone(),
            device: "ubi0".into(),
        };
        let device = ubi::Fake::new(ubi.clone(), 64 << 20);
        device.put("data", b"");
        device.put("boot", b"");
        for id in ["os-1", "os-2"] {
            let file = board.env.at(&format!("data/slots/{id}.erofs"));
            device.put(&ubi::image_volume(id), &fs::read(&file).unwrap());
            fs::remove_file(file).unwrap();
        }
        device.put(
            ubi::RECORD_VOLUME,
            b"default=os-2\ntrial=\ntrial_attempts=0\n",
        );
        fs::remove_file(board.env.at("boot/default.txt")).unwrap();
        board.write(
            "proc/cmdline",
            format!(
                "os.slot=os-2 os.roothash={} os.data=UBI=ubi0:data",
                hash("os-2")
            )
            .as_bytes(),
        );
        board.layout.ubi = Some(ubi.clone());
        board.nand = Some(ubi);
        board
    }

    fn record_volume(&self) -> String {
        String::from_utf8(
            self.layout
                .ubi
                .as_ref()
                .unwrap()
                .read(ubi::RECORD_VOLUME)
                .unwrap(),
        )
        .unwrap()
    }

    fn meta(&self, id: &str) -> Meta {
        Meta::load(&self.layout, id)
    }

    fn state(&self) -> State {
        State::load(&self.layout)
    }

    /// The board's default is untouched: slot-init never writes it.
    fn default_kept(&self) {
        assert_eq!(self.read("boot/default.txt"), "os_prefix=slots/os-2/\n");
        assert_eq!(self.state().default, "os-2");
    }
}

/// An ext4 superblock's first 2 KiB: its label, and whether it records errors.
fn ext(label: &str, errors: bool) -> Vec<u8> {
    let mut head = vec![0u8; 2048];
    head[1024 + 56..1024 + 58].copy_from_slice(&0xef53u16.to_le_bytes());
    head[1024 + 58] = if errors { 2 } else { 1 };
    head[1024 + 120..1024 + 120 + label.len()].copy_from_slice(label.as_bytes());
    head
}

#[test]
fn the_default_slot_boots_through_dm_verity() {
    let board = Board::new();
    let (fake, why) = board.boot(&[]);
    assert_eq!(why, "", "{:?}", fake.logs.borrow());
    let calls = fake.calls();
    let at = |prefix: &str| calls.iter().position(|c| c.starts_with(prefix)).unwrap();
    assert!(calls.contains(&"mount ext4 noatime,errors=remount-ro dev/mmcblk0p2 data".to_string()));
    assert!(calls.contains(&"mount vfat ro dev/mmcblk0p1 boot".to_string()));
    let verity = format!(
        "verity data/slots/os-2.erofs slot 0 8 verity 1 loop loop 4096 4096 1 1 sha256 {} {}",
        hash("os-2"),
        hash("x0")
    );
    assert!(calls.contains(&verity), "{calls:?}");
    assert!(at("module dm-verity 6.12.0-board") < at("verity "));
    assert!(at("verity ") < at("mount erofs ro /dev/dm-0 newroot"));
    assert!(at("mount erofs") < at("move data newroot/data"));
    assert_eq!(fake.last(), "switch_root newroot");
    assert_eq!(board.read("newroot/data/board/slots/booted"), "os-2\n");
    assert_eq!(board.read("data/board/slots/trial-init"), "");
    assert_eq!(board.read("newroot/data/board/slots/trial-started"), "");
}

#[test]
fn a_data_partition_with_errors_mounts_read_only() {
    let board = Board::new();
    board.write("dev/mmcblk0p2", &ext("data", true));
    let (fake, why) = board.boot(&[]);
    assert_eq!(why, "");
    assert!(fake.called("mount ext4 ro,noatime dev/mmcblk0p2 data"));
}

#[test]
fn the_data_partition_is_found_by_its_label_when_the_partuuid_doesnt_answer() {
    let board = Board::new();
    board.write(
        "proc/cmdline",
        format!(
            "os.slot=os-2 os.roothash={} os.data=PARTUUID=deadbeef-02",
            hash("os-2")
        )
        .as_bytes(),
    );
    let (fake, why) = board.boot(&[]);
    assert_eq!(why, "");
    assert!(fake.called("mount ext4 noatime,errors=remount-ro dev/mmcblk0p2 data"));
}

#[test]
fn no_data_partition_waits_and_restarts() {
    let board = Board::new();
    board.write("dev/mmcblk0p2", &ext("other", false));
    board.write(
        "proc/cmdline",
        format!(
            "os.slot=os-2 os.roothash={} os.data=PARTUUID=deadbeef-02",
            hash("os-2")
        )
        .as_bytes(),
    );
    let (fake, why) = board.boot(&[]);
    assert!(why.contains("no data partition"), "{why}");
    assert!(!fake.called("mount ext4"));
    assert!(fake.called("sleep 30"));
    assert_eq!(fake.last(), "restart");
}

#[test]
fn no_slot_on_the_command_line_waits_and_restarts() {
    let board = Board::new();
    board.write("proc/cmdline", b"console=tty1");
    let (fake, why) = board.boot(&[]);
    assert!(why.contains(".slot="), "{why}");
    assert_eq!(fake.last(), "restart");
}

#[test]
fn a_trial_is_told_from_the_firmwares_flag() {
    let board = Board::new();
    board.write(
        lemnos_slot_store::records::tryboot::TRIAL_FLAG,
        &[0, 0, 0, 1],
    );
    let (_, why) = board.boot(&[]);
    assert_eq!(why, "");
    assert_eq!(board.read("data/board/slots/trial-init"), "os-2\n");
    assert_eq!(
        board.read("newroot/data/board/slots/trial-started"),
        "os-2\n"
    );
}

#[test]
fn a_slot_that_isnt_the_default_is_a_trial() {
    let board = Board::new();
    board.cmdline("os-1", &hash("os-1"));
    let (_, why) = board.boot(&[]);
    assert_eq!(why, "");
    assert_eq!(board.read("data/board/slots/trial-init"), "os-1\n");
}

/// Falls back from the default to the previous slot on trial, recorded: the common end of every
/// refusal.
fn assert_fell_back(board: &Board, fake: &Fake, reason: &str) {
    let meta = board.meta("os-2");
    assert_eq!(meta.get("FAILED"), "yes");
    assert_eq!(meta.get("FAILURES"), "1");
    assert!(
        meta.get("FAIL_REASON").contains(reason),
        "{:?}",
        meta.get("FAIL_REASON")
    );
    assert_eq!(board.read("boot/trial.txt"), "os_prefix=slots/os-1/\n");
    // The trial's attempts are the board's: the default's kernel takes one before the trial boots.
    assert_eq!(board.read("boot/trial-attempts"), "3\n");
    assert_eq!(board.read("data/board/slots/fallbacks"), "1\n");
    assert_eq!(board.state().trial, "os-1");
    // A plain restart: the loader's pass takes the attempt, then the trial boots.
    assert_eq!(fake.last(), "restart");
    assert!(!fake.called("switch_root"));
    board.default_kept();
}

#[test]
fn a_root_hash_that_isnt_the_command_lines_is_refused() {
    let board = Board::new();
    board.cmdline("os-2", &hash("os-9"));
    let (fake, why) = board.boot(&[]);
    assert!(why.contains("root hash"), "{why}");
    assert!(!fake.called("verity "));
    assert_fell_back(&board, &fake, "root hash");
}

#[test]
fn a_verity_failure_is_recorded_and_the_board_falls_back() {
    let board = Board::new();
    let (fake, why) = board.boot(&["verity "]);
    assert!(why.contains("dm-verity"), "{why}");
    assert_fell_back(&board, &fake, "dm-verity refused the image");
    assert_eq!(
        board.read(&format!("data/board/slots/{DATA_LOG}")),
        "the kernel's log\n"
    );
    assert_eq!(
        board.read(&format!("boot/{BOOT_LOG}")),
        "the kernel's log\n"
    );
    assert!(fake.called("mount vfat rw dev/mmcblk0p1 boot"));
}

#[test]
fn a_cut_short_image_is_refused() {
    let board = Board::new();
    board.write("data/slots/os-2.erofs", &[0u8; 2048]);
    let (fake, _) = board.boot(&[]);
    assert!(!fake.called("verity "));
    assert_fell_back(&board, &fake, "cut short");
}

#[test]
fn a_missing_image_is_refused() {
    let board = Board::new();
    fs::remove_file(board.env.at("data/slots/os-2.erofs")).unwrap();
    let (fake, _) = board.boot(&[]);
    assert_fell_back(&board, &fake, "no image");
}

#[test]
fn a_manifest_for_another_slot_is_refused() {
    let board = Board::new();
    board.manifest("os-2", "os-7", "");
    let (fake, _) = board.boot(&[]);
    assert_fell_back(&board, &fake, "for slot os-7");
}

#[test]
fn a_slot_built_for_another_kernel_is_refused() {
    let board = Board::new();
    board.manifest("os-2", "os-2", "\"kernel\":\"6.1.0-other\",");
    let (fake, _) = board.boot(&[]);
    assert_fell_back(&board, &fake, "kernel 6.1.0-other");
}

#[test]
fn a_slot_built_for_this_kernel_boots() {
    let board = Board::new();
    board.manifest("os-2", "os-2", "\"kernel\":\"6.12.0-board\",");
    let (_, why) = board.boot(&[]);
    assert_eq!(why, "");
}

#[test]
fn an_image_that_wont_mount_falls_back() {
    let board = Board::new();
    let (fake, _) = board.boot(&["mount erofs"]);
    assert_fell_back(&board, &fake, "can't mount the image");
}

#[test]
fn rule_1_a_failed_trial_restarts_plainly_into_the_default() {
    let board = Board::new();
    board.write(
        lemnos_slot_store::records::tryboot::TRIAL_FLAG,
        &[0, 0, 0, 1],
    );
    let (fake, _) = board.boot(&["verity "]);
    assert_eq!(fake.last(), "restart");
    assert_eq!(board.read("boot/trial.txt"), "");
    assert_eq!(board.meta("os-2").get("FAILED"), "yes");
    assert!(!fake.called("sleep"));
    board.default_kept();
}

#[test]
fn rule_2_falling_back_stops_after_three_in_a_row() {
    let board = Board::new();
    board.write(
        "data/board/slots/fallbacks",
        format!("{MAX_FALLBACKS}\n").as_bytes(),
    );
    let (fake, why) = board.boot(&["verity "]);
    assert!(why.contains("in a row"), "{why}");
    assert_eq!(board.read("boot/trial.txt"), "");
    assert!(fake.called("sleep 30"));
    assert_eq!(fake.last(), "restart");
    board.default_kept();
}

#[test]
fn rule_3_a_previous_slot_that_failed_gives_way_to_a_good_one() {
    let board = Board::new();
    board.slot("os-3", 3);
    board.write(
        "data/board/slots/os-1.meta",
        b"GOOD=yes\nSEQ=1\nFAILED=yes\n",
    );
    let (fake, _) = board.boot(&["verity "]);
    assert_eq!(board.read("boot/trial.txt"), "os_prefix=slots/os-3/\n");
    assert_eq!(fake.last(), "restart");
}

#[test]
fn rule_3_only_a_slot_with_boot_files_on_the_boot_partition_is_tried() {
    let board = Board::new();
    fs::remove_dir_all(board.env.at("boot/slots/os-1")).unwrap();
    let (fake, why) = board.boot(&["verity "]);
    assert!(why.contains("no other slot"), "{why}");
    assert_eq!(board.read("boot/trial.txt"), "");
    assert_eq!(fake.last(), "restart");
}

#[test]
fn rule_4_a_board_with_one_slot_waits_and_restarts_into_it() {
    let board = Board::new();
    board.remove_slot("os-1");
    board.write("data/board/slots/state.env", b"DEFAULT=os-2\n");
    let (fake, why) = board.boot(&["verity "]);
    assert!(why.contains("no other slot"), "{why}");
    assert_eq!(board.read("boot/trial.txt"), "");
    assert!(fake.called("sleep 30"));
    assert_eq!(fake.last(), "restart");
    board.default_kept();
}

#[test]
fn a_uboot_board_falls_back_through_a_trial_with_attempts() {
    let board = Board::new();
    board.write(
        "boot/boot-record.env",
        b"default=os-2\ntrial=\ntrial_attempts=0\n",
    );
    let (fake, _) = board.boot(&["verity "]);
    assert_eq!(
        board.read("boot/boot-record.env"),
        "default=os-2\ntrial=os-1\ntrial_attempts=3\n"
    );
    assert_eq!(board.read("boot/trial.txt"), "");
    assert_eq!(fake.last(), "restart");
}

#[test]
fn a_file_record_trial_takes_one_attempt_and_keeps_the_trial() {
    let board = Board::new();
    board.write(
        "boot/boot-record",
        b"default=os-2\ntrial=os-1\ntrial_attempts=2\n",
    );
    let (fake, why) = board.boot(&[]);
    assert_eq!(why, "", "{:?}", fake.logs.borrow());
    assert_eq!(
        board.read("boot/boot-record"),
        "default=os-2\ntrial=os-1\ntrial_attempts=1\n"
    );
    assert!(fake.called("verity data/slots/os-1.erofs"));
    assert_eq!(board.read("data/board/slots/trial-init"), "os-1\n");
    assert_eq!(board.read("newroot/data/board/slots/booted"), "os-1\n");
}

#[test]
fn a_file_record_trial_with_no_attempts_left_is_cleared_and_the_default_boots() {
    let board = Board::new();
    board.write(
        "boot/boot-record",
        b"default=os-2\ntrial=os-1\ntrial_attempts=0\n",
    );
    let (fake, why) = board.boot(&[]);
    assert_eq!(why, "", "{:?}", fake.logs.borrow());
    assert_eq!(board.read("boot/boot-record"), "default=os-2\n");
    assert!(!fake.called("verity data/slots/os-1.erofs"));
    assert!(fake.called("verity data/slots/os-2.erofs"));
    assert!(!board.env.at("data/board/slots/trial-init").exists());
}

#[test]
fn a_file_record_trial_takes_its_root_hash_from_its_own_command_line() {
    let board = Board::new();
    board.write(
        "boot/boot-record",
        b"default=os-2\ntrial=os-1\ntrial_attempts=2\n",
    );
    board.write(
        "boot/slots/os-1/cmdline.txt",
        b"os.slot=os-1 os.roothash=bad\n",
    );
    let (fake, _) = board.boot(&[]);
    assert!(!fake.called("verity "));
    assert!(board.meta("os-1").get("FAIL_REASON").contains("root hash"));
    // A failed trial keeps its attempts: the next boot takes one more.
    assert_eq!(
        board.read("boot/boot-record"),
        "default=os-2\ntrial=os-1\ntrial_attempts=1\n"
    );
    assert_eq!(fake.last(), "restart");
}

#[test]
fn a_file_record_falls_back_through_its_trial_line() {
    let board = Board::new();
    board.write("boot/boot-record", b"default=os-2\n");
    let (fake, _) = board.boot(&["verity "]);
    assert_eq!(
        board.read("boot/boot-record"),
        "default=os-2\ntrial=os-1\ntrial_attempts=3\n"
    );
    assert_eq!(fake.last(), "restart");
}

#[test]
fn a_boot_partition_that_wont_mount_still_boots_the_command_lines_slot() {
    let board = Board::new();
    let (fake, why) = board.boot(&["mount vfat"]);
    assert_eq!(why, "");
    assert_eq!(fake.last(), "switch_root newroot");
}

#[test]
fn on_nand_the_slot_boots_from_its_volume_through_ubiblock() {
    let board = Board::nand();
    let (fake, why) = board.boot(&[]);
    assert_eq!(why, "", "{:?}", fake.logs.borrow());
    let calls = fake.calls();
    assert!(
        calls.contains(&"mount ubifs noatime ubi0:data data".to_string()),
        "{calls:?}"
    );
    assert!(
        calls.contains(&"mount ubifs ro ubi0:boot boot".to_string()),
        "{calls:?}"
    );
    let block = calls
        .iter()
        .position(|c| c == "ubiblock ubi0 slot-os-2")
        .unwrap();
    let verity = calls
        .iter()
        .position(|c| c.starts_with("verity dev/ubiblock0_"))
        .unwrap();
    assert!(block < verity, "{calls:?}");
    assert!(!fake.called("verity data/"), "no loop over a file");
    assert_eq!(fake.last(), "switch_root newroot");
}

#[test]
fn on_nand_a_cut_short_volume_falls_back_through_the_record_volume() {
    let board = Board::nand();
    let device = ubi::Fake {
        ubi: board.nand.clone().unwrap(),
        refuse: std::sync::Mutex::new(Vec::new()),
    };
    device.remove("slot-os-2").unwrap();
    device.put("slot-os-2", &[0u8; 2048]);
    let (fake, why) = board.boot(&[]);
    assert!(why.contains("cut short"), "{why}");
    assert!(!fake.called("ubiblock"));
    assert!(fake.called("ubi replace boot-record"));
    assert_eq!(
        board.record_volume(),
        "default=os-2\ntrial=os-1\ntrial_attempts=3\n"
    );
    assert_eq!(fake.last(), "restart");
    assert_eq!(board.meta("os-2").get("FAILED"), "yes");
}

#[test]
fn on_nand_a_verity_failure_falls_back_and_the_default_stays() {
    let board = Board::nand();
    let (fake, _) = board.boot(&["verity "]);
    assert_eq!(
        board.record_volume(),
        "default=os-2\ntrial=os-1\ntrial_attempts=3\n"
    );
    assert!(board.meta("os-2").get("FAIL_REASON").contains("dm-verity"));
    assert!(fake.called("mount ubifs rw ubi0:boot boot"));
    assert_eq!(
        board.read(&format!("boot/{BOOT_LOG}")),
        "the kernel's log\n"
    );
    assert_eq!(fake.last(), "restart");
}

#[test]
fn on_nand_a_missing_data_volume_waits_and_restarts() {
    let board = Board::nand();
    board.write(
        "proc/cmdline",
        format!(
            "os.slot=os-2 os.roothash={} os.data=UBI=ubi0:other",
            hash("os-2")
        )
        .as_bytes(),
    );
    let (fake, why) = board.boot(&[]);
    assert!(why.contains("no data volume"), "{why}");
    assert!(
        fake.logs
            .borrow()
            .iter()
            .any(|l| l.contains("has no volume other"))
    );
    assert_eq!(fake.last(), "restart");
}

#[test]
fn on_nand_a_failed_trial_restarts_plainly_and_leaves_the_record() {
    let board = Board::nand();
    board.write(
        "proc/cmdline",
        format!(
            "os.slot=os-1 os.roothash={} os.data=UBI=ubi0:data",
            hash("os-1")
        )
        .as_bytes(),
    );
    let (fake, _) = board.boot(&["verity "]);
    assert_eq!(fake.last(), "restart");
    assert!(!fake.called("ubi replace"));
    assert_eq!(
        board.record_volume(),
        "default=os-2\ntrial=\ntrial_attempts=0\n"
    );
}

/// The tryboot pass: a trial with attempts left takes one before anything else is mounted, and
/// the board restarts into the trial with the tryboot argument. Decided in `decide`; here it is
/// over the boot partition.
#[test]
fn a_pending_tryboot_trial_takes_an_attempt_and_restarts_into_it_before_anything_else() {
    let board = Board::new();
    board.write("boot/trial.txt", b"os_prefix=slots/os-1/\n");
    board.write("boot/trial-attempts", b"2\n");
    let (fake, why) = board.boot(&[]);
    assert!(why.contains("restarting into trial os-1"), "{why}");
    assert_eq!(board.read("boot/trial-attempts"), "1\n");
    assert_eq!(board.read("boot/trial.txt"), "os_prefix=slots/os-1/\n");
    assert_eq!(fake.last(), "restart 0 tryboot");
    // Only the boot partition was mounted for it: the data partition came later, if at all.
    assert!(fake.called("mount vfat rw dev/mmcblk0p1 boot"));
    assert!(!fake.called("mount ext4"));
    assert!(!fake.called("verity "));
}

#[test]
fn a_tryboot_trial_boot_takes_nothing_more() {
    let board = Board::new();
    board.write(
        lemnos_slot_store::records::tryboot::TRIAL_FLAG,
        &[0, 0, 0, 1],
    );
    board.write("boot/trial.txt", b"os_prefix=slots/os-1/\n");
    board.write("boot/trial-attempts", b"2\n");
    let (fake, why) = board.boot(&[]);
    assert_eq!(why, "", "{:?}", fake.logs.borrow());
    assert_eq!(board.read("boot/trial-attempts"), "2\n");
    assert!(!fake.called("restart"));
}

#[test]
fn a_tryboot_trial_with_no_attempts_left_is_removed_and_the_default_boots() {
    let board = Board::new();
    board.write("boot/trial.txt", b"os_prefix=slots/os-1/\n");
    board.write("boot/trial-attempts", b"0\n");
    let (fake, why) = board.boot(&[]);
    assert_eq!(why, "", "{:?}", fake.logs.borrow());
    assert!(!board.env.at("boot/trial.txt").exists());
    assert!(!board.env.at("boot/trial-attempts").exists());
    assert!(!fake.called("restart"));
    assert!(fake.called("verity data/slots/os-2.erofs"));
}

#[test]
fn a_tryboot_trial_without_a_counter_is_left_as_it_always_was() {
    let board = Board::new();
    board.write("boot/trial.txt", b"os_prefix=slots/os-1/\n");
    let (fake, why) = board.boot(&[]);
    assert_eq!(why, "", "{:?}", fake.logs.borrow());
    assert!(!fake.called("restart"));
    assert_eq!(board.read("boot/trial.txt"), "os_prefix=slots/os-1/\n");
}

#[test]
fn a_fallback_trial_gets_the_attempts_the_board_was_given() {
    let board = Board::new();
    board.write("data/board/slots/settings.env", b"TRIAL_ATTEMPTS=2\n");
    let (fake, _) = board.boot(&["verity "]);
    assert_eq!(board.read("boot/trial-attempts"), "2\n");
    assert_eq!(fake.last(), "restart");
}
