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
        let (tx, rx) = channel();
        thread::spawn(move || {
            for line in BufReader::new(file).lines() {
                let Ok(text) = line else { break };
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

    #[test]
    fn a_missing_console_is_an_error_to_report_not_a_reason_to_wait() {
        assert!(Console::open_readonly("/nonexistent/lemnos-console").is_err());
    }
}
