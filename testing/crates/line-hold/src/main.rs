//! `lemnos-line-hold CHIP OFFSET`: a second holder of one GPIO line, for a board check.
//!
//! It claims line `OFFSET` on the chip node `CHIP` (`/dev/gpiochipN`) the way the Linux backend
//! does, with the line's direction and level left as they are, writes `held` on its output and
//! then sleeps. The line is released when the process ends (a signal is enough: the kernel closes
//! the request). It writes nothing to the line and reads nothing from it.
//!
//! Exit status: it does not exit while it holds the line. 16 when the kernel says the line is
//! held by someone else (`EBUSY`), 1 on any other failure to claim it, 2 on a usage error.

use std::io::Write;
use std::process::ExitCode;

use lemnos_linux::GpioLine;

/// `EBUSY`: the line is already requested.
const BUSY: i32 = 16;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let (Some(chip), Some(offset), None) = (args.next(), args.next(), args.next()) else {
        let _ = writeln!(std::io::stderr(), "usage: lemnos-line-hold CHIP OFFSET");
        return ExitCode::from(2);
    };
    let Ok(offset) = offset.parse::<u32>() else {
        let _ = writeln!(
            std::io::stderr(),
            "lemnos-line-hold: {offset:?}: a line number"
        );
        return ExitCode::from(2);
    };
    let line = match GpioLine::open(&chip, offset) {
        Ok(line) => line,
        Err(error) => {
            let _ = writeln!(
                std::io::stderr(),
                "lemnos-line-hold: {chip} line {offset}: {error}"
            );
            return if error.raw_os_error() == Some(BUSY) {
                ExitCode::from(16)
            } else {
                ExitCode::FAILURE
            };
        }
    };
    let mut out = std::io::stdout();
    let _ = writeln!(out, "held {chip} line {offset}");
    let _ = out.flush();
    // Held until the process is ended: parking can wake for no reason, so it is asked again.
    loop {
        std::thread::park();
        let _ = &line;
    }
}
