//! The last install attempt: a fact about the device that any session reads in `info`.
//!
//! An install is a job that belongs to its starter's connection, so a session that reconnects
//! after the install ended cannot see how it ended. The driver keeps one fixed-size record of the
//! latest attempt, replaced when the next attempt starts, and `info` writes it (`docs/slots.md`,
//! "The last install attempt"). It is kept in memory for the daemon's life; the one thing a restart
//! can still tell is an install the restart itself cut short (see [`INTERRUPTED`]).

use lemnos_core::CallerId;

use crate::json::quoted;

/// Longest slot id kept, in bytes. A longer one is cut as a reason is.
pub const SLOT_MAX: usize = 64;
/// Longest reason kept, in bytes, the marker included.
pub const REASON_MAX: usize = 192;
/// What a reason cut short ends with.
pub const MARKER: &str = "...";

/// Why an install the service's restart cut short is reported: the board's state says the same.
pub const INTERRUPTED: &str = "an install was interrupted: the service restarted";
/// The reason when the starter cancelled the job.
pub const CANCELLED: &str = "cancelled by the caller that started it";
/// The reason when the starter's connection ended and the host cancelled the job.
pub const GONE: &str = "the caller that started it went away";
/// The reason when the board refuses every bundle because it requires signing and holds no key.
pub const NO_KEYS: &str =
    "this board requires signed bundles and holds no key to check one against";

/// How an attempt stands or ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ended {
    Running,
    Installed,
    /// Turned away by what the bundle or the board says, before the image was trusted: signature,
    /// policy, manifest, order, fit.
    Refused,
    Cancelled,
    /// The service restarted in the middle of it.
    Interrupted,
    /// Went wrong in the work: disk, a hash or size that did not match, a bundle cut short.
    Failed,
}

impl Ended {
    pub fn word(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Installed => "installed",
            Self::Refused => "refused",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
            Self::Failed => "failed",
        }
    }
}

/// Text kept in a fixed buffer: at most `N` bytes, cut on a character boundary with [`MARKER`]
/// when it was longer. Setting it allocates nothing.
#[derive(Clone, Copy)]
pub struct Fixed<const N: usize> {
    buf: [u8; N],
    len: usize,
}

impl<const N: usize> Fixed<N> {
    pub const fn empty() -> Self {
        Self {
            buf: [0; N],
            len: 0,
        }
    }

    pub fn set(&mut self, text: &str) {
        let (keep, marker) = if text.len() <= N {
            (text.len(), false)
        } else {
            let mut at = N - MARKER.len();
            while !text.is_char_boundary(at) {
                at -= 1;
            }
            (at, true)
        };
        self.buf[..keep].copy_from_slice(&text.as_bytes()[..keep]);
        self.len = keep;
        if marker {
            self.buf[keep..keep + MARKER.len()].copy_from_slice(MARKER.as_bytes());
            self.len += MARKER.len();
        }
    }

    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }
}

/// The record. Times are the host's monotonic clock in nanoseconds (`CLOCK_MONOTONIC`, which
/// counts from the boot named by `boot`); never wall time.
#[derive(Clone, Copy)]
pub struct Attempt {
    pub ended: Ended,
    pub slot: Fixed<SLOT_MAX>,
    pub reason: Fixed<REASON_MAX>,
    /// Bytes taken and bytes announced; `None` for an attempt a restart cut short, whose bytes
    /// nobody kept.
    pub received: Option<u64>,
    pub total: Option<u64>,
    pub started_ns: Option<u64>,
    pub ended_ns: Option<u64>,
    /// The caller that started it, to tell a cancel from the host's cancel when its connection
    /// ended. Never shown.
    pub owner: CallerId,
}

impl Attempt {
    /// An attempt that has just started.
    pub fn running(owner: CallerId, total: u64, now_ns: Option<u64>) -> Self {
        Self {
            ended: Ended::Running,
            slot: Fixed::empty(),
            reason: Fixed::empty(),
            received: Some(0),
            total: Some(total),
            started_ns: now_ns,
            ended_ns: None,
            owner,
        }
    }

    /// An install a restart cut short, known only from the board's own state record.
    pub fn restart_cut_short() -> Self {
        let mut reason = Fixed::empty();
        reason.set(INTERRUPTED);
        Self {
            ended: Ended::Interrupted,
            slot: Fixed::empty(),
            reason,
            received: None,
            total: None,
            started_ns: None,
            ended_ns: None,
            owner: CallerId::LOCAL,
        }
    }

    /// Writes the record as the JSON object `info` appends (`last_attempt`), with `boot` the id
    /// of the boot the times count from.
    pub fn json(attempt: Option<&Self>, boot: &str, out: &mut String) {
        let none = Self {
            ended: Ended::Running,
            slot: Fixed::empty(),
            reason: Fixed::empty(),
            received: None,
            total: None,
            started_ns: None,
            ended_ns: None,
            owner: CallerId::LOCAL,
        };
        let a = attempt.unwrap_or(&none);
        let known = attempt.is_some();
        out.push_str("{\"state\":\"");
        out.push_str(if known { a.ended.word() } else { "none" });
        out.push_str("\",\"slot\":");
        quoted(a.slot.as_str(), out);
        out.push_str(",\"reason\":");
        quoted(a.reason.as_str(), out);
        for (key, value) in [("received", a.received), ("total", a.total)] {
            out.push_str(",\"");
            out.push_str(key);
            out.push_str("\":");
            match value {
                Some(n) => out.push_str(&n.to_string()),
                None => out.push_str("null"),
            }
        }
        out.push_str(",\"boot\":");
        let boot = if a.started_ns.is_some() { boot } else { "" };
        quoted(boot, out);
        for (key, value) in [("started_ms", a.started_ns), ("ended_ms", a.ended_ns)] {
            out.push_str(",\"");
            out.push_str(key);
            out.push_str("\":");
            match value {
                Some(ns) => out.push_str(&(ns / 1_000_000).to_string()),
                None => out.push_str("null"),
            }
        }
        out.push_str(",\"text\":");
        quoted(&a.text(known, boot), out);
        out.push('}');
    }

    /// The record as one line, for `lemnos-ctl call slots info` to show and a person to read.
    fn text(&self, known: bool, boot: &str) -> String {
        if !known {
            return "last install: none".into();
        }
        let slot = if self.slot.as_str().is_empty() {
            "-"
        } else {
            self.slot.as_str()
        };
        let mut line = format!("last install: slot {slot} {}", self.ended.word());
        if !self.reason.as_str().is_empty() {
            line.push_str(": ");
            line.push_str(self.reason.as_str());
        }
        match (self.received, self.total) {
            (Some(got), Some(total)) => {
                line.push_str(&format!(" ({got} of {total} bytes"));
                let at = self.ended_ns.or(self.started_ns);
                match at {
                    Some(ns) => {
                        let id: String = boot.chars().take(8).collect();
                        let id = if id.is_empty() { "-" } else { id.as_str() };
                        let ms = ns / 1_000_000;
                        line.push_str(&format!(
                            "; boot {id}, +{}.{} s)",
                            ms / 1000,
                            ms % 1000 / 100
                        ));
                    }
                    None => line.push(')'),
                }
            }
            _ => line.push_str(" (bytes and time unknown: an earlier run of the daemon)"),
        }
        line
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_text_is_cut_on_a_character_boundary_with_the_marker() {
        let mut f = Fixed::<REASON_MAX>::empty();
        f.set("short");
        assert_eq!(f.as_str(), "short");
        // Two-byte characters, so a cut at an odd byte would split one.
        let long = "é".repeat(REASON_MAX);
        f.set(&long);
        assert!(f.as_str().len() <= REASON_MAX);
        assert!(f.as_str().ends_with(MARKER));
        assert!(f.as_str().starts_with('é'));
        let exact = "a".repeat(REASON_MAX);
        f.set(&exact);
        assert_eq!(f.as_str(), exact);
        f.set(&"a".repeat(REASON_MAX + 1));
        assert_eq!(f.as_str().len(), REASON_MAX);
        assert!(f.as_str().ends_with(MARKER));
    }
}
