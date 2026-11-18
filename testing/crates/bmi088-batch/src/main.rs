//! B8's capture tool: reads the FIFO's batches through the daemon, every 50 ms with a 4 KiB buffer, for
//! a fixed time, and prints the rows the record's checks read.
//!
//! ```text
//! lemnos-bmi088-batch --socket SOCK --device imu --contract accel --seconds 45 --period-ms 50 --out records.tsv
//! lemnos-bmi088-batch --socket SOCK --device imu-gyro --contract gyro --accel-device imu --seconds 45
//! lemnos-bmi088-batch --replay records.tsv --seconds 45
//! ```
//!
//! `--contract` picks the die: `accel` (the default, `accelerometer/1`) or `gyro` (`gyroscope/1`). A gyro
//! run also reads the accelerometer (`--accel-device`, default `imu`) on the same poll, which is what
//! check 7 (the offset between the two dies' stamps) needs. Each batch is read with `read_batch_stamped`,
//! and its host read span is taken on `CLOCK_MONOTONIC` (the clock the stamps are on), before and after
//! the read.
//!
//! `--out FILE` writes one row per record: `at_ns`, `mode`, `batch`, `read_start_ns`, `read_end_ns`.
//! `--replay FILE` runs the same checks on such a file (or on an older `at_ns`-only file) with no daemon.
//! What the file lacks is not run: check (a) and check (b) need the read times, and the replay says
//! `not_evaluated` for them, never a pass.
//!
//! Exit codes: 0 measured, every check passes; 2 usage; 3 NOT MEASURED (the daemon is not reachable, the
//! device is not the die it should be, or it refuses batches: a device with `fifo = false` answers
//! `NOT_SUPPORTED`, as the real board does without the FIFO); 4 a data fault (a batch that is not whole
//! records, or a record that does not decode, or a read that failed, or a file that is not a capture); 5
//! measured, and a check FAILED. A failed check is printed as `fail`, never as a pass. A replay that ran
//! every check it could (none failed) but could not run one (a file without read times) exits 3, not 0.

use std::io::Write;
use std::time::{Duration, Instant};

use lemnos_bmi088_batch::{
    BatchError, ReadSpan, ReplayOutcome, Report, all_pass, check7_rows, from_tsv, replay_rows,
    rows, stamps_tsv,
};
use lemnos_client::Client;
use lemnos_contract_accelerometer::Accelerometer;
use lemnos_contract_gyroscope::Gyroscope;
use lemnos_core::Status;

/// Writes one line to stdout (the rows) or stderr (the usage and the refusals).
fn out(text: &str) {
    let _ = writeln!(std::io::stdout(), "{text}");
}
fn err(text: &str) {
    let _ = writeln!(std::io::stderr(), "{text}");
}

/// Which die to read: the accelerometer's `read_batch_stamped`, or the gyroscope's.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Die {
    Accel,
    Gyro,
}

/// One stamped batch read: fills the buffer, returns the bytes written.
type BatchRead = Box<dyn FnMut(&mut [u8]) -> Result<usize, Status>>;

struct Args {
    socket: String,
    device: String,
    accel_device: String,
    die: Die,
    /// The capture's duration. Defaults to 45 s for a live capture; for a replay it is only shown, and only
    /// when given.
    seconds: Option<u64>,
    /// A replay's wall time in seconds (`--elapsed-s`, or `--seconds`), for check (d).
    elapsed_s: Option<f64>,
    /// A replay's poll period in ms (`--poll-ms`), for check (d). A live run uses `--period-ms`.
    poll_ms: Option<u64>,
    period_ms: u64,
    sample_period_ms: u64,
    out: Option<String>,
    replay: Option<String>,
}

fn usage() -> ! {
    err(
        "usage: lemnos-bmi088-batch --socket SOCK [--device imu] [--contract accel|gyro] [--accel-device imu] [--seconds 45] [--period-ms 50] [--sample-period-ms 5] [--out FILE]\n       lemnos-bmi088-batch --replay FILE [--elapsed-s S] [--poll-ms MS] [--sample-period-ms 5]",
    );
    std::process::exit(2);
}

fn parse() -> Args {
    let mut args = Args {
        socket: String::new(),
        device: "imu".into(),
        accel_device: "imu".into(),
        die: Die::Accel,
        seconds: None,
        elapsed_s: None,
        poll_ms: None,
        period_ms: 50,
        sample_period_ms: 5,
        out: None,
        replay: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let value = it.next().unwrap_or_else(|| usage());
        match flag.as_str() {
            "--socket" => args.socket = value,
            "--device" => args.device = value,
            "--accel-device" => args.accel_device = value,
            "--contract" => {
                args.die = match value.as_str() {
                    "accel" => Die::Accel,
                    "gyro" => Die::Gyro,
                    _ => usage(),
                }
            }
            "--seconds" => args.seconds = Some(value.parse().unwrap_or_else(|_| usage())),
            "--elapsed-s" => {
                let v: f64 = value.parse().unwrap_or_else(|_| usage());
                if !(v.is_finite() && v > 0.0) {
                    usage();
                }
                args.elapsed_s = Some(v)
            }
            "--poll-ms" => args.poll_ms = Some(value.parse().unwrap_or_else(|_| usage())),
            "--period-ms" => args.period_ms = value.parse().unwrap_or_else(|_| usage()),
            "--sample-period-ms" => {
                args.sample_period_ms = value.parse().unwrap_or_else(|_| usage())
            }
            "--out" => args.out = Some(value),
            "--replay" => args.replay = Some(value),
            _ => usage(),
        }
    }
    if (args.replay.is_none() && args.socket.is_empty())
        || args.period_ms == 0
        || args.seconds == Some(0)
        || args.sample_period_ms == 0
    {
        usage();
    }
    args
}

/// The chip's sample period in nanoseconds: what a gap is measured against (`--sample-period-ms`,
/// 5 ms at 200 Hz). Not the poll period (`--period-ms`), which is how often the tool reads.
fn sample_period_ns(args: &Args) -> u64 {
    args.sample_period_ms * 1_000_000
}

/// The host's `CLOCK_MONOTONIC` now, in nanoseconds: the clock the daemon stamps on.
fn host_ns() -> u64 {
    let t = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    t.tv_sec as u64 * 1_000_000_000 + t.tv_nsec as u64
}

fn not_measured(why: &str) -> ! {
    out(&format!("b8.status\tnot_measured\t\t{why}"));
    std::process::exit(3);
}

/// Opens the die's stamped batch reader on `device`, or stops with NOT MEASURED.
fn open(client: &Client, die: Die, device: &str) -> BatchRead {
    match die {
        Die::Accel => {
            let mut accel = client
                .open::<dyn Accelerometer>(device)
                .unwrap_or_else(|e| {
                    not_measured(&format!(
                        "{device:?} is not an accelerometer the daemon serves: {e}"
                    ))
                });
            Box::new(move |buf| accel.read_batch_stamped(buf))
        }
        Die::Gyro => {
            let mut gyro = client.open::<dyn Gyroscope>(device).unwrap_or_else(|e| {
                not_measured(&format!(
                    "{device:?} is not a gyroscope the daemon serves: {e}"
                ))
            });
            Box::new(move |buf| gyro.read_batch_stamped(buf))
        }
    }
}

/// Records one read's batch into `report`, or names the fault in `fault` (the first one kept).
fn record(report: &mut Report, fault: &mut Option<String>, buf: &[u8], span: ReadSpan) {
    if let Err(e) = report.add_batch(buf, span) {
        fault.get_or_insert(match e {
            BatchError::Partial { bytes } => {
                format!("a batch of {bytes} bytes is not whole records")
            }
            BatchError::Undecodable { index } => {
                format!("record {index} of a batch did not decode")
            }
        });
    }
}

/// `--replay FILE`: the same checks on a capture file's stamps, with no daemon. Exits with the outcome.
fn replay_file(path: &str, args: &Args) -> ! {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| {
        out(&format!("b8.status\tfault\t\tcannot read {path}: {e}"));
        std::process::exit(4);
    });
    let replay = from_tsv(&text).unwrap_or_else(|e| {
        out(&format!("b8.status\tfault\t\t{path}: {e}"));
        std::process::exit(4);
    });
    // Check (d) needs both the wall time and the poll period; with either missing it is not evaluated.
    let elapsed = args
        .elapsed_s
        .map(|s| (s * 1e9).round() as u64)
        .or(args.seconds.map(|s| s * 1_000_000_000));
    let timing = match (elapsed, args.poll_ms) {
        (Some(e), Some(p)) => Some((e, p * 1_000_000)),
        _ => None,
    };
    let (text, outcome) = replay_rows(&replay, sample_period_ns(args), timing);
    let _ = write!(std::io::stdout(), "{text}");
    std::process::exit(match outcome {
        ReplayOutcome::Pass => 0,
        ReplayOutcome::Incomplete => 3,
        ReplayOutcome::Fail => 5,
    });
}

fn main() {
    let args = parse();
    if let Some(path) = &args.replay {
        replay_file(path, &args);
    }
    let seconds = args.seconds.unwrap_or(45);
    let client = Client::connect(&args.socket, "b8-batch").unwrap_or_else(|e| {
        not_measured(&format!(
            "the daemon at {} is not reachable: {e}",
            args.socket
        ))
    });
    let mut read = open(&client, args.die, &args.device);
    // A gyro run also reads the accelerometer, for check 7.
    let mut accel_read =
        (args.die == Die::Gyro).then(|| open(&client, Die::Accel, &args.accel_device));

    let period = Duration::from_millis(args.period_ms);
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let started = Instant::now();
    let mut report = Report::default();
    let mut accel_report = Report::default();
    let mut buf = vec![0u8; 4096];
    let mut fault: Option<String> = None;
    let mut next = started;
    while Instant::now() < deadline {
        let start_ns = host_ns();
        let result = read(&mut buf);
        let end_ns = host_ns();
        match result {
            Ok(bytes) => record(
                &mut report,
                &mut fault,
                &buf[..bytes],
                ReadSpan { start_ns, end_ns },
            ),
            Err(status) if status == Status::NOT_SUPPORTED && report.reads == 0 => {
                not_measured(
                    "the device refuses batches (NOT_SUPPORTED): fifo = false, or no FIFO on this chip",
                );
            }
            Err(status) => {
                fault.get_or_insert(format!("read_batch failed: {status:?}"));
            }
        }
        if let Some(accel) = accel_read.as_mut() {
            let start_ns = host_ns();
            let result = accel(&mut buf);
            let end_ns = host_ns();
            match result {
                Ok(bytes) => record(
                    &mut accel_report,
                    &mut fault,
                    &buf[..bytes],
                    ReadSpan { start_ns, end_ns },
                ),
                Err(status) => {
                    fault.get_or_insert(format!("accelerometer read_batch failed: {status:?}"));
                }
            }
        }
        next += period;
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        }
    }
    let elapsed = started.elapsed();
    let period_ns = sample_period_ns(&args);
    let poll_ns = args.period_ms * 1_000_000;
    let elapsed_ns = elapsed.as_nanos() as u64;
    let mut text = rows(&report, period_ns, poll_ns, elapsed_ns);
    let mut pass = all_pass(&report, period_ns, elapsed_ns, poll_ns);
    if args.die == Die::Gyro {
        text.push_str(&check7_rows(&report, &accel_report));
        let accel_pass = all_pass(&accel_report, period_ns, elapsed_ns, poll_ns);
        text.push_str(&format!(
            "b8.accel.verdict\t{}\t\tchecks (a), (b) and (c) on the accelerometer's records in this run\n",
            if accel_pass { "pass" } else { "fail" }
        ));
        pass &= accel_pass;
    }
    let _ = write!(std::io::stdout(), "{text}");
    if let Some(path) = &args.out {
        let mut file = std::fs::File::create(path).unwrap_or_else(|e| {
            err(&format!("cannot write {path}: {e}"));
            std::process::exit(4);
        });
        let _ = write!(file, "{}", stamps_tsv(&report));
    }
    if let Some(why) = fault {
        out(&format!("b8.status\tfault\t\t{why}"));
        std::process::exit(4);
    }
    if !pass {
        out(&format!(
            "b8.status\tchecks_failed\t{} records in {:.2} s\tone or more checks failed: see the verdict rows",
            report.records,
            elapsed.as_secs_f64()
        ));
        std::process::exit(5);
    }
    out(&format!(
        "b8.status\tmeasured\t{} records in {:.2} s\tall checks pass",
        report.records,
        elapsed.as_secs_f64()
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// testing/crates/bmi088-batch/src/main.rs::tests::gaps_are_measured_at_the_sample_period_not_the_poll_period
    /// Fails on the code before this fix: the rows were given the poll period (50 ms), so a 9 ms gap
    /// between two samples was not a miss.
    #[test]
    fn gaps_are_measured_at_the_sample_period_not_the_poll_period() {
        let args = Args {
            socket: String::new(),
            device: String::new(),
            accel_device: String::new(),
            die: Die::Accel,
            seconds: Some(1),
            elapsed_s: None,
            poll_ms: None,
            period_ms: 50,
            sample_period_ms: 5,
            out: None,
            replay: None,
        };
        assert_eq!(sample_period_ns(&args), 5_000_000);
        assert_ne!(sample_period_ns(&args), 50_000_000);
    }

    /// The tool decodes both dies' stamped batches with one decoder (`lib.rs`), so the two contracts'
    /// stamped records must be the same bytes: the same length, and the same stamp and value layout.
    #[test]
    fn both_dies_stamped_records_are_the_same_length() {
        assert_eq!(
            lemnos_contract_accelerometer::STAMPED_RECORD_LEN,
            lemnos_contract_gyroscope::STAMPED_RECORD_LEN
        );
        assert_eq!(
            lemnos_contract_accelerometer::RECORD_LEN,
            lemnos_contract_gyroscope::RECORD_LEN
        );
    }

    /// The host clock is `CLOCK_MONOTONIC`: it never goes back between two reads.
    #[test]
    fn the_host_clock_never_goes_back() {
        let a = host_ns();
        let b = host_ns();
        assert!(b >= a && a > 0);
    }
}
