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
    let mut consoles: Vec<String> = Vec::new();
    let mut sd_controller: Option<u32> = None;
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
            "--console" => consoles.push(value()),
            "--sd-controller" => {
                let v = value();
                sd_controller = Some(
                    v.parse()
                        .unwrap_or_else(|_| usage("--sd-controller is 0 or 1")),
                );
            }
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
        // R1: the watchdog. Re-derived 2026-11-21 from the stage's accepted WDT clock range (wdt.rs: 1 MHz to 100 MHz):
        // TOP 11 is 2^27 cycles, 1.342178 s at 100 MHz (the earliest it can fire) and 134.217728 s at 1 MHz (the latest).
        // The old 100 s floor assumed 1 MHz only. The return is held to the user's 200 s bound.
        "report" => (
            Bounds {
                disappear_max: 134.217728,
                disappear_min: 1.342178,
                return_max: 200.0,
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
    // R3: a controller named on the command line, or none (the report then does not read the card).
    let sd = sd_controller.unwrap_or(SD_NONE);
    if let Err(e) = patch(&mut bytes, mode_bits, sd) {
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
    // The controller as sent, not a fixed word: R3's records said "sd none" for runs that did name one.
    let sd_text = sd_controller.map_or_else(|| "none".to_string(), |c| format!("controller {c}"));
    rec.note(format!(
        "mode {mode} = {mode_bits:#x}, sd {sd_text}, load {LOAD_ADDRESS:#010x}, port {port}"
    ));

    let mut io =
        RusbIo::open_port(Some(&port)).unwrap_or_else(|e| usage(&format!("opening {port}: {e}")));
    // The consoles, read only, from before the first transfer: a reader that cannot be opened is noted and skipped.
    let readers: Vec<lemnos_k230_usbboot::console::Console> = consoles
        .iter()
        .filter_map(
            |c| match lemnos_k230_usbboot::console::Console::open_readonly(c) {
                Ok(reader) => Some(reader),
                Err(e) => {
                    eprintln!("console {c}: {e} (skipped)");
                    None
                }
            },
        )
        .collect();
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
    let start_instant = Instant::now();
    let start = io.control_out(PROG_START, high, low, &[]);
    rec.note(format!(
        "start request sent at t0 = {t0:.3} s: {start:?} (its result is not trusted)"
    ));

    // The absence of the board is counted from the start, in the record window and in the poll alike.
    let mut disappeared: Option<f64> = None;
    // R1 reads the records the stage sends while it is up, for a few seconds.
    if reads_records {
        io.set_timeout_ms(500);
        let until = Instant::now() + Duration::from_secs(5);
        let mut count = 0;
        let mut errors = 0;
        while Instant::now() < until && disappeared.is_none() {
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
                    // R1e: a read error with the port gone from sysfs is the board leaving, at this instant.
                    let at = rec.started.elapsed().as_secs_f64() - t0;
                    if absent_at(false, port_present(&port)) {
                        rec.note(format!(
                            "the device is gone from {port} at {at:.3} s after the start (seen in the record window)"
                        ));
                        disappeared = Some(at);
                    }
                }
            }
        }
        rec.note(format!(
            "{count} record packets read after the start ({errors} read errors)"
        ));
    }
    drop(io);

    // The poll: one fresh open per tick, by port path, and an identity read on each open that succeeds.
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

    // The report's own end, from the consoles (read only): the line it prints before its SoC reset.
    let lines: Vec<lemnos_k230_usbboot::console::Line> =
        readers.iter().flat_map(|c| c.drain()).collect();
    // The console is read only here, by this probe alone (one reader per port): every line goes to the record, stamped
    // on CLOCK_REALTIME, so no separate capture is needed and no port has two readers.
    for line in &lines {
        rec.log
            .push(format!("{:.6} console {}", epoch_of(line.at), line.text));
    }
    let end_at =
        lemnos_k230_usbboot::console::banner_at(&lines, start_instant, "lemnos stage: end");
    if let Some(e) = end_at {
        rec.note(format!(
            "the report printed its end at {e:.3} s after the start request"
        ));
    }
    let back_ref = returned.as_ref().map(|(t, text)| (*t, text.as_str()));
    let outcome = judge(end_at, disappeared, back_ref, &bounds);
    match &outcome {
        Ok(()) => rec.note("the run's outcome holds its step's rule".into()),
        Err(reason) => rec.note(format!("STOP: {reason}")),
    }
    let pass = outcome.is_ok();
    rec.note(format!("verdict: {}", if pass { "PASS" } else { "STOP" }));
    finish(&record_path, &rec.log, pass);
}

/// The run's outcome, from what was seen. A report that printed its end (`lemnos stage: end`) finishes with its own
/// SoC reset: it passes when the board left within 5 s of that line and came back as K230 within 10 s of it. A run that
/// never printed its end is held to the step's watchdog bounds. A board that does not return as K230 fails either way.
fn judge(
    end_at: Option<f64>,
    gone: Option<f64>,
    back: Option<(f64, &str)>,
    bounds: &Bounds,
) -> Result<(), String> {
    let (gone, (back, text)) = match (gone, back) {
        (None, _) => return Err("the device never left the port after the start".into()),
        (Some(_), None) => {
            return Err(format!(
                "no return to the port within {} s after the start",
                bounds.return_max
            ));
        }
        (Some(g), Some(b)) => (g, b),
    };
    if text != "K230" {
        return Err(format!("the return answered {text:?}, not K230"));
    }
    if let Some(e) = end_at {
        if gone >= e - 0.5 && gone <= e + 5.0 && back <= e + 10.0 {
            return Ok(());
        }
        return Err(format!(
            "the report ended at {e:.3} s, the board left at {gone:.3} s and came back at {back:.3} s: outside its own reset window"
        ));
    }
    if gone < bounds.disappear_min || gone > bounds.disappear_max {
        return Err(format!(
            "the board left at {gone:.3} s, outside the step's bound {}..{} s",
            bounds.disappear_min, bounds.disappear_max
        ));
    }
    if back > bounds.return_max {
        return Err(format!(
            "the return at {back:.3} s is past the step's bound of {} s",
            bounds.return_max
        ));
    }
    Ok(())
}

/// A monotonic instant as CLOCK_REALTIME seconds (the same clock as the record's lines).
fn epoch_of(at: Instant) -> f64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    now - Instant::now().saturating_duration_since(at).as_secs_f64()
}

/// Whether the port is still in sysfs (the device's `idVendor` is there). Reads sysfs only; nothing is opened.
fn port_present(port: &str) -> bool {
    Path::new(&format!("/sys/bus/usb/devices/{port}/idVendor")).exists()
}

/// The board is absent at a sample when its read failed and the port is gone.
fn absent_at(read_ok: bool, present: bool) -> bool {
    !read_ok && !present
}

/// The first sample, as (seconds after the start, read ok, port present), at which the board was absent.
#[cfg_attr(not(test), allow(dead_code))]
fn first_absence(samples: &[(f64, bool, bool)]) -> Option<f64> {
    samples
        .iter()
        .find(|(_, ok, present)| absent_at(*ok, *present))
        .map(|(at, _, _)| *at)
}

fn finish(record: &Path, log: &[String], pass: bool) -> ! {
    if let Ok(mut f) = File::create(record) {
        for line in log {
            let _ = writeln!(f, "{line}");
        }
    }
    std::process::exit(if pass { 0 } else { 1 });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report_bounds() -> Bounds {
        Bounds {
            disappear_max: 134.217728,
            disappear_min: 1.342178,
            return_max: 200.0,
        }
    }

    #[test]
    fn a_report_that_printed_its_end_and_reset_itself_passes() {
        // R1f: the end line at 0.38 s, the board left at 0.88 s and came back as K230 at 1.18 s.
        assert!(
            judge(
                Some(0.38),
                Some(0.88),
                Some((1.18, "K230")),
                &report_bounds()
            )
            .is_ok()
        );
    }

    #[test]
    fn a_run_that_never_printed_its_end_is_held_to_the_watchdog_bound() {
        // No end line: a departure at 0.9 s is outside 1.342..134.2 s, so the run stops even though the board returned.
        let reason = judge(None, Some(0.9), Some((1.2, "K230")), &report_bounds()).unwrap_err();
        assert!(reason.contains("outside the step's bound"), "{reason}");
        // The same departure inside the bound passes.
        assert!(judge(None, Some(60.0), Some((80.0, "K230")), &report_bounds()).is_ok());
    }

    #[test]
    fn a_report_end_followed_by_a_return_that_is_not_k230_fails() {
        assert!(
            judge(
                Some(0.38),
                Some(0.88),
                Some((1.18, "boot")),
                &report_bounds()
            )
            .is_err()
        );
    }

    #[test]
    fn a_departure_long_after_the_end_is_not_the_reports_own_reset() {
        assert!(
            judge(
                Some(0.38),
                Some(30.0),
                Some((31.0, "K230")),
                &report_bounds()
            )
            .is_err()
        );
    }

    #[test]
    fn a_failed_read_with_the_port_gone_is_an_absence() {
        assert!(absent_at(false, false));
    }

    #[test]
    fn a_failed_read_with_the_port_still_there_is_not_an_absence() {
        assert!(
            !absent_at(false, true),
            "a timeout on a present device is not the board leaving"
        );
    }

    #[test]
    fn a_successful_read_is_never_an_absence() {
        assert!(!absent_at(true, false));
    }

    #[test]
    fn the_first_absence_in_the_record_window_is_the_one_reported() {
        // Read errors on a present port, then the board leaves at 0.9 s and the poll later samples too.
        let samples = [
            (0.5, false, true),
            (0.7, false, true),
            (0.9, false, false),
            (1.1, true, true),
        ];
        assert_eq!(first_absence(&samples), Some(0.9));
    }

    #[test]
    fn no_absence_in_the_samples_is_none() {
        assert_eq!(
            first_absence(&[(0.5, false, true), (1.0, true, true)]),
            None
        );
    }
}
