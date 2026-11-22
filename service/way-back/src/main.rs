//! `lemnos-way-back rpiboot [--device NODE]`: exits 0 when the firmware took RPIBOOT for the next boot,
//! 1 when it did not (the reason on stderr and in the kernel log, at the error level). See the crate
//! documentation.

use std::fs::OpenOptions;
use std::io::Write;
use std::process::ExitCode;

use lemnos_way_back::{Mailbox, parse, run};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let outcome = parse(&args).and_then(|ask| run(&ask, &Mailbox));
    match outcome {
        Ok(line) => {
            let _ = writeln!(std::io::stdout(), "lemnos-way-back: {line}");
            kernel_log(6, &line);
            ExitCode::SUCCESS
        }
        Err(why) => {
            let _ = writeln!(std::io::stderr(), "lemnos-way-back: {why}");
            kernel_log(3, &why);
            ExitCode::FAILURE
        }
    }
}

/// One line to the kernel log at `level` (6 info, 3 error), so it reaches the console and a
/// ramoops record from an initramfs with no other log. Nothing when /dev/kmsg is not there.
fn kernel_log(level: u8, line: &str) {
    if let Ok(mut kmsg) = OpenOptions::new().write(true).open("/dev/kmsg") {
        let _ = kmsg.write_all(format!("<{level}>lemnos-way-back: {line}\n").as_bytes());
    }
}
