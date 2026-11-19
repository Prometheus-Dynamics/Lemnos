//! The B2 host check (`lemnos_k230_usbboot::b2`) against the rehearsal's board (the check build's own code
//! over a register model of the dwc2 core) and against plain fakes for the refusals. The full rehearsal,
//! with the board's own bounds and every case, is `k230-b2 rehearse`.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use lemnos_driver_k230_usbboot_sys::b2::{NO_CLOCK_LINE, TIMEBASE_STOPPED_LINE};
use lemnos_k230_usbboot::b2::{self, Access, Board, Bounds, Link, Plan, Seen, Verdict, usb::Sysfs};
use lemnos_k230_usbboot::b2sim::{CoreRev, Faults, Reset, SimBoard, TimeCsr};

fn quick() -> Bounds {
    Bounds {
        enumerate: Duration::from_secs(3),
        back: Duration::from_secs(10),
        ..Bounds::BOARD
    }
}

fn plan(bounds: Bounds) -> Plan {
    Plan {
        bounds,
        outside_window: true,
        ..Plan::board()
    }
}

fn rehearse(
    faults: Faults,
    bounds: Bounds,
) -> (b2::Outcome, Vec<(f64, Reset)>, Vec<String>, SimBoard) {
    let mut board = SimBoard::new(faults);
    let rom = board.rom();
    let started = board.load_and_start();
    let mut lines = Vec::new();
    let outcome = b2::run(&mut board, &plan(bounds), rom.as_ref(), started, &mut |t| {
        lines.push(t.to_string())
    });
    std::thread::sleep(Duration::from_millis(100));
    (outcome, board.resets(), lines, board)
}

#[test]
fn the_check_passes_against_the_check_builds_own_code_and_the_board_comes_back() {
    let (outcome, resets, lines, board) = rehearse(Faults::default(), quick());
    assert_eq!(outcome.verdict, Verdict::Passed, "{lines:#?}");
    assert!(outcome.back.is_some());
    assert_eq!(resets.len(), 1);
    assert_eq!(
        resets[0].1,
        Reset::Soc,
        "the END's report takes the SoC reset"
    );
    let words = outcome.record.expect("the record was read");
    let console: Vec<String> = board.console().into_iter().map(|(_, l)| l).collect();
    assert!(b2::console_agrees(&b2::console_words(&console), &words).is_empty());
    // The endpoint 0 trace is printed at the end, with the board's core revision's sequence in it: the
    // OUT status stage armed (flag 2) after an IN data stage completed.
    assert!(console.iter().any(|l| l.starts_with("b2 ep0-trace ")));
    assert!(
        console
            .iter()
            .any(|l| l.starts_with("b2 t ")
                && l.split(' ').nth(3).is_some_and(|h| h.ends_with("0206"))),
        "{console:#?}"
    );
    assert!(board.unexpected().is_empty(), "{:?}", board.unexpected());
    assert_eq!(board.pad_writes_refused(), 0);
    let back = outcome.back.unwrap().0;
    let stage = outcome.stage.unwrap();
    assert!(back.devnum != stage.devnum && back.devnum != lemnos_k230_usbboot::b2sim::ROM_DEVNUM);
    board.stop();
}

#[test]
fn a_core_from_4_20a_resets_by_its_done_bit_and_the_check_passes() {
    let faults = Faults {
        core: CoreRev::V420a,
        ..Faults::default()
    };
    let (outcome, _, lines, board) = rehearse(faults, quick());
    assert_eq!(outcome.verdict, Verdict::Passed, "{lines:#?}");
    assert_eq!(
        outcome.record.unwrap()[lemnos_k230_usbboot_stage::fl1::check::record::index("reset-done")],
        2
    );
    board.stop();
}

#[test]
fn a_bit_flipped_host_to_device_is_caught_by_the_stages_crc() {
    let faults = Faults {
        corrupt_out: Some(3),
        ..Faults::default()
    };
    let (outcome, resets, lines, board) = rehearse(faults, quick());
    assert!(
        matches!(&outcome.verdict, Verdict::Failed(f) if f.iter().any(|t| t.contains("status 1"))),
        "{lines:#?}"
    );
    assert_eq!(resets[0].1, Reset::Soc);
    assert!(outcome.back.is_some());
    board.stop();
}

#[test]
fn a_bit_flipped_device_to_host_is_caught_by_the_hosts_crc() {
    let faults = Faults {
        corrupt_in: Some(3),
        ..Faults::default()
    };
    let (outcome, resets, lines, board) = rehearse(faults, quick());
    assert!(
        matches!(&outcome.verdict, Verdict::Failed(f) if f.iter().any(|t| t.contains("CRC"))),
        "{lines:#?}"
    );
    assert_eq!(
        resets[0].1,
        Reset::Soc,
        "the END after the failure still resets"
    );
    assert!(outcome.back.is_some());
    board.stop();
}

/// The model fails like the hardware: a driver that never enables endpoint 0 OUT again (the old driver's
/// fault) cannot be enumerated, and the host gives up at its bound.
#[test]
fn the_model_cannot_enumerate_a_driver_that_never_rearms_endpoint_0_out() {
    let faults = Faults {
        no_ep0_rearm: true,
        ..Faults::default()
    };
    let bounds = Bounds {
        back: Duration::from_millis(200),
        ..quick()
    };
    let (outcome, _, lines, board) = rehearse(faults, bounds);
    assert!(matches!(outcome.verdict, Verdict::GaveUp(_)), "{lines:#?}");
    assert!(
        board.kernel_log().iter().any(|(_, l)| l.contains("failed")),
        "{:?}",
        board.kernel_log()
    );
    board.stop();
}

/// B2 run 1's board: `time` reads 0 and stays there until the stage writes the system timer's enables.
/// The stage starts it, checks it against the watchdog's counter, and the check passes.
#[test]
fn a_timebase_stopped_in_rom_state_is_started_and_checked_and_the_check_passes() {
    let faults = Faults {
        time: TimeCsr::StoppedUntilEnabled,
        ..Faults::default()
    };
    let (outcome, resets, lines, board) = rehearse(faults, quick());
    assert_eq!(outcome.verdict, Verdict::Passed, "{lines:#?}");
    assert_eq!(resets.len(), 1);
    assert_eq!(resets[0].1, Reset::Soc);
    let console: Vec<String> = board.console().into_iter().map(|(_, l)| l).collect();
    assert!(
        !console
            .iter()
            .any(|l| l == NO_CLOCK_LINE || l == TIMEBASE_STOPPED_LINE)
    );
    let words = outcome.record.expect("the record was read");
    let w = |name: &str| words[lemnos_k230_usbboot_stage::fl1::check::record::index(name)];
    assert!(w("tb-cross-counts") >= lemnos_k230_usbboot_stage::fl1::check::CROSS_COUNTS);
    assert_ne!(w("tb-cross-us"), 0);
    board.stop();
}

/// A timebase the enables do not start: the stage says so on the console, never touches USB, and takes the
/// SoC reset; the host gives up at its bound and sees the boot ROM back.
#[test]
fn a_timebase_that_never_moves_stops_the_stage_before_usb_and_resets() {
    let faults = Faults {
        time: TimeCsr::Dead,
        ..Faults::default()
    };
    let bounds = Bounds {
        back: Duration::from_secs(10),
        ..quick()
    };
    let (outcome, resets, lines, board) = rehearse(faults, bounds);
    assert!(matches!(outcome.verdict, Verdict::GaveUp(_)), "{lines:#?}");
    assert!(outcome.stage.is_none());
    assert!(outcome.back.is_some(), "{lines:#?}");
    assert_eq!(resets.len(), 1);
    assert_eq!(resets[0].1, Reset::Soc);
    let console: Vec<String> = board.console().into_iter().map(|(_, l)| l).collect();
    assert!(
        console.iter().any(|l| l == TIMEBASE_STOPPED_LINE),
        "{console:#?}"
    );
    board.stop();
}

/// A ROM that left `time` running: the enables change nothing and the check passes.
#[test]
fn a_timebase_already_running_passes_the_same_way() {
    let faults = Faults {
        time: TimeCsr::Running,
        ..Faults::default()
    };
    let (outcome, _, lines, board) = rehearse(faults, quick());
    assert_eq!(outcome.verdict, Verdict::Passed, "{lines:#?}");
    board.stop();
}

/// A board with one device at the port and an `open` that must not be reached.
struct Fake {
    seen: Vec<Seen>,
    access: Access,
    opened: Rc<Cell<bool>>,
}

impl Board for Fake {
    fn scan(&mut self) -> Vec<Seen> {
        self.seen.clone()
    }
    fn access(&mut self, _: &Seen) -> Access {
        self.access.clone()
    }
    fn open(&mut self, _: &Seen) -> Result<Box<dyn Link>, String> {
        self.opened.set(true);
        Err("the fake never opens".to_string())
    }
}

fn stage_at(port: &str, product: &str) -> Seen {
    Seen {
        port: port.to_string(),
        vid: 0x1209,
        pid: 0x0001,
        product: Some(product.to_string()),
        interface: Some(
            std::str::from_utf8(&lemnos_flash_wire::usb::INTERFACE_NAME)
                .unwrap()
                .to_string(),
        ),
        speed: Some("480".to_string()),
        configuration: Some(1),
        busnum: 3,
        devnum: 50,
    }
}

fn fake_run(seen: Vec<Seen>, access: Access, bounds: Bounds) -> (Verdict, bool, Duration) {
    let opened = Rc::new(Cell::new(false));
    let mut board = Fake {
        seen,
        access,
        opened: opened.clone(),
    };
    let started = Instant::now();
    let outcome = b2::run(&mut board, &plan(bounds), None, started, &mut |_| {});
    (outcome.verdict, opened.get(), started.elapsed())
}

fn fast() -> Bounds {
    Bounds {
        enumerate: Duration::from_millis(300),
        back: Duration::from_millis(50),
        poll: Duration::from_millis(10),
        ..Bounds::BOARD
    }
}

#[test]
fn another_product_at_the_port_is_refused_and_never_opened() {
    let (verdict, opened, _) = fake_run(
        vec![stage_at(b2::PORT, "Something else")],
        Access::Granted,
        fast(),
    );
    assert!(matches!(verdict, Verdict::Refused(t) if t.contains("product")));
    assert!(!opened);
}

#[test]
fn another_interface_string_at_the_port_is_refused_and_never_opened() {
    let mut seen = stage_at(b2::PORT, lemnos_flash_wire::usb::PRODUCT);
    seen.interface = Some("lemnos flash wire 0000".to_string());
    let (verdict, opened, _) = fake_run(vec![seen], Access::Granted, fast());
    assert!(matches!(verdict, Verdict::Refused(t) if t.contains("interface")));
    assert!(!opened);
}

#[test]
fn the_stage_at_another_port_is_refused_at_the_bound_and_never_opened() {
    let (verdict, opened, took) = fake_run(
        vec![stage_at("3-4.2.1.4.2", lemnos_flash_wire::usb::PRODUCT)],
        Access::Granted,
        fast(),
    );
    assert!(matches!(verdict, Verdict::Refused(t) if t.contains("wrong port")));
    assert!(!opened);
    assert!(took >= Duration::from_millis(300));
}

#[test]
fn a_stage_that_never_shows_is_given_up_at_the_bound() {
    let (verdict, opened, took) = fake_run(Vec::new(), Access::Granted, fast());
    assert!(matches!(verdict, Verdict::GaveUp(_)));
    assert!(!opened);
    assert!(
        took >= Duration::from_millis(300) && took < Duration::from_millis(600),
        "{took:?}"
    );
}

#[test]
fn without_access_the_run_stops_and_says_so_and_never_opens() {
    let (verdict, opened, _) = fake_run(
        vec![stage_at(b2::PORT, lemnos_flash_wire::usb::PRODUCT)],
        Access::Denied("EACCES".to_string()),
        fast(),
    );
    assert!(
        matches!(&verdict, Verdict::Stopped(t) if t.contains("user's call") && t.contains("EACCES")),
        "{verdict:?}"
    );
    assert!(!opened);
}

#[test]
fn sysfs_is_read_as_the_kernel_writes_it() {
    let root = std::env::temp_dir().join(format!("b2-sysfs-{}", std::process::id()));
    let dev = root.join("3-4.2.1.4.1");
    std::fs::create_dir_all(dev.join("3-4.2.1.4.1:1.0")).unwrap();
    for (name, value) in [
        ("idVendor", "1209\n"),
        ("idProduct", "0001\n"),
        ("product", "Lemnos flash stage\n"),
        ("speed", "480\n"),
        ("bConfigurationValue", "1\n"),
        ("busnum", "3\n"),
        ("devnum", "121\n"),
    ] {
        std::fs::write(dev.join(name), value).unwrap();
    }
    std::fs::write(
        dev.join("3-4.2.1.4.1:1.0/interface"),
        "lemnos flash wire D0D1\n",
    )
    .unwrap();
    std::fs::create_dir_all(root.join("usb3")).unwrap();
    let mut sysfs = Sysfs::at(&root);
    let seen = sysfs.scan();
    std::fs::remove_dir_all(&root).unwrap();
    assert_eq!(seen.len(), 1);
    let s = &seen[0];
    assert_eq!((s.vid, s.pid, s.devnum, s.busnum), (0x1209, 1, 121, 3));
    assert_eq!(s.product.as_deref(), Some("Lemnos flash stage"));
    assert_eq!(s.interface.as_deref(), Some("lemnos flash wire D0D1"));
    assert_eq!(s.devnode(), "/dev/bus/usb/003/121");
}

#[test]
fn the_record_checks_name_each_bench_fact_that_does_not_hold() {
    use lemnos_k230_usbboot_stage::fl1::check::record;
    let seen = stage_at(b2::PORT, lemnos_flash_wire::usb::PRODUCT);
    let mut words = [0u32; record::WORDS];
    words[0] = record::MAGIC;
    // Full speed (ENUMSPD 1); every other word zero.
    words[record::index("speed")] = 1;
    let failures = b2::record_checks(&words, &seen);
    for fact in [
        "speed",
        "set-address",
        "status stage",
        "re-arms",
        "oversize chunk",
    ] {
        assert!(
            failures.iter().any(|f| f.contains(fact)),
            "{fact}: {failures:#?}"
        );
    }
    // DCFG must hold the address SET_ADDRESS gave.
    words[record::index("set-address")] = 17;
    words[record::index("dcfg-after-address")] = 5 << 4;
    let failures = b2::record_checks(&words, &seen);
    assert!(failures.iter().any(|f| f.contains("DCFG")), "{failures:#?}");
    // Under xHCI the address is the host controller's, not the kernel's device number (here 50):
    // that difference is not a failure.
    words[record::index("dcfg-after-address")] = 17 << 4;
    let failures = b2::record_checks(&words, &seen);
    assert!(
        !failures
            .iter()
            .any(|f| f.contains("DCFG") || f.contains("set-address")),
        "{failures:#?}"
    );
}

/// A sysfs tree whose hub at `3-4.2.1.4` has a `product` that blocks on read, as a hub's string
/// attributes do while the kernel enumerates a device on one of its ports (they take the hub's device
/// lock, which the hub's enumeration holds for seconds: B2 run 2's 15 s bound ended at 30.7 s). The
/// stand-in is a FIFO with no writer: a read of it blocks until one opens it.
struct SlowTree {
    root: std::path::PathBuf,
    fifos: Vec<std::path::PathBuf>,
}

impl SlowTree {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("b2-slow-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let hub = root.join("3-4.2.1.4");
        std::fs::create_dir_all(&hub).unwrap();
        for (name, value) in [
            ("idVendor", "05e3\n"),
            ("idProduct", "0610\n"),
            ("busnum", "3\n"),
            ("devnum", "20\n"),
            ("speed", "480\n"),
        ] {
            std::fs::write(hub.join(name), value).unwrap();
        }
        let mut tree = Self {
            root,
            fifos: Vec::new(),
        };
        tree.fifo(&hub.join("product"));
        tree.fifo(&hub.join("bConfigurationValue"));
        tree
    }

    fn fifo(&mut self, path: &std::path::Path) {
        let ok = std::process::Command::new("mkfifo")
            .arg(path)
            .status()
            .unwrap()
            .success();
        assert!(ok);
        self.fifos.push(path.to_path_buf());
    }

    /// The stage at the board's port, its `product` and `bConfigurationValue` blocking too (its own
    /// configuration by the kernel holds its lock).
    fn stage(&mut self) {
        let dev = self.root.join(b2::PORT);
        std::fs::create_dir_all(dev.join(format!("{}:1.0", b2::PORT))).unwrap();
        for (name, value) in [
            ("idVendor", "1209\n"),
            ("idProduct", "0001\n"),
            ("busnum", "3\n"),
            ("devnum", "121\n"),
            ("speed", "480\n"),
        ] {
            std::fs::write(dev.join(name), value).unwrap();
        }
        std::fs::write(
            dev.join(format!("{}:1.0/interface", b2::PORT)),
            "lemnos flash wire D0D1\n",
        )
        .unwrap();
        self.fifo(&dev.join("product"));
        self.fifo(&dev.join("bConfigurationValue"));
    }

    /// Lets every blocked read finish with `value` (the lock released).
    fn release(&self, path: &std::path::Path, value: &str) {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        // Non-blocking open for writing succeeds only while a reader waits; retry briefly.
        for _ in 0..200 {
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .write(true)
                .custom_flags(0o4000) // O_NONBLOCK
                .open(path)
            {
                let _ = f.write_all(value.as_bytes());
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for SlowTree {
    fn drop(&mut self) {
        for fifo in &self.fifos {
            self.release(fifo, "\n");
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Runs the host check over the sysfs tree at `root` on its own thread; `None` when it did not end
/// within `limit` (the scan blocked).
fn run_over_sysfs(
    root: std::path::PathBuf,
    bounds: Bounds,
    limit: Duration,
) -> Option<(Verdict, Duration)> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut board = Sysfs::at(root);
        let started = Instant::now();
        let outcome = b2::run(&mut board, &plan(bounds), None, started, &mut |_| {});
        let _ = tx.send((outcome.verdict, started.elapsed()));
    });
    rx.recv_timeout(limit).ok()
}

/// The 15 s bound must hold while the kernel retries enumeration under a hub whose attributes block.
#[test]
fn the_stage_wait_ends_at_its_bound_while_the_hubs_attributes_block() {
    let tree = SlowTree::new("hub");
    let result = run_over_sysfs(tree.root.clone(), fast(), Duration::from_secs(5));
    let (verdict, took) = result.expect("the host check did not end: the scan blocked");
    assert!(matches!(verdict, Verdict::GaveUp(_)), "{verdict:?}");
    assert!(took < Duration::from_millis(900), "{took:?}");
}

/// A stage whose own string attribute blocks for a while is taken once it can be read, within the
/// bound; it is never taken on its ids alone.
#[test]
fn a_stage_whose_attributes_block_for_a_while_is_taken_once_they_read() {
    let mut tree = SlowTree::new("stage");
    tree.stage();
    let product = tree.root.join(b2::PORT).join("product");
    let config = tree.root.join(b2::PORT).join("bConfigurationValue");
    let bounds = Bounds {
        enumerate: Duration::from_secs(3),
        ..fast()
    };
    let root = tree.root.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        // The host check stops at the open (no device node in a test tree): what matters is that the
        // stage was seen, with its product, before the bound.
        let mut board = Sysfs::at(root);
        let started = Instant::now();
        let mut lines = Vec::new();
        let outcome = b2::run(&mut board, &plan(bounds), None, started, &mut |t| {
            lines.push(t.to_string())
        });
        let _ = tx.send((outcome, lines, started.elapsed()));
    });
    std::thread::sleep(Duration::from_millis(1000));
    tree.release(&product, "Lemnos flash stage\n");
    tree.release(&config, "1\n");
    let (outcome, lines, took) = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("the host check did not end");
    assert!(outcome.stage.is_some(), "{lines:#?}");
    assert!(took < Duration::from_secs(3), "{took:?}");
    assert!(
        outcome.stage_after_s.unwrap_or(0.0) >= 0.9,
        "taken before its product could be read: {lines:#?}"
    );
}
