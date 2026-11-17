//! B8's capture tool: reads the accelerometer's FIFO batches through the daemon, every 50 ms with a
//! 4 KiB buffer, for a fixed time, and prints the rows the record's checks read.
//!
//! ```text
//! lemnos-bmi088-batch --socket SOCK --device imu --seconds 45 --period-ms 50 --out records.tsv
//! ```
//!
//! Exit codes: 0 measured; 2 usage; 3 NOT MEASURED (the daemon is not reachable, the device is not an
//! accelerometer, or it refuses batches: a device with `fifo = false` answers `NOT_SUPPORTED`, as the
//! real board does without the FIFO); 4 a data fault (a batch that is not whole records, or a record
//! that does not decode). Nothing is made up: a refused or failed read is counted, and a run with a
//! fault exits 4 rather than printing a pass.

use std::io::Write;

/// Writes one line to stdout (the rows) or stderr (the usage and the refusals).
fn out(text: &str) {
    let _ = writeln!(std::io::stdout(), "{text}");
}
fn err(text: &str) {
    let _ = writeln!(std::io::stderr(), "{text}");
}
use std::time::{Duration, Instant};

use lemnos_bmi088_batch::{BatchError, Report, rows};
use lemnos_client::Client;
use lemnos_contract_accelerometer::Accelerometer;
use lemnos_core::Status;

struct Args {
    socket: String,
    device: String,
    seconds: u64,
    period_ms: u64,
    sample_period_ms: u64,
    out: Option<String>,
}

fn usage() -> ! {
    err(
        "usage: lemnos-bmi088-batch --socket SOCK [--device imu] [--seconds 45] [--period-ms 50] [--out FILE]",
    );
    std::process::exit(2);
}

fn parse() -> Args {
    let mut args = Args {
        socket: String::new(),
        device: "imu".into(),
        seconds: 45,
        period_ms: 50,
        sample_period_ms: 5,
        out: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let value = it.next().unwrap_or_else(|| usage());
        match flag.as_str() {
            "--socket" => args.socket = value,
            "--device" => args.device = value,
            "--seconds" => args.seconds = value.parse().unwrap_or_else(|_| usage()),
            "--period-ms" => args.period_ms = value.parse().unwrap_or_else(|_| usage()),
            "--sample-period-ms" => {
                args.sample_period_ms = value.parse().unwrap_or_else(|_| usage())
            }
            "--out" => args.out = Some(value),
            _ => usage(),
        }
    }
    if args.socket.is_empty()
        || args.period_ms == 0
        || args.seconds == 0
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

fn not_measured(why: &str) -> ! {
    out(&format!("b8.status\tnot_measured\t\t{why}"));
    std::process::exit(3);
}

fn main() {
    let args = parse();
    let client = Client::connect(&args.socket, "b8-batch").unwrap_or_else(|e| {
        not_measured(&format!(
            "the daemon at {} is not reachable: {e}",
            args.socket
        ))
    });
    let mut accel = client
        .open::<dyn Accelerometer>(&args.device)
        .unwrap_or_else(|e| {
            not_measured(&format!(
                "{:?} is not an accelerometer the daemon serves: {e}",
                args.device
            ))
        });

    let period = Duration::from_millis(args.period_ms);
    let deadline = Instant::now() + Duration::from_secs(args.seconds);
    let started = Instant::now();
    let mut report = Report::default();
    let mut buf = vec![0u8; 4096];
    let mut fault: Option<String> = None;
    let mut next = started;
    while Instant::now() < deadline {
        match accel.read_batch(&mut buf) {
            Ok(bytes) => {
                if let Err(e) = report.add_batch(&buf[..bytes]) {
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
            Err(status) if status == Status::NOT_SUPPORTED && report.reads == 0 => {
                not_measured(
                    "the device refuses batches (NOT_SUPPORTED): fifo = false, or no FIFO on this chip",
                );
            }
            Err(status) => {
                fault.get_or_insert(format!("read_batch failed: {status:?}"));
            }
        }
        next += period;
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        }
    }
    let elapsed = started.elapsed();
    let text = rows(&report, sample_period_ns(&args), elapsed.as_nanos() as u64);
    let _ = write!(std::io::stdout(), "{text}");
    if let Some(path) = &args.out {
        let mut file = std::fs::File::create(path).unwrap_or_else(|e| {
            err(&format!("cannot write {path}: {e}"));
            std::process::exit(4);
        });
        let _ = writeln!(file, "at_ns");
        for stamp in &report.stamps {
            let _ = writeln!(file, "{stamp}");
        }
    }
    if let Some(why) = fault {
        out(&format!("b8.status\tfault\t\t{why}"));
        std::process::exit(4);
    }
    out(&format!(
        "b8.status	measured	{} records in {:.2} s	",
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
            seconds: 1,
            period_ms: 50,
            sample_period_ms: 5,
            out: None,
        };
        assert_eq!(sample_period_ns(&args), 5_000_000);
        assert_ne!(sample_period_ns(&args), 50_000_000);
    }
}
