//! Route A: a slot of ours added to another system's disk (`docs/slots.md`, "Adding to another
//! system's disk"). The boot partition's pointers stay the other system's, and the slots state
//! under the data partition is that system's. The driver starts as `start` starts it, over the
//! real record writer, and writes nothing there: not at start, not on any tick, not by any call.

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use lemnos_contract_security_precondition::SecurityPrecondition;
use lemnos_contract_system_slots::Slots;
use lemnos_core::{CallerId, Config, ConfigValue, Health, Lifecycle, Origin, Status};
use lemnos_slot_store::fatimage::{FOREIGN_SLOT, route_a_system};

use crate::store::Layout;
use crate::tests::{TempDir, TestRoots, View, boot_tree};
use crate::{FOREIGN, SystemSlots, SystemSlotsNeeds};

/// Every entry under `root`, in path order: its path, kind, mode, owner and bytes (a link's target).
fn tree(root: &Path) -> Vec<(PathBuf, String, u32, u32, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(path) = stack.pop() {
        let meta = fs::symlink_metadata(&path).unwrap();
        let rel = path.strip_prefix(root).unwrap().to_path_buf();
        let (kind, bytes) = if meta.is_symlink() {
            (
                "link",
                fs::read_link(&path)
                    .unwrap()
                    .into_os_string()
                    .into_encoded_bytes(),
            )
        } else if meta.is_dir() {
            for entry in fs::read_dir(&path).unwrap() {
                stack.push(entry.unwrap().path());
            }
            ("dir", Vec::new())
        } else {
            ("file", fs::read(&path).unwrap())
        };
        out.push((rel, kind.into(), meta.mode(), meta.uid(), bytes));
    }
    out.sort();
    out
}

/// The other system's slots state, as an older build of it left it on its data partition: the
/// eight-key state file this system's writer also uses, a slot's meta, the boot markers, a slot's
/// image and something a sender left in `.incoming`.
fn foreign_state(layout: &Layout) {
    let dir = layout.state_dir();
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("state.env"),
        format!(
            "DEFAULT='{FOREIGN_SLOT}'\nPREVIOUS=''\nTRIAL=''\nSTAGED=''\nSTATE='idle'\nERROR=''\nPROGRESS='0'\nSTARTED_BY=''\n"
        ),
    )
    .unwrap();
    fs::write(
        dir.join(format!("{FOREIGN_SLOT}.meta")),
        "GOOD=yes\nFAILED=no\nPINNED=no\nSEQ=1\n",
    )
    .unwrap();
    fs::write(dir.join("booted"), format!("{FOREIGN_SLOT}\n")).unwrap();
    fs::write(dir.join("trial-keeps-default"), format!("{FOREIGN_SLOT}\n")).unwrap();
    fs::write(dir.join("settings.env"), "KEEP='2'\n").unwrap();
    fs::set_permissions(dir.join("settings.env"), fs::Permissions::from_mode(0o600)).unwrap();
    let images = layout.images();
    fs::create_dir_all(images.join(FOREIGN_SLOT)).unwrap();
    fs::write(
        images.join(FOREIGN_SLOT).join("rootfs.erofs"),
        b"the other system's root",
    )
    .unwrap();
    fs::create_dir_all(images.join(".incoming")).unwrap();
    fs::write(images.join(".incoming/part"), b"half a bundle").unwrap();
}

/// The driver as the host starts it, on a board whose boot partition holds `image`.
fn started(dir: &TempDir, layout: &Layout) -> (SystemSlots, PathBuf, TestRoots) {
    let data = layout.data.display().to_string();
    let boot = layout.boot.display().to_string();
    let missing = dir.0.join("no-such-node").display().to_string();
    let entries = [
        ("data", ConfigValue::Str(&data)),
        ("boot", ConfigValue::Str(&boot)),
        (
            "restart_socket",
            ConfigValue::Str("/nonexistent/lemnos-restart.sock"),
        ),
        ("partuuid", ConfigValue::Str("abcd-02")),
        ("boot_args", ConfigValue::Str("board")),
        ("reserve_mb", ConfigValue::Int(0)),
        ("device_tree", ConfigValue::Str(&missing)),
        ("vcio", ConfigValue::Str(&missing)),
    ];
    let lists: [(&str, &[&str]); 1] = [("boot_sources", &["/soc/mmc@7e202000=/soc/mmc@7e202000"])];
    let cfg = Config::new(&entries).with_lists(&lists);
    let roots = TestRoots::set(&dir.0);
    let slots = <SystemSlots as Lifecycle>::start(SystemSlotsNeeds { indicator: None }, &cfg)
        .unwrap_or_else(|status| panic!("start: {status:?}"));
    (slots, dir.0.join("dev/mmcblk0p1"), roots)
}

/// The board as the host shows it: one healthy device, so the good-boot look has what it waits on.
fn view() -> View {
    View(vec![("camera", Origin::BuiltIn, true, Health::Ok)])
}

/// Ticks the driver from 0 to `secs` seconds into the boot, a look each second, past the uptime
/// (30 s) and the slow check (at once, then every ten minutes).
fn run_for(slots: &mut SystemSlots, secs: u64) {
    for s in 0..=secs {
        slots.board(&view());
        slots.tick(s * 1_000_000_000);
    }
}

#[test]
fn route_a_writes_nothing_at_start_on_ticks_or_by_any_call() {
    let dir = TempDir::new("route-a");
    let layout = Layout::new(dir.0.join("data"), dir.0.join("boot"));
    foreign_state(&layout);
    let node = boot_tree(&dir.0, &route_a_system());
    let data_before = tree(&layout.data);
    let boot_before = fs::read(&node).unwrap();

    let (mut slots, _, _roots) = started(&dir, &layout);
    assert_eq!(
        tree(&layout.data),
        data_before,
        "start wrote the other system's state"
    );

    // Past the uptime and the slow check, and past a second slow check.
    run_for(&mut slots, 1300);
    assert_eq!(
        tree(&layout.data),
        data_before,
        "a tick wrote the other system's state"
    );

    // Every call that would write is refused by its status, before it reads or writes anything.
    slots.caller(CallerId(5));
    let refused = Err(Status::FAILED_PRECONDITION);
    assert_eq!(slots.install(1024, false, false).map(drop), refused);
    assert_eq!(slots.accept(1024), refused);
    assert_eq!(slots.install_file("part", false, false).map(drop), refused);
    assert_eq!(slots.write(0, b"bytes"), refused);
    assert_eq!(slots.set_default(FOREIGN_SLOT, true), refused);
    // The refusal says why in words: the records are another system's, and whose.
    let mut said = [0u8; 256];
    let mut words = lemnos_core::Reply::new(&mut said);
    lemnos_core::Lifecycle::refusal(&mut slots, Status::FAILED_PRECONDITION, &mut words);
    let text = core::str::from_utf8(words.written()).unwrap().to_string();
    assert!(
        text.starts_with(&format!("{FOREIGN}: default.txt")),
        "{text:?}"
    );
    assert_eq!(slots.trial(FOREIGN_SLOT, false, false, true), refused);
    assert_eq!(slots.confirm(), refused);
    assert_eq!(slots.remove(FOREIGN_SLOT), refused);
    assert_eq!(slots.pin(FOREIGN_SLOT, true), refused);
    assert_eq!(slots.set_keep(9), refused);
    assert_eq!(slots.apply(true), refused);
    assert_eq!(slots.apply(false), refused);

    // The reads still answer, and say whose the state is.
    let mut buf = vec![0u8; 1 << 16];
    let n = slots.list(&mut buf).unwrap();
    let list = String::from_utf8_lossy(&buf[..n]).into_owned();
    assert!(
        list.contains(&format!("\"foreign\":\"{FOREIGN}: default.txt")),
        "{list}"
    );
    let n = slots.info(&mut buf).unwrap();
    let info = String::from_utf8_lossy(&buf[..n]).into_owned();
    assert!(
        info.contains(&format!("\"foreign\":\"{FOREIGN}: default.txt")),
        "{info}"
    );
    slots.boots(&mut buf).unwrap();
    let n = slots.precondition(true, &mut buf).unwrap();
    let lines = String::from_utf8_lossy(&buf[..n]).into_owned();
    assert!(lines.starts_with(&format!("busy: {FOREIGN}")), "{lines}");
    assert_eq!(slots.health(), Health::Degraded(FOREIGN));

    // And more ticks after the calls.
    run_for(&mut slots, 1300);
    assert_eq!(tree(&layout.data), data_before, "the state dir changed");
    assert_eq!(
        fs::read(&node).unwrap(),
        boot_before,
        "the boot partition changed"
    );
}

#[test]
fn our_own_disk_still_writes_at_start_and_on_confirm() {
    let dir = TempDir::new("route-ours");
    let layout = Layout::new(dir.0.join("data"), dir.0.join("boot"));
    fs::create_dir_all(layout.state_dir()).unwrap();
    fs::create_dir_all(layout.images().join("os-1")).unwrap();
    fs::write(layout.marker("booted"), "os-1\n").unwrap();
    boot_tree(&dir.0, &crate::bay_fixture::tryboot_image("os-1"));
    let before = tree(&layout.data);

    let (mut slots, _, _roots) = started(&dir, &layout);
    assert_eq!(slots.health(), Health::Ok);
    let after_start = tree(&layout.data);
    assert_ne!(
        after_start, before,
        "start on our own disk writes its settings"
    );

    slots.caller(CallerId(5));
    slots.confirm().unwrap();
    assert_eq!(
        fs::read_to_string(layout.marker("fallbacks")).unwrap(),
        "0\n"
    );
    assert!(
        fs::read_to_string(layout.state_dir().join("state.env"))
            .unwrap()
            .contains("DEFAULT=os-1\n"),
    );
    let mut buf = vec![0u8; 1 << 16];
    let n = slots.list(&mut buf).unwrap();
    let list = String::from_utf8_lossy(&buf[..n]).into_owned();
    assert!(list.contains("\"foreign\":\"\""), "{list}");
}

/// Records that can't be read to say whose they are (here the boot partition's device is not there):
/// nothing is written, on ticks or at start, but once the daemon has run its uptime the firmware's
/// count is cleared through the mailbox, so the initramfs's way back never counts an ordinary restart
/// of a board that only can't judge its records. The mailbox here is not there: the clear is refused,
/// said in `info` beside the foreign words, and tried again; nothing is written still.
#[test]
fn records_that_cannot_be_judged_still_clear_the_count_once_up_and_write_nothing() {
    let dir = TempDir::new("route-unknown");
    let layout = Layout::new(dir.0.join("data"), dir.0.join("boot"));
    fs::create_dir_all(layout.state_dir()).unwrap();
    fs::write(layout.marker("booted"), "os-1\n").unwrap();
    let before = tree(&layout.data);
    let (mut slots, _, _roots) = started(&dir, &layout);
    assert_eq!(slots.health(), Health::Degraded(crate::OWNER_UNKNOWN));
    let mut buf = vec![0u8; 1 << 16];
    // Before the uptime (30 s): nothing is tried.
    run_for(&mut slots, 10);
    let n = slots.info(&mut buf).unwrap();
    let info = String::from_utf8_lossy(&buf[..n]).into_owned();
    assert!(info.contains("foreign-unknown"), "{info}");
    assert!(!info.contains("ran its uptime"), "{info}");
    // Past it: the clear is tried, refused here (no mailbox), and said.
    let mut next = Some(10_000_000_000);
    while let Some(at) = next.filter(|at| *at <= 60_000_000_000) {
        next = slots.tick(at);
    }
    assert!(next.is_some(), "an unknown answer is looked at again");
    let n = slots.info(&mut buf).unwrap();
    let info = String::from_utf8_lossy(&buf[..n]).into_owned();
    assert!(
        info.contains("the boot count couldn't be cleared although the daemon ran its uptime"),
        "{info}"
    );
    assert_eq!(
        tree(&layout.data),
        before,
        "nothing is written to the state"
    );
}
