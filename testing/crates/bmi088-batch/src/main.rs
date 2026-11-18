//! B8's capture tool: reads the FIFO's batches through the daemon, every 50 ms with a 4 KiB buffer, for
//! a fixed time, and prints the rows the record's checks read.
//!
//! ```text
//! lemnos-bmi088-batch --socket SOCK --device imu --contract accel --seconds 45 --period-ms 50 --out records.tsv
//! lemnos-bmi088-batch --socket SOCK --device imu --contract accel --wait-model 45 --seconds 45 --out records.tsv
//! lemnos-bmi088-batch --socket SOCK --device imu-gyro --contract gyro --accel-device imu --out gyro.tsv --accel-out accel.tsv
//! lemnos-bmi088-batch --replay records.tsv --contract accel --elapsed-s 45 --poll-ms 50
//! lemnos-bmi088-batch --replay gyro.tsv --contract gyro --accel-replay accel.tsv --elapsed-s 45 --poll-ms 50
//! ```
//!
//! `--contract` picks the die: `accel` (the default, `accelerometer/1`) or `gyro` (`gyroscope/1`). It
//! also picks the die's tick drain period, which bounds its tick-time stamps (see `lib.rs`), so a replay
//! names it too. A gyro run also reads the accelerometer (`--accel-device`, default `imu`) on the same
//! poll, which is what check 7 (the two dies' stamps against each other) needs. Each batch is read with
//! `read_batch_stamped`, and its host read span is taken on `CLOCK_MONOTONIC` (the clock the stamps are
//! on), before and after the read.
//!
//! `--wait-model S` waits, before the capture, until the die (both dies in a gyro run) delivers a batch
//! whose newest record is the model's, reading and dropping batches on the capture's own poll. It waits
//! `S` seconds at most. A die that has not reached its model by then is NOT MEASURED (exit 3): the
//! capture is not taken, and nothing is a pass.
//!
//! `--out FILE` writes one row per record: `at_ns`, `mode`, `batch`, `read_start_ns`, `read_end_ns`,
//! `poll`. `--accel-out FILE` writes the accelerometer's records of a gyro run the same way.
//! `--replay FILE` runs the same checks on such a file (or on an older file with fewer columns) with no
//! daemon, and `--accel-replay FILE` adds check 7 from a gyro run's two files. What a file lacks is not
//! run: the replay says `not_evaluated` for it, never a pass.
//!
//! Exit codes: 0 measured, every check passes; 2 usage; 3 NOT MEASURED (the daemon is not reachable, the
//! device is not the die it should be, it refuses batches, or it did not reach its model within
//! `--wait-model`: a device with `fifo = false` answers `NOT_SUPPORTED`, as the real board does without
//! the FIFO); 4 a data fault (a batch that is not whole records, or a record that does not decode, or a
//! read that failed, or a file that is not a capture); 5 measured, and a check FAILED. A failed check is
//! printed as `fail`, never as a pass. A replay that ran every check it could (none failed) but could
//! not run one (a file without read times) exits 3, not 0.

use std::io::Write;
use std::time::{Duration, Instant};

use lemnos_bmi088_batch::{
    ACCEL_DRAIN_PERIODS, BATCH_BYTES, BatchError, Bounds, Capture, GYRO_DRAIN_PERIODS, ReadSpan,
    Report, check_rows, check7_rows, from_tsv, prefixed, read_rows, stamps_tsv, status_row,
};
use lemnos_client::Client;
use lemnos_contract_accelerometer::{Accelerometer, Stamp};
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

impl Die {
    /// The die's bounds at this sample period: its tick drain period is half its FIFO depth.
    fn bounds(self, period_ns: u64) -> Bounds {
        match self {
            Die::Accel => Bounds::new(period_ns, ACCEL_DRAIN_PERIODS),
            Die::Gyro => Bounds::new(period_ns, GYRO_DRAIN_PERIODS),
        }
    }
}

/// One stamped batch read: fills the buffer, returns the bytes written.
type BatchRead = Box<dyn FnMut(&mut [u8]) -> Result<usize, Status>>;

struct Args {
    socket: String,
    device: String,
    accel_device: String,
    die: Die,
    /// The capture's duration. Defaults to 45 s for a live capture; for a replay it is the wall time
    /// when `--elapsed-s` is not given.
    seconds: Option<u64>,
    /// A replay's wall time in seconds (`--elapsed-s`, or `--seconds`), for check (d).
    elapsed_s: Option<f64>,
    /// A replay's poll period in ms (`--poll-ms`), for check (d). A live run uses `--period-ms`.
    poll_ms: Option<u64>,
    period_ms: u64,
    sample_period_ms: u64,
    /// How long to wait for the model before the capture, seconds (`--wait-model`).
    wait_model_s: Option<u64>,
    out: Option<String>,
    accel_out: Option<String>,
    replay: Option<String>,
    accel_replay: Option<String>,
}

fn usage() -> ! {
    err(
        "usage: lemnos-bmi088-batch --socket SOCK [--device imu] [--contract accel|gyro] [--accel-device imu] [--wait-model S] [--seconds 45] [--period-ms 50] [--sample-period-ms 5] [--out FILE] [--accel-out FILE]\n       lemnos-bmi088-batch --replay FILE [--contract accel|gyro] [--accel-replay FILE] [--elapsed-s S] [--poll-ms MS] [--sample-period-ms 5]",
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
        wait_model_s: None,
        out: None,
        accel_out: None,
        replay: None,
        accel_replay: None,
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
            "--wait-model" => args.wait_model_s = Some(value.parse().unwrap_or_else(|_| usage())),
            "--out" => args.out = Some(value),
            "--accel-out" => args.accel_out = Some(value),
            "--replay" => args.replay = Some(value),
            "--accel-replay" => args.accel_replay = Some(value),
            _ => usage(),
        }
    }
    if (args.replay.is_none() && args.socket.is_empty())
        || args.period_ms == 0
        || args.seconds == Some(0)
        || args.sample_period_ms == 0
        || (args.accel_replay.is_some() && (args.replay.is_none() || args.die != Die::Gyro))
        || (args.accel_out.is_some() && args.die != Die::Gyro)
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

fn fault(why: &str) -> ! {
    out(&format!("b8.status\tfault\t\t{why}"));
    std::process::exit(4);
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

/// Reads a capture file, or stops with a fault.
fn read_capture(path: &str) -> lemnos_bmi088_batch::Replay {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| fault(&format!("cannot read {path}: {e}")));
    from_tsv(&text).unwrap_or_else(|e| fault(&format!("{path}: {e}")))
}

/// `--replay FILE`: the same checks on a capture file's stamps, with no daemon. Exits with the outcome.
fn replay_file(path: &str, args: &Args) -> ! {
    let replay = read_capture(path);
    let period_ns = sample_period_ns(args);
    // Check (d) needs both the wall time and the poll period; with either missing it is not evaluated.
    let elapsed = args
        .elapsed_s
        .map(|s| (s * 1e9).round() as u64)
        .or(args.seconds.map(|s| s * 1_000_000_000));
    let timing = match (elapsed, args.poll_ms) {
        (Some(e), Some(p)) => Some((e, p * 1_000_000)),
        _ => None,
    };
    let mut text = format!(
        "b8.records\tmeasured\t{}\trecords in the capture file\n\
         b8.not_in_file\tinfo\t\treads, empty reads, repeats and refused bytes are not in a capture file, so they are not shown\n",
        replay.report.records
    );
    let (rows, mut outcome) = check_rows(&Capture::replayed(
        &replay,
        args.die.bounds(period_ns),
        timing,
    ));
    text.push_str(&rows);
    if let Some(accel_path) = &args.accel_replay {
        let accel = read_capture(accel_path);
        let (rows, accel_outcome) = check_rows(&Capture::replayed(
            &accel,
            Die::Accel.bounds(period_ns),
            timing,
        ));
        text.push_str(&prefixed(&rows, "b8.accel."));
        let (rows, seven) = check7_rows(
            &replay.report,
            &accel.report,
            period_ns,
            replay.has_polls && accel.has_polls,
        );
        text.push_str(&rows);
        outcome = outcome.and(accel_outcome).and(seven);
    }
    text.push_str(&status_row(
        outcome,
        &format!("{} records", replay.report.records),
    ));
    let _ = write!(std::io::stdout(), "{text}");
    std::process::exit(outcome.exit_code());
}

/// Whether the last record a die delivered is the model's.
fn on_model(report: &Report) -> bool {
    report.modes.last() == Some(&Stamp::Model)
}

/// `--wait-model`: reads and drops batches on the capture's poll until every die read delivers the
/// model's records, for `limit` at most. Returns how long it took, or `None` when it was not reached.
fn wait_model(
    reads: &mut [&mut BatchRead],
    buf: &mut [u8],
    period: Duration,
    limit: Duration,
) -> Option<Duration> {
    let started = Instant::now();
    let mut seen: Vec<Report> = reads.iter().map(|_| Report::default()).collect();
    let mut next = started;
    loop {
        for (read, report) in reads.iter_mut().zip(seen.iter_mut()) {
            let span = ReadSpan {
                start_ns: 0,
                end_ns: 0,
            };
            match read(buf) {
                Ok(bytes) => {
                    if report.add_batch(&buf[..bytes], span).is_err() {
                        fault("a batch read while waiting for the model is not whole records");
                    }
                }
                Err(status) if status == Status::NOT_SUPPORTED => not_measured(
                    "the device refuses batches (NOT_SUPPORTED): fifo = false, or no FIFO on this chip",
                ),
                Err(status) => fault(&format!(
                    "read_batch failed while waiting for the model: {status:?}"
                )),
            }
        }
        if seen.iter().all(on_model) {
            return Some(started.elapsed());
        }
        if started.elapsed() >= limit {
            return None;
        }
        next += period;
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        }
    }
}

/// Writes a capture file, or stops with a fault.
fn write_capture(path: &str, report: &Report) {
    let mut file =
        std::fs::File::create(path).unwrap_or_else(|e| fault(&format!("cannot write {path}: {e}")));
    let _ = write!(file, "{}", stamps_tsv(report));
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
    let mut buf = vec![0u8; BATCH_BYTES];
    let mut waited = None;
    if let Some(limit) = args.wait_model_s {
        let mut reads: Vec<&mut BatchRead> = vec![&mut read];
        if let Some(accel) = accel_read.as_mut() {
            reads.push(accel);
        }
        match wait_model(&mut reads, &mut buf, period, Duration::from_secs(limit)) {
            Some(took) => waited = Some(took),
            None => {
                out(&format!(
                    "b8.wait_model\tnot_reached\t{limit}\tseconds waited: a die still stamps from tick time, so the capture after convergence was not taken"
                ));
                not_measured(
                    "the model was not reached within --wait-model: not measured, and not a pass",
                );
            }
        }
    }

    let deadline = Instant::now() + Duration::from_secs(seconds);
    let started = Instant::now();
    let mut report = Report::default();
    let mut accel_report = Report::default();
    let mut fault_seen: Option<String> = None;
    let mut next = started;
    while Instant::now() < deadline {
        let start_ns = host_ns();
        let result = read(&mut buf);
        let end_ns = host_ns();
        match result {
            Ok(bytes) => record(
                &mut report,
                &mut fault_seen,
                &buf[..bytes],
                ReadSpan { start_ns, end_ns },
            ),
            Err(status) if status == Status::NOT_SUPPORTED && report.reads == 0 => {
                not_measured(
                    "the device refuses batches (NOT_SUPPORTED): fifo = false, or no FIFO on this chip",
                );
            }
            Err(status) => {
                fault_seen.get_or_insert(format!("read_batch failed: {status:?}"));
                // The poll is counted, so the two dies' polls stay in step.
                report.reads += 1;
            }
        }
        if let Some(accel) = accel_read.as_mut() {
            let start_ns = host_ns();
            let result = accel(&mut buf);
            let end_ns = host_ns();
            match result {
                Ok(bytes) => record(
                    &mut accel_report,
                    &mut fault_seen,
                    &buf[..bytes],
                    ReadSpan { start_ns, end_ns },
                ),
                Err(status) => {
                    fault_seen
                        .get_or_insert(format!("accelerometer read_batch failed: {status:?}"));
                    accel_report.reads += 1;
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
    let mut text = String::new();
    if let Some(took) = waited {
        text.push_str(&format!(
            "b8.wait_model\tmeasured\t{:.3}\tseconds waited before the capture for the model's records\n",
            took.as_secs_f64()
        ));
    }
    text.push_str(&read_rows(&report));
    let (rows, mut outcome) = check_rows(&Capture::live(
        &report,
        args.die.bounds(period_ns),
        elapsed_ns,
        poll_ns,
    ));
    text.push_str(&rows);
    if args.die == Die::Gyro {
        // The accelerometer's records from the same polls, by the accelerometer's bounds, and check 7.
        let (rows, accel_outcome) = check_rows(&Capture::live(
            &accel_report,
            Die::Accel.bounds(period_ns),
            elapsed_ns,
            poll_ns,
        ));
        text.push_str(&prefixed(&read_rows(&accel_report), "b8.accel."));
        text.push_str(&prefixed(&rows, "b8.accel."));
        let (rows, seven) = check7_rows(&report, &accel_report, period_ns, true);
        text.push_str(&rows);
        outcome = outcome.and(accel_outcome).and(seven);
    }
    let _ = write!(std::io::stdout(), "{text}");
    if let Some(path) = &args.out {
        write_capture(path, &report);
    }
    if let Some(path) = &args.accel_out {
        write_capture(path, &accel_report);
    }
    if let Some(why) = fault_seen {
        fault(&why);
    }
    let _ = write!(
        std::io::stdout(),
        "{}",
        status_row(
            outcome,
            &format!(
                "{} records in {:.2} s",
                report.records,
                elapsed.as_secs_f64()
            )
        )
    );
    std::process::exit(outcome.exit_code());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args() -> Args {
        Args {
            socket: String::new(),
            device: String::new(),
            accel_device: String::new(),
            die: Die::Accel,
            seconds: Some(1),
            elapsed_s: None,
            poll_ms: None,
            period_ms: 50,
            sample_period_ms: 5,
            wait_model_s: None,
            out: None,
            accel_out: None,
            replay: None,
            accel_replay: None,
        }
    }

    /// testing/crates/bmi088-batch/src/main.rs::tests::gaps_are_measured_at_the_sample_period_not_the_poll_period
    /// Fails on the code before this fix: the rows were given the poll period (50 ms), so a 9 ms gap
    /// between two samples was not a miss.
    #[test]
    fn gaps_are_measured_at_the_sample_period_not_the_poll_period() {
        let args = args();
        assert_eq!(sample_period_ns(&args), 5_000_000);
        assert_ne!(sample_period_ns(&args), 50_000_000);
    }

    /// The tick-time bound is each die's own drain period: half its FIFO depth and one sample period,
    /// 370 ms for the accelerometer and 255 ms for the gyroscope at 200 Hz.
    #[test]
    fn each_die_has_its_own_tick_drain_period() {
        assert_eq!(Die::Accel.bounds(5_000_000).drain_ns, 370_000_000);
        assert_eq!(Die::Gyro.bounds(5_000_000).drain_ns, 255_000_000);
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
