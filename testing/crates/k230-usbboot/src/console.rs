//! The bench console as a second witness. The board's serial console (a CH342 dual serial on the bench:
//! `/dev/serial/by-id/...`) is opened READ-ONLY: this module never writes to it. Each line is timestamped
//! on receipt, in seconds from the run's start request, so a run's record carries both the USB
//! re-enumeration and the console's reset banner when the console prints one.
//!
//! The banner text is not known from any source (UNVERIFIED): the caller passes the text to look for.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::sync::mpsc::{Receiver, channel};
use std::thread;
use std::time::Instant;

/// A console line and the instant it was received.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub at: Instant,
    pub text: String,
}

/// A read-only console reader: a thread reads lines and sends them over a channel.
pub struct Console {
    rx: Receiver<Line>,
}

impl Console {
    /// Opens `path` read-only and starts reading. Nothing is written to it.
    pub fn open_readonly(path: &str) -> std::io::Result<Self> {
        let file = File::open(path)?;
        set_line(&file)?;
        let (tx, rx) = channel();
        thread::spawn(move || {
            for line in BufReader::new(file).lines() {
                let Ok(text) = line else { break };
                // Raw mode leaves the carriage return on the line.
                let text = text.trim_end_matches('\r').to_string();
                let at = Instant::now();
                if tx.send(Line { at, text }).is_err() {
                    break;
                }
            }
        });
        Ok(Self { rx })
    }

    /// The lines received so far.
    pub fn drain(&self) -> Vec<Line> {
        self.rx.try_iter().collect()
    }
}

/// The console's line speed: the stage's UART and the ROM's messages are at 115200.
pub const SPEED: u32 = 115_200;

/// Sets a tty to 115200, raw, with echo off. A file that is not a tty (a test's plain file) is left alone.
///
/// Bench, 2026-11-18: the reader used to rely on an earlier capture having set the speed. After the adapter
/// re-enumerated, the tty was back at 9600 with echo on, and a whole run's console was lost. Echo matters as
/// well: with it on, the kernel sends every received byte back to the board, which is a write to the console.
fn set_line(file: &File) -> std::io::Result<()> {
    use rustix::termios::{OptionalActions, isatty, tcgetattr, tcsetattr};
    if !isatty(file) {
        return Ok(());
    }
    let mut line = tcgetattr(file)?;
    line.make_raw();
    line.set_speed(SPEED)?;
    tcsetattr(file, OptionalActions::Now, &line)?;
    Ok(())
}

/// The receipt time, in seconds from `start`, of the first line containing `needle`.
pub fn banner_at(lines: &[Line], start: Instant, needle: &str) -> Option<f64> {
    lines
        .iter()
        .find(|l| l.text.contains(needle))
        .map(|l| l.at.saturating_duration_since(start).as_secs_f64())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn the_banner_is_timed_from_the_start_and_only_a_matching_line_counts() {
        let start = Instant::now();
        let lines = vec![
            Line {
                at: start + Duration::from_millis(100),
                text: "boot".into(),
            },
            Line {
                at: start + Duration::from_millis(2500),
                text: "RESET banner".into(),
            },
        ];
        let at = banner_at(&lines, start, "RESET").unwrap();
        assert!((at - 2.5).abs() < 0.01, "{at}");
        assert_eq!(banner_at(&lines, start, "absent"), None);
    }

    /// The stand-in for the adapter after it re-enumerates: a pty whose line is at 9600, cooked, with echo on.
    /// The R3 run of 2026-11-18 lost its console to exactly this state.
    #[test]
    fn a_console_left_at_another_speed_with_echo_on_is_set_to_115200_raw_before_reading() {
        use rustix::fs::{Mode, OFlags};
        use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
        use rustix::termios::{LocalModes, OptionalActions, tcgetattr, tcsetattr};
        use std::io::Write;

        let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).unwrap();
        grantpt(&master).unwrap();
        unlockpt(&master).unwrap();
        let name = ptsname(&master, Vec::new()).unwrap();
        let path = name.to_str().unwrap().to_string();
        // Held open so the settings outlive each open, as a real tty's do while the adapter stays.
        let slave =
            rustix::fs::open(path.as_str(), OFlags::RDWR | OFlags::NOCTTY, Mode::empty()).unwrap();
        let mut line = tcgetattr(&slave).unwrap();
        line.set_speed(9600).unwrap();
        line.local_modes |= LocalModes::ECHO | LocalModes::ICANON;
        tcsetattr(&slave, OptionalActions::Now, &line).unwrap();

        let console = Console::open_readonly(&path).unwrap();
        let after = tcgetattr(&slave).unwrap();
        assert_eq!(after.output_speed(), SPEED);
        assert_eq!(after.input_speed(), SPEED);
        assert!(
            !after.local_modes.contains(LocalModes::ECHO),
            "echo on: every byte the board prints would be written back to it"
        );
        assert!(!after.local_modes.contains(LocalModes::ICANON));

        // A line the board prints arrives whole, without its carriage return, and nothing comes back.
        let mut board = File::from(master);
        board.write_all(b"lemnos stage: end\r\n").unwrap();
        let start = Instant::now();
        let mut lines = Vec::new();
        while lines.is_empty() && start.elapsed() < Duration::from_secs(5) {
            lines = console.drain();
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert_eq!(lines[0].text, "lemnos stage: end");
        rustix::fs::fcntl_setfl(&board, OFlags::NONBLOCK).unwrap();
        let mut back = [0u8; 16];
        let echoed = std::io::Read::read(&mut board, &mut back);
        assert!(
            matches!(&echoed, Err(e) if e.kind() == std::io::ErrorKind::WouldBlock),
            "the console echoed {echoed:?}"
        );
    }

    #[test]
    fn a_missing_console_is_an_error_to_report_not_a_reason_to_wait() {
        assert!(Console::open_readonly("/nonexistent/lemnos-console").is_err());
    }
}
