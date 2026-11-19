//! The B2 run (`testing/board-runs/K230-USB.md`, "B2"): the FL1 stage's own USB device path, first run.
//!
//! ```text
//! k230-b2 run --image B2.bin --record FILE [--console TTY]... [--port P] [--dry]
//! k230-b2 rehearse CASE --record FILE
//! ```
//!
//! `run` holds the board's lock, loads the check build (`--b2` image, patched to `check` mode) through
//! the boot ROM at the port, reads the consoles read-only, and runs the host check (`lemnos_k230_usbboot::b2`).
//! `--dry` reads sysfs and the image and prints what would be done; it takes no lock and opens nothing.
//!
//! `rehearse` runs the same host check against the rehearsal's board (`lemnos_k230_usbboot::b2sim`): the
//! check build's own code over a register model. The cases: `good`, `no-access`, `wrong-product`,
//! `wrong-port`, `never-enumerates`, `corrupt-out`, `corrupt-in`, `rev-420a`, `no-ep0-rearm`.
//!
//! Exit 0 only when the check passed (or the rehearsal's case ended as it must) and the boot ROM came back.

#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lemnos_k230_usbboot::b2::{self, Board, Plan, Verdict, usb::Sysfs};
use lemnos_k230_usbboot::b2sim::{Faults, Reset, SimBoard};
use lemnos_k230_usbboot::bootrom::{CPU_INFO_LEN, PAGE, PROG_START, SET_DATA_ADDRESS};
use lemnos_k230_usbboot::console::Console;
use lemnos_k230_usbboot::transport::UsbIo;
use lemnos_k230_usbboot::usb::RusbIo;
use lemnos_k230_usbboot_stage::image::{LOAD_ADDRESS, SD_NONE, mode_word, patch};
use sha2::{Digest, Sha256};

fn die(message: &str) -> ! {
    eprintln!("k230-b2: {message}");
    std::process::exit(2);
}

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// The record: every line on stdout and in the file, with the wall time.
struct Record(Arc<Mutex<std::fs::File>>);

impl Record {
    fn open(path: &str) -> Self {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap_or_else(|e| die(&format!("the record {path}: {e}")));
        Self(Arc::new(Mutex::new(file)))
    }

    fn note(&self, text: &str) {
        let line = format!("{:.6} {text}", now());
        println!("{line}");
        let mut file = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let _ = writeln!(file, "{line}");
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let command = args.next().unwrap_or_else(|| die("run or rehearse"));
    let case = if command == "rehearse" {
        Some(args.next().unwrap_or_else(|| die("rehearse needs a case")))
    } else {
        None
    };
    let mut opt = std::collections::HashMap::new();
    let mut consoles = Vec::new();
    let mut dry = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--dry" => dry = true,
            "--console" => consoles.push(
                args.next()
                    .unwrap_or_else(|| die("--console needs a value")),
            ),
            name if name.starts_with("--") => {
                let value = args
                    .next()
                    .unwrap_or_else(|| die(&format!("{name} needs a value")));
                opt.insert(name[2..].to_string(), value);
            }
            other => die(&format!("unknown argument {other}")),
        }
    }
    let record = Record::open(
        opt.get("record")
            .map(String::as_str)
            .unwrap_or_else(|| die("--record is required")),
    );
    let ok = match (command.as_str(), case) {
        ("rehearse", Some(case)) => rehearse(&case, &record),
        ("run", None) => board_run(&opt, &consoles, dry, &record),
        _ => die("run or rehearse"),
    };
    std::process::exit(if ok { 0 } else { 1 });
}

/// What a rehearsal case sets, and the verdict it must end with.
fn case_faults(case: &str) -> (Faults, fn(&Verdict) -> bool) {
    let mut f = Faults::default();
    let want: fn(&Verdict) -> bool = match case {
        "good" | "rev-420a" => |v| *v == Verdict::Passed,
        "no-access" => |v| matches!(v, Verdict::Stopped(t) if t.contains("may not open")),
        "wrong-product" => |v| matches!(v, Verdict::Refused(t) if t.contains("product")),
        "wrong-port" => |v| matches!(v, Verdict::Refused(t) if t.contains("wrong port")),
        "never-enumerates" | "no-ep0-rearm" => |v| matches!(v, Verdict::GaveUp(_)),
        "corrupt-out" => {
            |v| matches!(v, Verdict::Failed(f) if f.iter().any(|t| t.contains("status 1")))
        }
        "corrupt-in" => |v| matches!(v, Verdict::Failed(f) if f.iter().any(|t| t.contains("CRC"))),
        other => die(&format!("unknown case {other}")),
    };
    match case {
        "rev-420a" => f.rev_420a = true,
        "no-access" => f.access_denied = true,
        "wrong-product" => f.product = Some("Lemnos flash stage (other)".to_string()),
        "wrong-port" => f.port = Some("3-4.2.1.4.2".to_string()),
        "never-enumerates" => f.hang = true,
        // Packet 3 is the first chunk's first packet (identify 1, write request 2).
        "corrupt-out" => f.corrupt_out = Some(3),
        // Packet 3 is the first read reply's first packet (identity 1, write status 2).
        "corrupt-in" => f.corrupt_in = Some(3),
        "no-ep0-rearm" => f.no_ep0_rearm = true,
        _ => {}
    }
    (f, want)
}

fn rehearse(case: &str, record: &Record) -> bool {
    let (faults, want) = case_faults(case);
    record.note(&format!("rehearsal {case}: faults {faults:?}"));
    record.note("the board: the check build's code (lemnos_driver_k230_usbboot_sys::b2::check and end, the dwc2 driver, the FL1 device and session) over the register model; the host: lemnos_k230_usbboot::b2::run, the code the board run uses");
    let mut board = SimBoard::new(faults);
    let rom = board.rom();
    record.note(&format!("before the load: {rom:?}"));
    let plan = Plan {
        outside_window: true,
        ..Plan::board()
    };
    record.note(&format!("plan: {plan:?}"));
    let started = board.load_and_start();
    let outcome = b2::run(&mut board, &plan, rom.as_ref(), started, &mut |t| {
        record.note(&format!("host: {t}"))
    });
    std::thread::sleep(Duration::from_millis(200));
    for (at, line) in board.kernel_log() {
        record.note(&format!("kernel {at:.3}: {line}"));
    }
    let console = board.console();
    for (at, line) in &console {
        record.note(&format!("console {at:.3}: {line}"));
    }
    for (at, reset) in board.resets() {
        record.note(&format!("board reset at {at:.3} s: {reset:?}"));
    }
    let unexpected = board.unexpected();
    for text in &unexpected {
        record.note(&format!("UNEXPECTED: {text}"));
    }
    record.note(&format!(
        "pad writes refused: {}",
        board.pad_writes_refused()
    ));
    let mut ok = want(&outcome.verdict) && outcome.back.is_some() && unexpected.is_empty();
    if let Some(words) = &outcome.record {
        let lines: Vec<String> = console.iter().map(|(_, l)| l.clone()).collect();
        let differ = b2::console_agrees(&b2::console_words(&lines), words);
        for text in &differ {
            record.note(&format!("console and USB record differ: {text}"));
        }
        if differ.is_empty() {
            record.note("the console's record agrees with the USB record on every stable word");
        }
        ok &= differ.is_empty();
    }
    let resets = board.resets();
    let expected_reset = match case {
        "never-enumerates" => Reset::Watchdog,
        _ => Reset::Soc,
    };
    ok &= resets.iter().all(|(_, r)| *r == expected_reset) && !resets.is_empty();
    record.note(&format!(
        "rehearsal {case}: {} (verdict {:?}; reset {:?}; back {:?})",
        if ok { "AS IT MUST" } else { "NOT AS IT MUST" },
        outcome.verdict,
        resets,
        outcome.back.as_ref().map(|(s, after)| (s.devnum, *after))
    ));
    board.stop();
    ok
}

fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Loads `bytes` at `address` through the boot ROM at `port` and starts there. Nothing is sent unless the
/// ROM answers `K230` first.
fn load_and_start(port: &str, address: u32, bytes: &[u8]) -> Result<(), String> {
    let mut io = RusbIo::open_port(Some(port)).map_err(|e| format!("opening {port}: {e}"))?;
    let reply = io
        .control_in(0, 0, 0, CPU_INFO_LEN)
        .map_err(|e| e.to_string())?;
    let who = String::from_utf8_lossy(&reply)
        .trim_matches(|c: char| c == '\0' || c.is_whitespace())
        .to_string();
    if who != "K230" {
        return Err(format!(
            "the device answers {who:?}, not K230: nothing loaded"
        ));
    }
    let (high, low) = ((address >> 16) as u16, (address & 0xffff) as u16);
    io.control_out(SET_DATA_ADDRESS, high, low, &[])
        .and_then(|()| bytes.chunks(PAGE).try_for_each(|page| io.bulk_out(page)))
        .map_err(|e| format!("the load failed: {e}"))?;
    // The start's own result is not trusted: the device leaves the bus under it.
    let _ = io.control_out(PROG_START, high, low, &[]);
    Ok(())
}

fn board_run(
    opt: &std::collections::HashMap<String, String>,
    console_paths: &[String],
    dry: bool,
    record: &Record,
) -> bool {
    let port = opt
        .get("port")
        .cloned()
        .unwrap_or_else(|| b2::PORT.to_string());
    let mut image = std::fs::read(
        opt.get("image")
            .unwrap_or_else(|| die("--image is required")),
    )
    .unwrap_or_else(|e| die(&format!("the image: {e}")));
    let mode = mode_word("check").unwrap_or_else(|| die("no check mode"));
    patch(&mut image, mode, SD_NONE).unwrap_or_else(|e| die(e));
    let plan = Plan {
        port: port.clone(),
        ..Plan::board()
    };
    record.note(&format!(
        "b2 run: image {} bytes sha256 {} (patched: mode check {mode:#x}) at {LOAD_ADDRESS:#010x}; plan {plan:?}",
        image.len(),
        sha(&image)
    ));
    let mut sysfs = Sysfs::default();
    let rom = sysfs.seen(&port);
    record.note(&format!("the port before the load: {rom:?}"));
    for other in sysfs
        .scan()
        .iter()
        .filter(|s| s.vid == 0x1209 && s.pid == 0x0001)
    {
        record.note(&format!(
            "already on the bus with the stage's ids: {other:?} (never opened)"
        ));
    }
    let Some(rom) = rom.filter(|s| (s.vid, s.pid) == (0x29f1, 0x0230)) else {
        record.note("STOP: the port does not show the boot ROM (29f1:0230)");
        return false;
    };
    if dry {
        record.note(&format!(
            "dry: access to the boot ROM's node {} (for the load): {:?}",
            rom.devnode(),
            sysfs.access(&rom)
        ));
        record.note("dry: no lock taken, nothing opened, nothing loaded");
        return true;
    }
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(b2::LOCK)
        .unwrap_or_else(|e| die(&format!("the lock {}: {e}", b2::LOCK)));
    if let Err(e) = rustix::fs::flock(&lock, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
        record.note(&format!(
            "STOP: the board's lock {} is held ({e})",
            b2::LOCK
        ));
        return false;
    }
    // Console lines are stamped from here (just before the load's start command).
    let opened = Instant::now();
    let consoles: Vec<(String, Console)> = console_paths
        .iter()
        .map(|p| {
            (
                p.clone(),
                Console::open_readonly(p).unwrap_or_else(|e| die(&format!("the console {p}: {e}"))),
            )
        })
        .collect();
    let mut lines: Vec<String> = Vec::new();
    let drain = |lines: &mut Vec<String>| {
        for (path, console) in &consoles {
            for line in console.drain() {
                record.note(&format!(
                    "console {path} +{:.3} s from the consoles opening: {}",
                    line.at.saturating_duration_since(opened).as_secs_f64(),
                    line.text
                ));
                lines.push(line.text);
            }
        }
    };
    drain(&mut lines);
    let started = Instant::now();
    if let Err(e) = load_and_start(&port, LOAD_ADDRESS, &image) {
        record.note(&format!("STOP: {e}"));
        return false;
    }
    record.note("loaded and started");
    let outcome = b2::run(&mut sysfs, &plan, Some(&rom), started, &mut |t| {
        record.note(&format!("host: {t}"))
    });
    std::thread::sleep(Duration::from_secs(2));
    drain(&mut lines);
    let mut ok = outcome.verdict == Verdict::Passed && outcome.back.is_some();
    if let Some(words) = &outcome.record {
        let differ = b2::console_agrees(&b2::console_words(&lines), words);
        for text in &differ {
            record.note(&format!("console and USB record differ: {text}"));
        }
        ok &= differ.is_empty();
    }
    record.note(&format!(
        "b2 run: {} (verdict {:?}; stage after {:?} s; back {:?})",
        if ok { "PASSED" } else { "NOT PASSED" },
        outcome.verdict,
        outcome.stage_after_s,
        outcome.back.as_ref().map(|(s, after)| (s.devnum, *after))
    ));
    drop(lock);
    ok
}
