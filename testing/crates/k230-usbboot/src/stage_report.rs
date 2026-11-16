//! The host's one attached flow for Stage L (the report). It stays in one process from the identify to the
//! return. Order: identify (the first action, so a keep-alive stopped seconds before is fine), load the
//! report image with the mode word patched in, start it, wait for the stage's identity, read the records,
//! wait for the watchdog's return (`K230` again), and write the run record.
//!
//! Nothing is written to storage, and the flow writes nothing to a pad: it sends the image, the start, and
//! reads. The console, when a path is given, is opened READ-ONLY by the caller and its lines are passed in.

use std::fmt::Write as _;
use std::time::{Duration, Instant};

/// The time the flow measures and waits with. The CLI uses [`RealTimer`]; the sim dry run uses the sim's clock,
/// so the wait for the watchdog's return runs in virtual time.
pub trait Timer {
    /// Seconds since the flow started.
    fn now_s(&self) -> f64;
    /// Waits.
    fn sleep(&mut self, d: Duration);
}

/// The wall clock.
pub struct RealTimer(pub Instant);

impl Timer for RealTimer {
    fn now_s(&self) -> f64 {
        self.0.elapsed().as_secs_f64()
    }
    fn sleep(&mut self, d: Duration) {
        std::thread::sleep(d);
    }
}

use lemnos_k230_usbboot_stage::format::{PACKET, STAGE_IDENT};
use lemnos_k230_usbboot_stage::image::{LOAD_ADDRESS, mode_word, patch, SD_NONE};
use lemnos_k230_usbboot_stage::telemetry::{self, Record};

use crate::bootrom::{PAGE, PROG_START, SET_DATA_ADDRESS, TEXT_BOOT_ROM};
use crate::transport::{UsbError, UsbIo};

/// The options of one run.
#[derive(Clone, Debug)]
pub struct Options {
    /// The stage image as built (`lemnos-k230-usbboot-image`'s `.bin`).
    pub image: Vec<u8>,
    /// The SD controller parameter (0 `sdhci0`, 1 `sdhci1`), or none: the stage does not choose one.
    pub sd_controller: Option<u32>,
    /// How long the stage may take to answer its identity after the start.
    pub identify_wait: Duration,
    /// How long the board may take to return after the records (the watchdog, `2^27` cycles at its clock).
    pub return_wait: Duration,
}

/// One line of the run record, with the time it was written from the run's start.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub at_s: f64,
    pub text: String,
}

/// What one run did and saw.
#[derive(Clone, Debug, Default)]
pub struct RunRecord {
    pub entries: Vec<Entry>,
    /// The decoded records, in arrival order.
    pub records: Vec<(f64, Record)>,
    /// The board's return to USB boot, in seconds from the start, when it came back `K230`.
    pub returned_s: Option<f64>,
}

impl RunRecord {
    fn note(&mut self, at_s: f64, text: impl Into<String>) {
        self.entries.push(Entry {
            at_s,
            text: text.into(),
        });
    }

    /// The record as text: the commands and results in order, then the decoded records, then the console.
    pub fn render(&self, console: &[(f64, String)]) -> String {
        let mut out = String::new();
        for e in &self.entries {
            let _ = writeln!(out, "{:10.3} {}", e.at_s, e.text);
        }
        let _ = writeln!(out, "--- records ({})", self.records.len());
        for (at, r) in &self.records {
            let _ = writeln!(out, "{at:10.3} {r:?}");
        }
        let _ = writeln!(out, "--- console ({} lines)", console.len());
        for (at, line) in console {
            let _ = writeln!(out, "{at:10.3} {line}");
        }
        out
    }
}

/// The function-select field of a pad word (`SEL`, bit 11 in `k230_evb.h`). Its width is UNVERIFIED: three bits
/// are taken here, and the raw word is printed beside it.
pub fn pad_sel(raw: u32) -> u32 {
    (raw >> 11) & 0x7
}

/// The SD function's select value, as the other board's pad table gives it (SEL=1 on IO26 to IO31). No source
/// for our SoC's mapping is in the SDK tree (`k230_evb.h` names the fields, not the function numbers), so a pad on
/// this value is "SD per the other board's table", which is not proof on this SoC.
pub const SD_SEL_OTHER_BOARD: u32 = 1;

/// The pads the report names: the SD pads and UART0's pads (`k230_evb.h`: IO38, IO39 for UART0).
pub const NAMED_PADS: [usize; 8] = [26, 27, 28, 29, 30, 31, 38, 39];

/// The pad table, as the run record prints it: every pad's raw word, its decoded single-bit fields, the bank bit
/// (`MSC`: 0 = 3V3, 1 = 1V8 per `K230_MSC_3V3`/`K230_MSC_1V8` in `k230_evb.h`), and, for the named pads, what the
/// ROM left selected. The reset default function of each pad is not stated in the SDK, so it is printed as unknown.
pub fn pad_table(pads: &[u32; 64]) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(out, "pad   raw       sel st pu pd oe ie msc(bank) sl ds  note");
    for (n, &raw) in pads.iter().enumerate() {
        let bit = |b: u32| (raw >> b) & 1;
        let note = if NAMED_PADS.contains(&n) {
            let sel = pad_sel(raw);
            if n >= 26 && n <= 31 {
                if sel == SD_SEL_OTHER_BOARD {
                    "SD function selected (other board's table value; not proof here)".to_string()
                } else {
                    format!("not the SD function (sel {sel}); reset default unknown")
                }
            } else if sel == 0 {
                "UART0 pad: function value unknown (reset default unknown)".to_string()
            } else {
                format!("UART0 pad: sel {sel} (UART function value not stated in the SDK)")
            }
        } else {
            String::new()
        };
        let _ = writeln!(
            out,
            "IO{n:<3} {raw:#010x} {:>3} {:>2} {:>2} {:>2} {:>2} {:>2} {:>3}({})   {:>2} {:>2}  {note}",
            pad_sel(raw),
            bit(0),
            bit(6),
            bit(5),
            bit(7),
            bit(8),
            bit(9),
            if bit(9) == 0 { "3V3" } else { "1V8" },
            bit(10),
            (raw >> 1) & 0x3,
        );
    }
    out
}

/// The identity the ROM or the stage gives, as text (trailing NULs and spaces stripped).
fn identity(io: &mut dyn UsbIo) -> Result<String, UsbError> {
    let reply = io.control_in(0, 0, 0, 32)?;
    Ok(String::from_utf8_lossy(&reply)
        .trim_matches(|c: char| c == '\0' || c.is_whitespace())
        .to_string())
}

/// Runs the flow. `console` is the console's lines (already captured read-only, with times from the run's
/// start), and is copied into the record by the caller's renderer.
pub fn run(io: &mut dyn UsbIo, timer: &mut dyn Timer, opts: &Options) -> Result<RunRecord, UsbError> {
    let mut rec = RunRecord::default();
    rec.note(timer.now_s(), format!("image bytes {} (report mode, sd parameter {:?})", opts.image.len(), opts.sd_controller));

    // 1. Identify first: the device must be the ROM, and must answer. A deaf ROM is named.
    let first = identity(io).map_err(|e| {
        rec.note(timer.now_s(), format!("identify failed: {e}"));
        e
    })?;
    rec.note(timer.now_s(), format!("identify: {first:?}"));
    if first != TEXT_BOOT_ROM {
        rec.note(timer.now_s(), "identify is not K230: the flow stops (nothing is loaded)");
        return Err(UsbError::Protocol(format!("the device answered {first:?}, not K230")));
    }

    // 2. Patch the report mode and the SD parameter into the image, and load it.
    let mut image = opts.image.clone();
    let mode = mode_word("report").expect("the report mode is named");
    let sd = opts.sd_controller.unwrap_or(SD_NONE);
    patch(&mut image, mode, sd).map_err(|e| UsbError::Other(e.to_string()))?;
    let (high, low) = ((LOAD_ADDRESS >> 16) as u16, (LOAD_ADDRESS & 0xffff) as u16);
    io.control_out(SET_DATA_ADDRESS, high, low, &[])?;
    for page in image.chunks(PAGE) {
        io.bulk_out(page)?;
    }
    rec.note(timer.now_s(), format!("loaded {} bytes at {LOAD_ADDRESS:#010x} in pages of {PAGE}", image.len()));

    // 3. The start. Its own result is recorded, not trusted: the identity decides.
    let started = io.control_out(PROG_START, high, low, &[]);
    rec.note(timer.now_s(), format!("start request: {started:?}"));

    // 4. The stage's identity, polled until it answers or the wait ends.
    let until = timer.now_s() + opts.identify_wait.as_secs_f64();
    loop {
        match identity(io) {
            Ok(text) if text == STAGE_IDENT => {
                rec.note(timer.now_s(), "stage identity answered");
                break;
            }
            Ok(text) => rec.note(timer.now_s(), format!("identify (waiting): {text:?}")),
            Err(e) => rec.note(timer.now_s(), format!("identify (waiting): {e}")),
        }
        if timer.now_s() >= until {
            rec.note(timer.now_s(), "the stage did not answer its identity in time");
            return Ok(rec);
        }
        timer.sleep(Duration::from_millis(200));
    }

    // 5. The records: read until the stage has no more (a timeout or the device leaving the bus).
    loop {
        match io.bulk_in(PACKET) {
            Ok(bytes) => match telemetry::decode(&bytes) {
                Some(record) => rec.records.push((timer.now_s(), record)),
                None => rec.note(timer.now_s(), format!("a packet that is not a record: {} bytes", bytes.len())),
            },
            Err(e) => {
                rec.note(timer.now_s(), format!("records end: {e}"));
                break;
            }
        }
    }

    // 6. The watchdog's return: the board comes back as the ROM, `K230`. Nothing is sent to bring it back.
    let until = timer.now_s() + opts.return_wait.as_secs_f64();
    while timer.now_s() < until {
        timer.sleep(Duration::from_millis(250));
        if let Ok(text) = identity(io) {
            if text == TEXT_BOOT_ROM {
                rec.returned_s = Some(timer.now_s());
                rec.note(timer.now_s(), "returned: K230");
                break;
            }
        }
    }
    if rec.returned_s.is_none() {
        rec.note(timer.now_s(), "no return within the wait: the board is left as it is");
    }
    Ok(rec)
}
