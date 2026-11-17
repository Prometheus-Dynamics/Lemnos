//! The Orion precheck: the board's `system.security-precondition/1` stakeholder for the security
//! switch, answered by Orion (`docs/slots.md`, "The Orion precheck").
//!
//! The driver asks Orion's `security_precheck` for a change to on or off, and answers in the words of
//! the contract: one `missing:` or `busy:` line for each thing against the change, and nothing when
//! nothing is.
//!
//! Orion's session is not `Send`, so it lives on a worker thread of its own, which the driver talks to
//! over channels. The worker connects to the comm core the board names (`address`, as `ORION_ADDRESS`
//! names it; unset, the board's own socket), and never gives up:
//!
//! - Connecting is retried on a backoff (1 s, doubling, capped at 8 s). While it is not connected the
//!   device's health is `Degraded`, and a precheck says it did not answer.
//! - Each call to Orion has a deadline of [`CALL`] (its reply timeout), and the driver waits for the
//!   reply for [`WAIT`], under the host's 2 s call deadline. A call that times out, is refused, or loses
//!   its session says `missing: orion: it did not answer (<why>)`. A lost session is dropped, and the
//!   worker reconnects at once. A call is never retried.
//! - A node below the precheck's interface level says which level it needs and which it speaks.
//!
//! The device has no boot-side mark (`apply` does nothing): Orion keeps no mark for the switch.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use lemnos_contract_security_precondition::{SecurityPrecondition, TEXT_MAX};
use lemnos_core::{Config, ConfigError, Health, Key, Kind, Lifecycle, Result, Status, driver};
use orion::node::SecurityTo;
use orion::{Address, ClientError, Error, Options, Orion, Roles};

/// The name the worker's session says itself as to the comm core.
const NAME: &str = "lemnos-orion-precheck";
/// Orion's interface level that has the precheck (`orion_connect::Feature::SecurityPrecheck`).
pub const NEEDS_LEVEL: u16 = 5;
/// The reply timeout of one call to Orion, and of one connect.
pub const CALL: Duration = Duration::from_millis(1500);
/// How long the driver waits for the worker's reply: the call's own timeout and a margin, under the
/// host's 2 s call deadline.
pub const WAIT: Duration = Duration::from_millis(1700);
const FIRST_NS: u64 = 1_000_000_000;
const CAP_NS: u64 = 8_000_000_000;
const IDLE: Duration = Duration::from_secs(3600);
const KEYS: &[Key] = &[Key::optional("address", Kind::Str)];

/// The delay before the next connect after `failures` failures in a row: 1 s, doubling, capped at 8 s.
#[must_use]
pub fn delay_ns(failures: u32) -> u64 {
    let doubled = FIRST_NS.saturating_mul(1u64 << failures.saturating_sub(1).min(16));
    doubled.min(CAP_NS)
}

/// The answer for one call's outcome, as the contract's text: `missing:` lines for what is against the
/// change, the empty text when nothing is. Each error is named, never swallowed.
#[must_use]
pub fn answer_for(outcome: std::result::Result<Vec<String>, Error>) -> String {
    match outcome {
        Ok(lines) => lines.join("\n"),
        Err(Error::Client(ClientError::Level { needs, node, .. })) => format!(
            "missing: orion: the node speaks interface level {node}; the precheck needs level {needs}"
        ),
        Err(error) => did_not_answer(&error.to_string()),
    }
}

/// The reason a precheck gives when Orion did not answer.
#[must_use]
pub fn did_not_answer(why: &str) -> String {
    format!("missing: orion: it did not answer ({why})")
}

/// Whether a call's outcome ends the session: a call that did not come back, or a lost one. A level
/// answer does not: the node is there and answers.
fn ends_session(outcome: &std::result::Result<Vec<String>, Error>) -> bool {
    matches!(outcome, Err(error) if !matches!(error, Error::Client(ClientError::Level { .. })))
}

fn address<'a>(cfg: &Config<'a>) -> std::result::Result<Address, ConfigError<'a>> {
    match cfg.str("address") {
        None | Some("") => Ok(Address::default()),
        Some(text) => text.parse().map_err(|_| {
            ConfigError::new(
                "address",
                "not an Orion address (a unix:<path> or a socket path)",
            )
        }),
    }
}

fn connect(address: &Address) -> std::result::Result<Orion, Error> {
    Orion::connect_once(
        address,
        NAME,
        Options {
            roles: Roles::CLIENT,
            timeout: CALL,
            ..Options::default()
        },
    )
}

struct Call {
    seq: u64,
    on: bool,
}

struct Reply {
    seq: u64,
    text: String,
}

/// The worker: owns the session, connects on the backoff, answers each call, and stops when the driver
/// is dropped.
fn serve(
    address: Address,
    calls: mpsc::Receiver<Call>,
    replies: mpsc::Sender<Reply>,
    connected: Arc<AtomicBool>,
    failures: Arc<AtomicU32>,
) {
    let mut orion: Option<Orion> = None;
    let mut next_try = Instant::now();
    loop {
        if orion.is_none() && Instant::now() >= next_try {
            match connect(&address) {
                Ok(session) => {
                    orion = Some(session);
                    failures.store(0, Ordering::Relaxed);
                    connected.store(true, Ordering::Relaxed);
                }
                Err(_) => {
                    let n = failures.fetch_add(1, Ordering::Relaxed).saturating_add(1);
                    next_try = Instant::now() + Duration::from_nanos(delay_ns(n));
                }
            }
        }
        let wait = if orion.is_some() {
            IDLE
        } else {
            next_try.saturating_duration_since(Instant::now())
        };
        match calls.recv_timeout(wait) {
            Ok(call) => {
                let to = if call.on {
                    SecurityTo::On
                } else {
                    SecurityTo::Off
                };
                let text = match orion.as_ref() {
                    None => did_not_answer("not connected; trying again"),
                    Some(session) => {
                        let outcome: std::result::Result<Vec<String>, Error> =
                            session.security_precheck(to);
                        if ends_session(&outcome) {
                            orion = None;
                            connected.store(false, Ordering::Relaxed);
                            failures.store(0, Ordering::Relaxed);
                            next_try = Instant::now();
                        }
                        answer_for(outcome)
                    }
                };
                if replies
                    .send(Reply {
                        seq: call.seq,
                        text,
                    })
                    .is_err()
                {
                    return;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

#[driver("orion-precheck", provides = [SecurityPrecondition])]
pub struct OrionPrecheck {
    calls: mpsc::Sender<Call>,
    replies: mpsc::Receiver<Reply>,
    seq: u64,
    connected: Arc<AtomicBool>,
}

impl Lifecycle for OrionPrecheck {
    type Needs = OrionPrecheckNeeds;

    fn start(_needs: OrionPrecheckNeeds, cfg: &Config<'_>) -> Result<Self> {
        let address = address(cfg).map_err(|_| Status::INVALID_ARGUMENT)?;
        let (calls, call_rx) = mpsc::channel();
        let (reply_tx, replies) = mpsc::channel();
        let connected = Arc::new(AtomicBool::new(false));
        let failures = Arc::new(AtomicU32::new(0));
        let worker_connected = Arc::clone(&connected);
        thread::Builder::new()
            .name("orion-precheck".to_owned())
            .spawn(move || serve(address, call_rx, reply_tx, worker_connected, failures))
            .map_err(|_| Status::NO_MEMORY)?;
        Ok(Self {
            calls,
            replies,
            seq: 0,
            connected,
        })
    }

    fn check_config<'a>(cfg: &Config<'a>) -> std::result::Result<(), ConfigError<'a>> {
        cfg.check_keys(KEYS)?;
        address(cfg).map(|_| ())
    }

    fn health(&self) -> Health {
        if self.connected.load(Ordering::Relaxed) {
            Health::Ok
        } else {
            Health::Degraded("orion: not connected; trying again")
        }
    }
}

impl OrionPrecheck {
    /// Asks the worker, and waits for the reply to this call, for at most [`WAIT`]. A reply to an
    /// earlier call (one that timed out) is dropped by its number.
    fn ask(&mut self, on: bool) -> String {
        self.seq = self.seq.wrapping_add(1);
        let seq = self.seq;
        if self.calls.send(Call { seq, on }).is_err() {
            return did_not_answer("the Orion session has stopped");
        }
        let until = Instant::now() + WAIT;
        loop {
            let wait = until.saturating_duration_since(Instant::now());
            match self.replies.recv_timeout(wait) {
                Ok(reply) if reply.seq == seq => return reply.text,
                Ok(_) => {}
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    return did_not_answer("no answer within the call's deadline");
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return did_not_answer("the Orion session has stopped");
                }
            }
        }
    }
}

impl SecurityPrecondition for OrionPrecheck {
    fn precondition(&mut self, on: bool, into: &mut [u8]) -> Result<usize> {
        let mut text = self.ask(on);
        if text.is_empty() {
            return Ok(0);
        }
        text.push('\n');
        let bytes = text.as_bytes();
        // A reply that does not fit is cut at a whole line where one fits, so the lines that remain
        // still say what is against the change.
        let n = if bytes.len() <= into.len() {
            bytes.len()
        } else {
            let limit = TEXT_MAX.min(into.len());
            into[..limit]
                .iter()
                .rposition(|b| *b == b'\n')
                .map_or(limit, |at| at + 1)
        };
        into[..n].copy_from_slice(&bytes[..n]);
        Ok(n)
    }

    fn apply(&mut self, _on: bool) -> Result<()> {
        // Orion keeps no boot-side mark for the switch.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orion::Feature;

    #[test]
    fn the_backoff_doubles_to_the_cap_and_stays_there() {
        let delays: Vec<u64> = (1..=6).map(|n| delay_ns(n) / 1_000_000_000).collect();
        assert_eq!(delays, [1, 2, 4, 8, 8, 8]);
    }

    #[test]
    fn no_line_is_a_yes() {
        assert_eq!(answer_for(Ok(Vec::new())), "");
    }

    #[test]
    fn each_line_of_orion_is_a_no_reason() {
        let lines = vec!["missing: orion: nobody but root could get in".to_owned()];
        assert_eq!(
            answer_for(Ok(lines)),
            "missing: orion: nobody but root could get in"
        );
    }

    #[test]
    fn a_level_below_the_precheck_says_needed_and_seen() {
        let level = Error::Client(ClientError::Level {
            feature: Feature::SecurityPrecheck,
            needs: NEEDS_LEVEL,
            node: 3,
        });
        assert_eq!(
            answer_for(Err(level)),
            "missing: orion: the node speaks interface level 3; the precheck needs level 5"
        );
    }

    #[test]
    fn any_other_error_is_did_not_answer_with_why() {
        let text = answer_for(Err(Error::Client(ClientError::Lost)));
        assert!(
            text.starts_with("missing: orion: it did not answer ("),
            "{text}"
        );
        assert!(text.ends_with(')'), "{text}");
    }

    #[test]
    fn a_lost_session_ends_but_a_level_answer_does_not() {
        let lost: std::result::Result<Vec<String>, Error> = Err(Error::Client(ClientError::Lost));
        let level: std::result::Result<Vec<String>, Error> =
            Err(Error::Client(ClientError::Level {
                feature: Feature::SecurityPrecheck,
                needs: NEEDS_LEVEL,
                node: 0,
            }));
        assert!(ends_session(&lost));
        assert!(!ends_session(&level));
        assert!(!ends_session(&Ok(Vec::new())));
    }
}
