//! The card over the boot ROM's USB download mode, one load per piece (`testing/board-runs/K230-USB.md`,
//! the write half). Each piece is one ROM load: the stage image in `piece` mode at its load address and a
//! job beside it (`lemnos_k230_usbboot_stage::piece`). The stage arms its watchdog, does the job on the
//! card, prints the outcome on the console, and resets back to the ROM. This tool reads that outcome on
//! the consoles (read only), checks it against what it sent, and waits for the ROM to answer again
//! before the next piece.
//!
//! ```text
//! k230-card piece --op hash|zero|write|test --lba N --count N --extent FIRST,COUNT [--data FILE --data-offset BYTES]
//!                 [--expect-source FILE] [--expect-sha HEX]
//! k230-card flash --source FILE [--hold N,N] [--from N] [--to N] [--progress FILE]
//! k230-card load  --file FILE --offset BYTES --size BYTES --address HEX        (a raw load and start)
//! common: --port P --image STAGE.bin --cid HEX32 --blocks N --record FILE [--console TTY]... [--fast] [--dry]
//! ```
//!
//! Every piece declares the whole run's extent (FLASH-EXTENT): `piece` takes it as `--extent FIRST,COUNT`, and
//! `flash` declares the sectors it was asked to write, `--from` up to `--to`. The stage refuses a piece with no
//! declaration, a write, zero or test piece outside it, and an extent past the card. An older stage refuses every
//! piece this sends, with the refusal on the console.
//!
//! `--hold` names sectors written as zero instead of the source's content (the boot headers, so the card
//! stays one the ROM does not boot). `--dry` does everything but open the device and the consoles.
//! Exit 0 only when every piece ended `piece ok` with the digest expected and the ROM came back.

#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::time::{Duration, Instant};

use lemnos_k230_usbboot::bootrom::{CPU_INFO_LEN, PAGE, PROG_START, SET_DATA_ADDRESS};
use lemnos_k230_usbboot::console::Console;
use lemnos_k230_usbboot::transport::UsbIo;
use lemnos_k230_usbboot::usb::RusbIo;
use lemnos_k230_usbboot_stage::extent::Extent;
use lemnos_k230_usbboot_stage::format::crc32;
use lemnos_k230_usbboot_stage::image::{LOAD_ADDRESS, mode_word, patch};
use lemnos_k230_usbboot_stage::piece::{
    DATA_OFFSET, FAST, JOB_ADDRESS, Job, MAX_SECTORS, Op, TEST_SECTORS,
};
use sha2::{Digest, Sha256};

const SECTOR: usize = 512;
/// The most sectors one zero-fill job covers: bounds how long one load runs.
const ZERO_RUN: u32 = 16_384;

fn die(message: &str) -> ! {
    eprintln!("k230-card: {message}");
    std::process::exit(2);
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn sha(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// `--extent FIRST,COUNT` for a piece: the declared extent, in sectors (decimal or 0x hex).
fn extent_option(text: &str) -> Extent {
    let (first, count) = text
        .split_once(',')
        .unwrap_or_else(|| die("--extent is FIRST,COUNT"));
    let first =
        u32::try_from(number(first)).unwrap_or_else(|_| die("--extent: FIRST is past 2 TiB"));
    let count =
        u32::try_from(number(count)).unwrap_or_else(|_| die("--extent: COUNT is past 2 TiB"));
    Extent::new(first, count).unwrap_or_else(|e| die(&format!("--extent: {e}")))
}

fn number(text: &str) -> u64 {
    let parsed = match text.strip_prefix("0x") {
        Some(h) => u64::from_str_radix(h, 16),
        None => text.parse(),
    };
    parsed.unwrap_or_else(|_| die(&format!("not a number: {text}")))
}

/// Reads `count` sectors of `file` from sector `lba`, with the held sectors as zero.
fn source(file: &mut File, lba: u64, count: u32, hold: &[u64]) -> Vec<u8> {
    let mut data = vec![0u8; count as usize * SECTOR];
    file.seek(SeekFrom::Start(lba * SECTOR as u64))
        .and_then(|_| file.read_exact(&mut data))
        .unwrap_or_else(|e| die(&format!("reading the source at sector {lba}: {e}")));
    for held in hold {
        if *held >= lba && *held < lba + u64::from(count) {
            let at = (*held - lba) as usize * SECTOR;
            data[at..at + SECTOR].fill(0);
        }
    }
    data
}

struct Run {
    port: String,
    image: Vec<u8>,
    cid: [u8; 16],
    blocks: u64,
    fast: bool,
    dry: bool,
    consoles: Vec<Console>,
    record: File,
    pieces: u32,
}

/// What the console said about one piece.
#[derive(Default, Debug)]
struct Said {
    ok: bool,
    end: bool,
    sha256: Option<String>,
    before: Option<String>,
    after: Option<String>,
    retries: u32,
    trouble: Vec<String>,
}

fn read_said(lines: &[String]) -> Said {
    let mut said = Said::default();
    for line in lines {
        if line == "piece ok" {
            said.ok = true;
        } else if line.contains("lemnos stage: end") {
            said.end = true;
        } else if let Some(d) = line.strip_prefix("piece sha256 ") {
            said.sha256 = Some(d.to_string());
        } else if let Some(d) = line.strip_prefix("piece retries ") {
            said.retries = u32::from_str_radix(d, 16).unwrap_or(0);
        } else if let Some(d) = line.strip_prefix("piece before ") {
            said.before = Some(d.to_string());
        } else if let Some(d) = line.strip_prefix("piece after ") {
            said.after = Some(d.to_string());
        } else if line.contains("FAILED")
            || line.contains("refused")
            || line.contains("sd error")
            || line.contains("no reply")
            || line.contains("stack overflow")
        {
            said.trouble.push(line.clone());
        }
    }
    said
}

fn devnum(port: &str) -> Option<u32> {
    let base = format!("/sys/bus/usb/devices/{port}");
    let id = std::fs::read_to_string(format!("{base}/idVendor")).ok()?;
    let product = std::fs::read_to_string(format!("{base}/idProduct")).ok()?;
    if id.trim() != "29f1" || product.trim() != "0230" {
        return None;
    }
    std::fs::read_to_string(format!("{base}/devnum"))
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn identity(io: &mut dyn UsbIo) -> Result<String, String> {
    let reply = io
        .control_in(0, 0, 0, CPU_INFO_LEN)
        .map_err(|e| e.to_string())?;
    Ok(String::from_utf8_lossy(&reply)
        .trim_matches(|c: char| c == '\0' || c.is_whitespace())
        .to_string())
}

/// Loads `bytes` at `address` and starts there. Nothing is sent unless the ROM answers `K230` first.
fn load_and_start(port: &str, address: u32, bytes: &[u8]) -> Result<(), String> {
    let mut io = RusbIo::open_port(Some(port)).map_err(|e| format!("opening {port}: {e}"))?;
    let who = identity(&mut io)?;
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

/// Waits for the ROM to be back at `port` under a new device number and answering `K230`.
fn wait_back(port: &str, old: u32, seconds: f64) -> Result<f64, String> {
    let from = Instant::now();
    while from.elapsed().as_secs_f64() < seconds {
        std::thread::sleep(Duration::from_millis(100));
        if matches!(devnum(port), Some(n) if n != old)
            && let Ok(mut io) = RusbIo::open_port(Some(port))
            && identity(&mut io).as_deref() == Ok("K230")
        {
            return Ok(from.elapsed().as_secs_f64());
        }
    }
    Err(format!(
        "the ROM did not come back as K230 at {port} within {seconds} s"
    ))
}

impl Run {
    fn note(&mut self, text: &str) {
        let line = format!("{:.6} {text}", now());
        println!("{line}");
        let _ = writeln!(self.record, "{line}");
    }

    /// The bytes one load carries: the image in piece mode, zeros up to the job address, the header, the payload.
    fn blob(&self, job: &Job, payload: &[u8]) -> Vec<u8> {
        let mut blob = self.image.clone();
        let mode = mode_word("piece").unwrap_or_else(|| die("no piece mode"));
        patch(&mut blob, mode, 1).unwrap_or_else(|e| die(e));
        let gap = (JOB_ADDRESS - LOAD_ADDRESS) as usize;
        if blob.len() > gap {
            die("the stage image runs into the job address");
        }
        blob.resize(gap, 0);
        blob.extend_from_slice(&job.encode());
        if !payload.is_empty() {
            blob.resize(gap + DATA_OFFSET, 0);
            blob.extend_from_slice(payload);
        }
        blob
    }

    /// One piece: load, start, read the outcome, wait for the ROM. Returns what the console said.
    fn piece(
        &mut self,
        op: Op,
        lba: u64,
        count: u32,
        payload: &[u8],
        expect: Option<&str>,
        extent: Extent,
    ) -> Result<Said, String> {
        let job = Job {
            op,
            lba,
            count,
            data_crc: if payload.is_empty() {
                0
            } else {
                crc32(payload)
            },
            flags: if self.fast { FAST } else { 0 },
            card_blocks: self.blocks,
            cid: self.cid,
            extent: Some(extent),
        };
        Job::parse(&job.encode()).map_err(str::to_string)?;
        let blob = self.blob(&job, payload);
        self.pieces += 1;
        let n = self.pieces;
        self.note(&format!(
            "piece {n}: op {op:?} sectors {lba}..{} in extent {}..{} ({} bytes of payload, sha256 {}), load {} bytes sha256 {} at {LOAD_ADDRESS:#010x}",
            lba + u64::from(count),
            extent.first(),
            extent.end(),
            payload.len(),
            if payload.is_empty() { "-".into() } else { sha(payload) },
            blob.len(),
            sha(&blob),
        ));
        if self.dry {
            return Ok(Said {
                ok: true,
                end: true,
                sha256: expect.map(str::to_string),
                before: Some("dry".into()),
                after: Some("dry".into()),
                ..Said::default()
            });
        }
        let old = devnum(&self.port).ok_or(format!("no 29f1:0230 at {}", self.port))?;
        for console in &self.consoles {
            let _ = console.drain();
        }
        let started = Instant::now();
        load_and_start(&self.port, LOAD_ADDRESS, &blob)?;
        // The longest a piece may take: a floor for the card's start, and a bound per sector at the slow clock.
        let limit = 20.0 + f64::from(count) * if matches!(op, Op::Hash) { 0.03 } else { 0.08 };
        let mut lines: Vec<String> = Vec::new();
        let mut returned_early = None;
        while started.elapsed().as_secs_f64() < limit {
            std::thread::sleep(Duration::from_millis(20));
            for console in &self.consoles {
                for line in console.drain() {
                    let _ = writeln!(self.record, "{:.6} console {}", now(), line.text);
                    lines.push(line.text);
                }
            }
            if lines.iter().any(|l| l.contains("lemnos stage: end")) {
                break;
            }
            // A reset before the end line: the watchdog or a fault took the stage.
            if matches!(devnum(&self.port), Some(n) if n != old) {
                if returned_early.is_none() {
                    returned_early = Some(Instant::now());
                } else if returned_early.is_some_and(|t| t.elapsed().as_secs_f64() > 1.0) {
                    break;
                }
            }
        }
        let said = read_said(&lines);
        let back = wait_back(&self.port, old, 15.0);
        let took = started.elapsed().as_secs_f64();
        if !said.end {
            let last = lines.last().cloned().unwrap_or_default();
            return Err(format!(
                "no end line within {limit:.0} s (last console line: {last:?}); ROM back: {back:?}"
            ));
        }
        if !said.ok {
            return Err(format!(
                "the stage reported: {:?}; ROM back: {back:?}",
                said.trouble
            ));
        }
        back?;
        if let Some(expect) = expect
            && said.sha256.as_deref() != Some(expect)
        {
            return Err(format!(
                "the card's sha256 {:?} is not the expected {expect}",
                said.sha256
            ));
        }
        self.note(&format!(
            "piece {n}: ok in {took:.3} s, {} transfers tried again, card sha256 {}{}",
            said.retries,
            said.sha256.clone().unwrap_or_default(),
            if expect.is_some() { " = expected" } else { "" }
        ));
        Ok(said)
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let command = args.next().unwrap_or_else(|| die("piece, flash or load"));
    let mut opt: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut consoles = Vec::new();
    let (mut fast, mut dry) = (false, false);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--fast" => fast = true,
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
    let need = |name: &str| {
        opt.get(name)
            .cloned()
            .unwrap_or_else(|| die(&format!("--{name} is required")))
    };
    let port = need("port");
    let record = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(need("record"))
        .unwrap_or_else(|e| die(&format!("the record: {e}")));

    if command == "load" {
        // A raw load: a range of a file at an address, started there. The consoles are not opened.
        let mut file = File::open(need("file")).unwrap_or_else(|e| die(&format!("the file: {e}")));
        let (offset, size) = (number(&need("offset")), number(&need("size")) as usize);
        let address = number(&need("address")) as u32;
        let mut bytes = vec![0u8; size];
        file.seek(SeekFrom::Start(offset))
            .and_then(|_| file.read_exact(&mut bytes))
            .unwrap_or_else(|e| die(&format!("reading the file: {e}")));
        let mut run = Run {
            port,
            image: Vec::new(),
            cid: [0; 16],
            blocks: 0,
            fast,
            dry,
            consoles: Vec::new(),
            record,
            pieces: 0,
        };
        run.note(&format!(
            "load: {} offset {offset} size {size} sha256 {} at {address:#010x}, then start",
            need("file"),
            sha(&bytes)
        ));
        if !(0x8030_0000..0x8040_0000).contains(&address) || address as usize + size > 0x8040_0000 {
            die("the load is outside the ROM's range 0x80300000..0x80400000");
        }
        if run.dry {
            run.note("dry: nothing opened");
            return;
        }
        match load_and_start(&run.port, address, &bytes) {
            Ok(()) => run.note("load: sent and started"),
            Err(e) => {
                run.note(&format!("load: STOP: {e}"));
                std::process::exit(1);
            }
        }
        return;
    }

    let image =
        std::fs::read(need("image")).unwrap_or_else(|e| die(&format!("the stage image: {e}")));
    let cid_text = need("cid");
    if cid_text.len() != 32 {
        die("--cid is 32 hex digits");
    }
    let mut cid = [0u8; 16];
    for (i, byte) in cid.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&cid_text[2 * i..2 * i + 2], 16)
            .unwrap_or_else(|_| die("--cid is hex"));
    }
    let blocks = number(&need("blocks"));
    let hold: Vec<u64> = opt
        .get("hold")
        .map(|h| h.split(',').map(number).collect())
        .unwrap_or_default();
    let readers = if dry {
        Vec::new()
    } else {
        consoles
            .iter()
            .map(|c| {
                Console::open_readonly(c).unwrap_or_else(|e| die(&format!("console {c}: {e}")))
            })
            .collect()
    };
    if !dry && readers.is_empty() {
        die("a piece's outcome is read on the console: give --console");
    }
    let mut run = Run {
        port,
        image,
        cid,
        blocks,
        fast,
        dry,
        consoles: readers,
        record,
        pieces: 0,
    };
    run.note(&format!(
        "k230-card {command}: stage image sha256 {} ({} bytes), card cid {cid_text} blocks {blocks}, fast {fast}, dry {dry}",
        sha(&run.image),
        run.image.len()
    ));

    let result: Result<(), String> = match command.as_str() {
        "piece" => {
            let (lba, count) = (number(&need("lba")), number(&need("count")) as u32);
            let op = match need("op").as_str() {
                "hash" => Op::Hash,
                "zero" => Op::Zero,
                "write" => Op::Write,
                "test" => Op::Test,
                _ => die("--op is hash, zero, write or test"),
            };
            if op == Op::Test && count as usize > TEST_SECTORS {
                die("the write test is at most eight sectors");
            }
            let payload = if matches!(op, Op::Write | Op::Test) {
                let mut file =
                    File::open(need("data")).unwrap_or_else(|e| die(&format!("--data: {e}")));
                let offset = opt.get("data-offset").map(|o| number(o)).unwrap_or(0);
                source(&mut file, offset / SECTOR as u64, count, &[])
            } else {
                Vec::new()
            };
            let extent = extent_option(&need("extent"));
            let expect = match (opt.get("expect-sha"), opt.get("expect-source")) {
                (Some(text), _) => Some(text.clone()),
                (None, Some(path)) => {
                    let mut file =
                        File::open(path).unwrap_or_else(|e| die(&format!("--expect-source: {e}")));
                    let mut hasher = Sha256::new();
                    let mut at = lba;
                    while at < lba + u64::from(count) {
                        let n = (lba + u64::from(count) - at).min(2048) as u32;
                        hasher.update(source(&mut file, at, n, &hold));
                        at += u64::from(n);
                    }
                    Some(hex(&hasher.finalize()))
                }
                (None, None) if matches!(op, Op::Write | Op::Test) => Some(sha(&payload)),
                (None, None) if op == Op::Zero => Some(sha(&vec![0u8; count as usize * SECTOR])),
                (None, None) => None,
            };
            if let Some(e) = &expect {
                run.note(&format!(
                    "expected sha256 of the sectors read from the card: {e}"
                ));
            }
            run.piece(op, lba, count, &payload, expect.as_deref(), extent)
                .and_then(|said| {
                    if op == Op::Test {
                        run.note(&format!(
                            "write test: before {:?}, after {:?}",
                            said.before, said.after
                        ));
                        if said.before.is_none() || said.before != said.after {
                            return Err("the range was not restored".into());
                        }
                    }
                    Ok(())
                })
        }
        "flash" => {
            let path = need("source");
            let mut file = File::open(&path).unwrap_or_else(|e| die(&format!("--source: {e}")));
            let total = file.metadata().map(|m| m.len()).unwrap_or(0) / SECTOR as u64;
            // The progress file holds the first sector not yet written and verified: a run that was cut
            // resumes there. Stopping between pieces is always safe: each piece is its own verified load.
            let progress = opt.get("progress").cloned();
            let resumed = progress
                .as_ref()
                .and_then(|p| std::fs::read_to_string(p).ok())
                .map(|t| number(t.trim()));
            let from = resumed.unwrap_or_else(|| opt.get("from").map(|v| number(v)).unwrap_or(0));
            if let Some(at) = resumed {
                run.note(&format!(
                    "flash: resuming at sector {at} from the progress file"
                ));
            }
            let to = opt.get("to").map(|v| number(v)).unwrap_or(total).min(total);
            run.note(&format!(
                "flash: {path} sectors {from}..{to} of {total}, held as zero: {hold:?}"
            ));
            // The run's whole extent, declared in every piece's job: the sectors this run was asked to write.
            let extent = Extent::new(
                u32::try_from(from).unwrap_or_else(|_| die("--from is past 2 TiB")),
                u32::try_from(to.saturating_sub(from))
                    .unwrap_or_else(|_| die("the flash is past 2 TiB")),
            )
            .unwrap_or_else(|e| die(&format!("nothing to flash: {e}")));
            run.note(&format!(
                "flash: declared extent {} sectors from {}",
                extent.count(),
                extent.first()
            ));
            let started = Instant::now();
            let (mut at, mut written, mut zeroed) = (from, 0u64, 0u64);
            let mut outcome = Ok(());
            while at < to {
                let n = (to - at).min(u64::from(MAX_SECTORS)) as u32;
                let data = source(&mut file, at, n, &hold);
                let step = if data.iter().all(|b| *b == 0) {
                    // A run of zero sectors: extended while the source stays zero, written without a payload.
                    let mut run_len = n;
                    while run_len < ZERO_RUN && at + u64::from(run_len) < to {
                        let more =
                            (to - at - u64::from(run_len)).min(u64::from(MAX_SECTORS)) as u32;
                        if source(&mut file, at + u64::from(run_len), more, &hold)
                            .iter()
                            .any(|b| *b != 0)
                        {
                            break;
                        }
                        run_len += more;
                    }
                    let run_len = run_len.min(ZERO_RUN);
                    let expect = sha(&vec![0u8; run_len as usize * SECTOR]);
                    zeroed += u64::from(run_len);
                    run.piece(Op::Zero, at, run_len, &[], Some(&expect), extent)
                        .map(|_| run_len)
                } else {
                    written += u64::from(n);
                    run.piece(Op::Write, at, n, &data, Some(&sha(&data)), extent)
                        .map(|_| n)
                };
                match step {
                    Ok(done) => {
                        at += u64::from(done);
                        if let Some(p) = &progress
                            && let Err(e) = std::fs::write(p, format!("{at}\n"))
                        {
                            outcome = Err(format!("the progress file: {e}"));
                            break;
                        }
                    }
                    Err(e) => {
                        outcome = Err(format!("at sector {at}: {e}"));
                        break;
                    }
                }
            }
            run.note(&format!(
                "flash: reached sector {at} of {from}..{to}: {} bytes written from the source, {} bytes of zeros, {} pieces, {:.1} s",
                written * SECTOR as u64,
                zeroed * SECTOR as u64,
                run.pieces,
                started.elapsed().as_secs_f64()
            ));
            outcome
        }
        _ => die("piece, flash or load"),
    };
    match result {
        Ok(()) => run.note("verdict: PASS"),
        Err(e) => {
            run.note(&format!("STOP: {e}"));
            run.note("verdict: STOP");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_console_lines_of_a_good_piece_and_of_a_failed_one_are_told_apart() {
        let good: Vec<String> = [
            "lemnos stage: piece (writes the card; watchdog armed before this line)",
            "piece sha256 ab",
            "piece ok",
            "lemnos stage: end",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let said = read_said(&good);
        assert!(said.ok && said.end && said.trouble.is_empty());
        assert_eq!(said.sha256.as_deref(), Some("ab"));
        let bad: Vec<String> = [
            "piece FAILED: read back differs at sector 00000005",
            "lemnos stage: end",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let said = read_said(&bad);
        assert!(!said.ok && said.end);
        assert_eq!(said.trouble.len(), 1);
        // A stage the watchdog cut prints neither.
        assert!(!read_said(&["piece at 00004000".to_string()]).end);
    }

    #[test]
    fn held_sectors_read_as_zero_and_the_rest_as_the_source_has_them() {
        let dir = std::env::temp_dir().join(format!("k230-card-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("source.img");
        std::fs::write(&path, vec![0x11u8; 4 * SECTOR]).unwrap();
        let mut file = File::open(&path).unwrap();
        let data = source(&mut file, 1, 3, &[2, 9]);
        assert!(data[..SECTOR].iter().all(|b| *b == 0x11));
        assert!(data[SECTOR..2 * SECTOR].iter().all(|b| *b == 0));
        assert!(data[2 * SECTOR..].iter().all(|b| *b == 0x11));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
