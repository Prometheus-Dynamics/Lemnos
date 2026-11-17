//! One image, one return, timed. The host's side of R0 (`soc` mode) and R1 (`report` mode) in the board plan
//! (`testing/board-runs/K230-USB.md`): identify the ROM, load the image at its load address with the named mode
//! and no SD controller, start it, then time the board's disappearance and its return to download mode, and
//! check them against the step's bounds. It reads no card, writes no console, and reads records only in
//! `report` mode, for a few seconds after the start.
//!
//! ```text
//! k230-return-probe --port 3-4.2.1.4.1 --image FILE --mode soc|report --record FILE
//! ```
//!
//! Exit 0 only when the step's pass rule holds; any other outcome is a stop, and the reason is in the record.

#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use lemnos_k230_usbboot::bootrom::{CPU_INFO_LEN, PAGE, PROG_START, SET_DATA_ADDRESS};
use lemnos_k230_usbboot::transport::UsbIo;
use lemnos_k230_usbboot::usb::RusbIo;
use lemnos_k230_usbboot_stage::format::PACKET;
use lemnos_k230_usbboot_stage::image::{LOAD_ADDRESS, SD_NONE, mode_word, patch};
use sha2::{Digest, Sha256};

fn usage(message: &str) -> ! {
    eprintln!("k230-return-probe: {message}");
    eprintln!(
        "usage: k230-return-probe --port BUS-PORT.PORT --image FILE --mode soc|report --record FILE"
    );
    std::process::exit(2);
}

/// Another process with the device's usbfs node open, as "pid N (cmdline)". Reads /proc only.
fn other_holder(port: &str) -> Option<String> {
    let base = format!("/sys/bus/usb/devices/{port}");
    let bus: u32 = std::fs::read_to_string(format!("{base}/busnum"))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    let dev: u32 = std::fs::read_to_string(format!("{base}/devnum"))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    let node = PathBuf::from(format!("/dev/bus/usb/{bus:03}/{dev:03}"));
    let me = std::process::id();
    for entry in std::fs::read_dir("/proc").ok()?.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        if pid == me {
            continue;
        }
        let Ok(fds) = std::fs::read_dir(entry.path().join("fd")) else {
            continue;
        };
        for fd in fds.flatten() {
            if std::fs::read_link(fd.path())
                .map(|t| t == node)
                .unwrap_or(false)
            {
                let cmd = std::fs::read(entry.path().join("cmdline")).unwrap_or_default();
                return Some(format!(
                    "pid {pid} ({})",
                    String::from_utf8_lossy(&cmd).replace('\0', " ").trim()
                ));
            }
        }
    }
    None
}

/// The ROM's (or the stage's) identity text: the 32-byte CPU info, NUL padded.
fn identity(io: &mut dyn UsbIo) -> Result<String, String> {
    let reply = io
        .control_in(0, 0, 0, CPU_INFO_LEN)
        .map_err(|e| e.to_string())?;
    Ok(String::from_utf8_lossy(&reply)
        .trim_matches(|c: char| c == '\0' || c.is_whitespace())
        .to_string())
}

/// The run's own clock and its lines, written to the record and printed as they are made.
struct Rec {
    started: Instant,
    log: Vec<String>,
}

impl Rec {
    fn note(&mut self, text: String) {
        // Absolute CLOCK_REALTIME (seconds since the epoch), the same clock the console capture stamps with, and
        // the run's own elapsed seconds.
        let epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        let line = format!(
            "{epoch:.6} +{:8.3} {text}",
            self.started.elapsed().as_secs_f64()
        );
        println!("{line}");
        self.log.push(line);
    }
}

/// The pass rule for each mode, and its bounds, from the plan's R0 and R1.
struct Bounds {
    /// The latest the board may disappear after the start, in seconds (`None`: no lower bound).
    disappear_max: f64,
    /// The earliest the board may disappear after the start, in seconds.
    disappear_min: f64,
    /// The latest the board may return after the start, in seconds.
    return_max: f64,
}

fn main() {
    let mut port: Option<String> = None;
    let mut image: Option<PathBuf> = None;
    let mut mode: Option<String> = None;
    let mut record: Option<PathBuf> = None;
    let mut identify_only = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || {
            args.next()
                .unwrap_or_else(|| usage(&format!("{arg} needs a value")))
        };
        match arg.as_str() {
            "--port" => port = Some(value()),
            "--image" => image = Some(PathBuf::from(value())),
            "--mode" => mode = Some(value()),
            "--record" => record = Some(PathBuf::from(value())),
            "--identify-only" => identify_only = true,
            other => usage(&format!("unknown argument {other}")),
        }
    }
    let port =
        port.unwrap_or_else(|| usage("--port is required (a port path, never VID:PID alone)"));
    let record_path = record.unwrap_or_else(|| usage("--record is required"));
    if identify_only {
        // One read-only identify, no load: the device's answer and the time it took, for the deaf-state record.
        let mut rec = Rec {
            started: Instant::now(),
            log: Vec::new(),
        };
        if let Some(holder) = other_holder(&port) {
            usage(&format!(
                "another process has the device at {port} open: {holder}; stop it first"
            ));
        }
        rec.note(format!("identify-only on {port}"));
        match RusbIo::open_port(Some(&port)) {
            Err(e) => {
                rec.note(format!("open failed: {e}"));
                finish(&record_path, &rec.log, false);
            }
            Ok(mut io) => {
                let answer = identity(&mut io);
                rec.note(format!("identify: {answer:?}"));
                finish(&record_path, &rec.log, answer.as_deref() == Ok("K230"));
            }
        }
    }
    let image_path = image.unwrap_or_else(|| usage("--image is required"));
    let mode = mode.unwrap_or_else(|| usage("--mode is soc or report"));
    let (bounds, reads_records) = match mode.as_str() {
        // R0: back within 10 s; the disappearance is at the start (within 2 s).
        "soc" => (
            Bounds {
                disappear_max: 2.0,
                disappear_min: 0.0,
                return_max: 10.0,
            },
            false,
        ),
        // R1: the watchdog; the disappearance is 100 s to 420 s after the start, the return by 420 s.
        "report" => (
            Bounds {
                disappear_max: 420.0,
                disappear_min: 100.0,
                return_max: 420.0,
            },
            true,
        ),
        _ => usage("--mode is soc or report"),
    };

    let mut rec = Rec {
        started: Instant::now(),
        log: Vec::new(),
    };

    if let Some(holder) = other_holder(&port) {
        usage(&format!(
            "another process has the device at {port} open: {holder}; stop it first"
        ));
    }
    let mut bytes =
        std::fs::read(&image_path).unwrap_or_else(|e| usage(&format!("reading the image: {e}")));
    let sha_in: String = Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let mode_bits = mode_word(&mode).expect("the mode is soc or report");
    if let Err(e) = patch(&mut bytes, mode_bits, SD_NONE) {
        usage(&format!("patching the mode block: {e}"));
    }
    let sha_out: String = Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    rec.note(format!(
        "image {} sha256 {sha_in} (before patch), sent {} bytes sha256 {sha_out}",
        image_path.display(),
        bytes.len()
    ));
    rec.note(format!(
        "mode {mode} = {mode_bits:#x}, sd none, load {LOAD_ADDRESS:#010x}, port {port}"
    ));

    let mut io =
        RusbIo::open_port(Some(&port)).unwrap_or_else(|e| usage(&format!("opening {port}: {e}")));
    let first = identity(&mut io).unwrap_or_else(|e| usage(&format!("identify: {e}")));
    rec.note(format!("identify before load: {first:?}"));
    if first != "K230" {
        rec.note("STOP: the device is not the ROM answering K230; nothing is loaded".into());
        finish(&record_path, &rec.log, false);
    }

    let (high, low) = ((LOAD_ADDRESS >> 16) as u16, (LOAD_ADDRESS & 0xffff) as u16);
    let loaded = io
        .control_out(SET_DATA_ADDRESS, high, low, &[])
        .and_then(|()| bytes.chunks(PAGE).try_for_each(|page| io.bulk_out(page)));
    if let Err(e) = loaded {
        rec.note(format!("STOP: the load failed: {e}"));
        finish(&record_path, &rec.log, false);
    }
    rec.note(format!("loaded {} bytes in pages of {PAGE}", bytes.len()));

    let t0 = rec.started.elapsed().as_secs_f64();
    let start = io.control_out(PROG_START, high, low, &[]);
    rec.note(format!(
        "start request sent at t0 = {t0:.3} s: {start:?} (its result is not trusted)"
    ));

    // R1 reads the records the stage sends while it is up, for a few seconds.
    if reads_records {
        io.set_timeout_ms(500);
        let until = Instant::now() + Duration::from_secs(5);
        let mut count = 0;
        let mut errors = 0;
        while Instant::now() < until {
            match io.bulk_in(PACKET) {
                Ok(packet) => {
                    count += 1;
                    if count <= 3 {
                        rec.note(format!(
                            "record {count}: {} bytes {:02x?}",
                            packet.len(),
                            &packet[..packet.len().min(16)]
                        ));
                    }
                }
                // R1d: a read error is not the end of the window: records are polled until the window closes, and
                // the first error is noted once.
                Err(e) => {
                    if errors == 0 {
                        rec.note(format!("first read error: {e}"));
                    }
                    errors += 1;
                }
            }
        }
        rec.note(format!(
            "{count} record packets read after the start ({errors} read errors)"
        ));
    }
    drop(io);

    // The poll: one fresh open per tick, by port path, and an identity read on each open that succeeds.
    let mut disappeared: Option<f64> = None;
    let mut returned: Option<(f64, String)> = None;
    let deadline = bounds.return_max + 5.0;
    while rec.started.elapsed().as_secs_f64() - t0 < deadline {
        std::thread::sleep(Duration::from_millis(200));
        let at = rec.started.elapsed().as_secs_f64() - t0;
        match RusbIo::open_port(Some(&port)) {
            Err(_) => {
                if disappeared.is_none() && at >= 0.0 {
                    disappeared = Some(at);
                    rec.note(format!(
                        "the device is gone from {port} at {at:.3} s after the start"
                    ));
                }
            }
            Ok(mut again) => {
                if disappeared.is_some() {
                    let text =
                        identity(&mut again).unwrap_or_else(|e| format!("identify failed: {e}"));
                    rec.note(format!(
                        "the device is back at {port} at {at:.3} s, identity {text:?}"
                    ));
                    returned = Some((at, text));
                    break;
                }
            }
        }
    }

    let pass = match (&disappeared, &returned) {
        (Some(gone), Some((back, text))) => {
            let gone_ok = *gone <= bounds.disappear_max && *gone >= bounds.disappear_min;
            let back_ok = *back <= bounds.return_max && text == "K230";
            rec.note(format!(
                "disappeared at {gone:.3} s (bound {}..{} s): {}; returned at {back:.3} s (bound {} s), identity {text:?}: {}",
                bounds.disappear_min,
                bounds.disappear_max,
                if gone_ok { "within" } else { "OUT" },
                bounds.return_max,
                if back_ok { "within" } else { "OUT" },
            ));
            gone_ok && back_ok
        }
        (None, _) => {
            rec.note(format!("STOP: the device never disappeared from {port} after the start (the start did not reset it)"));
            false
        }
        (Some(_), None) => {
            rec.note(format!(
                "STOP: no return to {port} within {} s after the start",
                bounds.return_max
            ));
            false
        }
    };
    rec.note(format!("verdict: {}", if pass { "PASS" } else { "STOP" }));
    finish(&record_path, &rec.log, pass);
}

fn finish(record: &Path, log: &[String], pass: bool) -> ! {
    if let Ok(mut f) = File::create(record) {
        for line in log {
            let _ = writeln!(f, "{line}");
        }
    }
    std::process::exit(if pass { 0 } else { 1 });
}
