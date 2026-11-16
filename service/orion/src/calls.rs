//! What Orion's actions ask of a device, run on a lemnosd connection of their own so a slow
//! call never holds up the values.
//!
//! Every device's resource has three actions, and each takes its arguments as text, the words
//! `lemnos-ctl` takes for the same thing:
//!
//! | Action | Arguments | Does |
//! | --- | --- | --- |
//! | `<resource>.set` | `ITEM VALUE...` | drives a control or action item |
//! | `<resource>.call` | `METHOD ARG...`, the method as `NAME` or `TAG:NAME` | calls any method; the reply is the output, as text |
//! | `<resource>.power` | `on` or `off` | switches the device |
//!
//! The device's description says what each word must be; nothing here knows a device.
//!
//! A method the description marks as a long job runs as one through `call`. The run is started
//! on lemnosd, and the job's progress goes back as the run's progress: `percent`, the message
//! `"<done> of <total>"`, and the data `done` and `total` as two little-endian `u64`s. The run
//! ends as the job does: its result as the output (text when it is UTF-8, hex otherwise),
//! `Cancelled` when it was cancelled, `Failed` otherwise. Cancelling the run cancels the job.
//!
//! Each Orion caller has a lemnosd connection of its own. lemnosd keeps the callers of a driver
//! apart by connection, so a driver that tracks callers sees each Orion caller as its own.
//!
//! A cancel stops the run and nothing else: the job is cancelled on lemnosd, and the caller's
//! connection stays, so the drivers keep what the caller holds. Orion's cancel says why it was sent,
//! and the bridge logs that reason as words; it is not passed on. A session's end is a different
//! message (`CallerGone`): the holds of the callers of that session are released at once, by closing
//! their lemnosd connections (see [`caller_gone`]). A silent caller's holds are bounded by a lease
//! (see [`expire`]): a caller silent for the lease is dropped, and lemnosd then tells the drivers so.
//! Only silence expires a lease: a request, a transfer chunk or a run's progress renews it (see
//! [`expire`] for the list).

use std::collections::HashMap;
use std::path::Path;
use std::sync::mpsc::{Receiver, RecvTimeoutError, TryRecvError};
use std::time::{Duration, Instant};

use lemnos_client::{Client, Error, JobEvent};
use lemnos_core::{ItemKind, Status};
use lemnos_wire::OwnedShape;
use lemnos_wire::text::{
    Call, encode_call_within, find_item, find_method, method_of, render_reply, too_small,
};
use orion::actions::Outcome;

use crate::{LEMNOSD_CALLS_NAME, Shared, log};

/// The largest reply read, as `lemnos-ctl` reads it.
const REPLY_MAX: usize = 64 * 1024;
/// The word `call` answers with the device's description, as text. No device method takes it.
pub const DESCRIBE: &str = "describe";
/// The most reply an action's output carries. Orion's client and comm core take frames of 64 KiB
/// (`client/connect` `Options::max_frame`, `comm/server` `Limits::max_frame`), and the action result
/// frame also carries the call, the outcome and the detail, so 4 KiB is kept for those. A reply
/// larger than this is refused, not cut short, and an out buffer never asks for more.
pub const ORION_REPLY_MAX: u32 = 60 * 1024;

/// What an action does to its device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    Set,
    Call,
    Power,
}

impl Verb {
    pub const ALL: [Self; 3] = [Self::Set, Self::Call, Self::Power];

    /// The last part of the action's name.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Set => "set",
            Self::Call => "call",
            Self::Power => "power",
        }
    }

    /// The action's description, for whoever lists it.
    pub const fn description(self) -> &'static str {
        match self {
            Self::Set => {
                "Drives a control or action item. Arguments: ITEM VALUE..., as lemnos-ctl set takes them"
            }
            Self::Call => {
                "Calls any method of the device. Arguments: METHOD ARG..., the method as NAME or TAG:NAME, as lemnos-ctl call takes them. The output is the reply as text. `describe` with no arguments gives the device description as text"
            }
            Self::Power => "Switches the device. Arguments: on or off",
        }
    }
}

/// One run of an action, handed to the call thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    /// The Orion session the run came on. An answer for a session that has ended is dropped.
    pub session: u64,
    /// The token of the Orion session that asked for the run (`Request::Run`'s `session`): Orion
    /// says it again when that session ends (`Request::CallerGone`). Zero from a node that does not
    /// say.
    pub token: u64,
    /// The run, as Orion numbered it.
    pub call: u64,
    pub device: String,
    pub verb: Verb,
    pub args: String,
    /// Who asked for the run, as Orion names the caller (`local:<uid>`, `operator:<key>`,
    /// `peer:<node>`, `device:<name>` or `anonymous`). Empty when Orion did not say.
    pub caller: String,
}

/// What the publisher hands the call thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    /// Run this.
    Run(Job),
    /// Stop run `call` of session `session`, if it is a long job still going. `reason` is Orion's
    /// cancel reason by its wire value (see [`cancel_words`]).
    Cancel { session: u64, call: u64, reason: u8 },
    /// The Orion session with token `token` ended: its callers' holds are released.
    CallerGone { token: u64 },
}

/// How far a run that is a long job has got.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    pub session: u64,
    pub call: u64,
    pub done: u64,
    pub total: u64,
}

impl Progress {
    /// Percent done, or 255 when the total isn't known.
    pub fn percent(&self) -> u8 {
        if self.total == 0 {
            return 255;
        }
        u8::try_from(u128::from(self.done.min(self.total)) * 100 / u128::from(self.total))
            .unwrap_or(100)
    }

    /// `done` and `total`, little-endian, for the run's progress data.
    pub fn data(&self) -> [u8; 16] {
        let mut data = [0; 16];
        data[..8].copy_from_slice(&self.done.to_le_bytes());
        data[8..].copy_from_slice(&self.total.to_le_bytes());
        data
    }
}

/// A long job a run started, until it ends.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Running {
    session: u64,
    /// The Orion session token the run came from ([`Job::token`]).
    token: u64,
    call: u64,
    job: u64,
    /// The caller the run came from: the key of its lemnosd connection.
    caller: String,
}

/// A session's lemnosd connection, and when its session last had a request: a run, a run's end or a
/// cancel. Its holds last for the lease after that, while no run of the session is in progress.
struct Lemnosd {
    client: Client,
    seen: Instant,
    /// The class of the session's identity (`local`, `operator`, ...), the only thing a log names.
    class: &'static str,
}

impl Lemnosd {
    fn new(client: Client, class: &'static str) -> Self {
        Self {
            client,
            seen: Instant::now(),
            class,
        }
    }
}

/// The lemnosd connections the calls go on, by caller.
type Callers = HashMap<CallerKey, Lemnosd>;

/// How long the call thread waits for a job's news before it looks for new runs again.
const JOB_WAIT: Duration = Duration::from_millis(50);
/// How often the call thread looks for a caller whose lease has run out, when no run is going.
const LEASE_TICK: Duration = Duration::from_millis(200);

/// How a run ended, as the bridge says it to Orion: Orion's outcome, and the action's status code a
/// caller branches on. The code is the magnitude of the lemnosd status the ending names (the Linux
/// error number: 16 for `BUSY`, 110 for `TIMED_OUT`); zero where no status is named.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ending {
    pub outcome: Outcome,
    pub code: u32,
}

impl Ending {
    /// It worked.
    pub const SUCCEEDED: Self = Self {
        outcome: Outcome::Succeeded,
        code: 0,
    };
    /// It ran, or tried to, and failed; no status is named.
    pub const FAILED: Self = Self {
        outcome: Outcome::Failed,
        code: 0,
    };
    /// Refused without running; no status is named (arguments that are not what the device takes).
    pub const REJECTED: Self = Self {
        outcome: Outcome::Rejected,
        code: 0,
    };
    /// Cancelled by the bridge, for a caller's own cancel or a session's end; no status is named.
    pub const CANCELLED: Self = Self {
        outcome: Outcome::Cancelled,
        code: 0,
    };
}

/// The lemnosd statuses that end a run as `Rejected`: refused before anything ran, so asking again or
/// asking differently may succeed.
const REFUSED: [Status; 8] = [
    Status::NOT_PERMITTED,
    Status::NOT_FOUND,
    Status::AGAIN,
    Status::BUSY,
    Status::NO_DEVICE,
    Status::INVALID_ARGUMENT,
    Status::NOT_SUPPORTED,
    Status::BUFFER_TOO_SMALL,
];

/// The ending a lemnosd status names, with the status as its code. `CANCELLED` is `Cancelled` and
/// `TIMED_OUT` is `TimedOut`; the statuses in [`REFUSED`] are `Rejected`; every other status ran and
/// failed, and is `Failed` with code zero. The table is in docs/daemon.md.
pub fn ending_of_status(status: Status) -> Ending {
    let code = status.0.unsigned_abs();
    let outcome = if status == Status::CANCELLED {
        Outcome::Cancelled
    } else if status == Status::TIMED_OUT {
        Outcome::TimedOut
    } else if REFUSED.contains(&status) {
        Outcome::Rejected
    } else {
        return Ending::FAILED;
    };
    Ending { outcome, code }
}

/// How a run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Done {
    pub session: u64,
    pub call: u64,
    pub ending: Ending,
    /// The reply as text, for `call`.
    pub output: String,
    /// Why, when it did not succeed.
    pub detail: String,
}

/// Runs what the publisher hands over until it stops sending.
///
/// Each caller's connection is made when a run from it comes and none is up, so a call made just
/// after lemnosd comes back reaches it, rather than waiting out a retry delay from while it was
/// away. A connection is kept for as long as the caller's lease lasts ([`expire`]).
pub fn run(socket: &Path, messages: &Receiver<Message>, shared: &Shared, lease: Option<Duration>) {
    let mut clients: Callers = HashMap::new();
    let mut running: Vec<Running> = Vec::new();
    let mut ended: Vec<CallerKey> = Vec::new();
    // Sessions that ended while a detached run of theirs was still going (see `caller_gone`).
    let mut departed: Vec<CallerKey> = Vec::new();
    loop {
        expire(
            &mut clients,
            &running,
            &mut ended,
            lease,
            Instant::now(),
            shared,
        );
        release_departed(&mut clients, &running, &mut ended, &mut departed, shared);
        let message = if !running.is_empty() {
            match messages.try_recv() {
                Ok(message) => Some(message),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => return,
            }
        } else if clients.is_empty() {
            match messages.recv() {
                Ok(message) => Some(message),
                Err(_) => return,
            }
        } else {
            match messages.recv_timeout(LEASE_TICK) {
                Ok(message) => Some(message),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        };
        match message {
            Some(Message::Run(job)) => {
                run_one(socket, &mut clients, &job, &mut running, &mut ended, shared);
            }
            Some(Message::CallerGone { token }) => {
                caller_gone(
                    &mut clients,
                    &running,
                    &mut ended,
                    &mut departed,
                    token,
                    shared,
                );
            }
            Some(Message::Cancel {
                session,
                call,
                reason,
            }) => {
                cancel(
                    &mut clients,
                    &mut running,
                    &mut ended,
                    session,
                    call,
                    reason,
                    shared,
                );
            }
            None if !running.is_empty() => follow(&mut clients, &mut running, &mut ended, shared),
            None => {}
        }
    }
}

/// The key a session's lemnosd connection is kept under (see [`caller_key`]).
pub type CallerKey = String;

/// The key a session's lemnosd connection, and its holds, are kept under: the Orion session's token,
/// so two sessions of one identity have connections of their own and ending one never releases the
/// other's holds. A token of zero (a node that does not say) is one shared key.
pub fn caller_key(token: u64) -> CallerKey {
    format!("session:{token}")
}

/// The class of a caller, as its identity names it: `local`, `operator`, `peer`, `device` or
/// `anonymous`. The class is all a log line or the hello document says of a caller; never its key.
pub fn caller_class(caller: &str) -> &'static str {
    match caller.split(':').next() {
        Some("local") => "local",
        Some("operator") => "operator",
        Some("peer") => "peer",
        Some("device") => "device",
        _ => "anonymous",
    }
}

/// Drops the connection of each caller whose lease has run out, when the lease is on. Each one is
/// released through [`release`], and its jobs end with its connection.
///
/// Only silence expires a lease. A caller's last renewal is the latest of:
///
/// - a request of it: a `call`, `set` or `power` run, or a `cancel` of its run;
/// - the end of one of its runs (its job's last news);
/// - a progress report of a long job it started (`Progress` news of its run);
/// - a file or stream chunk the bridge accepted (its answer says it is still receiving). Pinned Orion
///   does not say which session sent a chunk, so an accepted chunk renews every caller's lease.
///
/// The bridge's own housekeeping (its publishing, its hello document, its ticks) renews nothing, and
/// neither does a topic published to a caller.
///
/// A caller with a run in flight is alive, whether or not the run reports progress, so its lease does
/// not run out while the run is in flight, however silent the caller is. Such a run ends by its own end,
/// by its cancel, or by its session's end (`CallerGone`, see [`caller_gone`]), not by the lease. A caller
/// with no run in flight whose lease runs out is dropped here: its connection closes, and lemnosd
/// releases what it held.
///
/// A holder keeps its holds by renewing them: `<resource>.call describe` is the renewal with nothing
/// else to do.
// A session's end is released by `CallerGone` (see `caller_gone`) at once. The lease is the backstop
// for a caller that holds and says nothing: it is the only release for a silent holder.
fn expire(
    clients: &mut Callers,
    running: &[Running],
    ended: &mut Vec<CallerKey>,
    lease: Option<Duration>,
    now: Instant,
    shared: &Shared,
) {
    let Some(lease) = lease else {
        return;
    };
    let transferred = shared.last_transfer();
    let gone: Vec<CallerKey> = clients
        .iter()
        .filter(|(caller, held)| {
            // A caller with any run in flight is alive, whether or not the run reports progress. A session
            // that ends is released by CallerGone, not by its lease; a detached run keeps its session's
            // connection until it ends (see `caller_gone`).
            if running.iter().any(|r| r.caller == **caller) {
                return false;
            }
            let renewed = transferred.map_or(held.seen, |at| at.max(held.seen));
            now.saturating_duration_since(renewed) >= lease
        })
        .map(|(caller, _)| caller.clone())
        .collect();
    for caller in gone {
        release(clients, ended, &caller, "its lease ran out", shared);
    }
}

/// The most callers whose lease ended and who have had no reply since. The oldest is forgotten
/// first, so a caller that comes back much later sees no note.
pub const ENDED_MAX: usize = 64;

/// The note a caller's first reply after its lease ended carries, at the start of its `detail`. A
/// client matches the prefix `lease-ended:`; the rest of the detail is free text.
pub const LEASE_ENDED_NOTE: &str = "lease-ended: holds released";

/// Remembers that `caller`'s lease ended, so its next reply carries [`LEASE_ENDED_NOTE`].
fn remember(ended: &mut Vec<CallerKey>, caller: &str) {
    if ended.iter().any(|c| c == caller) {
        return;
    }
    if ended.len() == ENDED_MAX {
        ended.remove(0);
    }
    ended.push(caller.to_owned());
}

/// `detail` with the note in front when `caller`'s lease ended and it has had no reply since; the
/// note is given once.
fn noted(ended: &mut Vec<CallerKey>, caller: &str, detail: String) -> String {
    let Some(at) = ended.iter().position(|c| c == caller) else {
        return detail;
    };
    ended.remove(at);
    if detail.is_empty() {
        LEASE_ENDED_NOTE.to_owned()
    } else {
        format!("{LEASE_ENDED_NOTE}; {detail}")
    }
}

// Dropping the connection closes it on lemnosd, which tells the drivers the caller is gone. This is the
// one release of a session's holds: a session's end (`caller_gone`), its lease, or a dropped link.
fn release(
    clients: &mut Callers,
    ended: &mut Vec<CallerKey>,
    caller: &str,
    why: &str,
    shared: &Shared,
) {
    if let Some(held) = clients.remove(caller) {
        remember(ended, caller);
        log(&format!(
            "caller of class {} dropped: {why}; its connection to lemnosd closed, and lemnosd released what it held (1 connection)",
            held.class
        ));
        shared.with(|mirror| mirror.note_lease_end());
    }
}

/// Orion's cancel reasons by their wire value (Orion's `CancelReason`): 0 not said, 1 its caller
/// asked, 2 its caller's session ended, 3 its deadline passed, 4 node logic is stopping. A value
/// this bridge does not know is said as such.
pub(crate) fn cancel_words(reason: u8) -> &'static str {
    match reason {
        0 => "not said",
        1 => "its caller asked",
        2 => "its caller's session ended",
        3 => "its deadline passed",
        4 => "node logic is stopping",
        _ => "a reason this bridge does not know",
    }
}

/// Stops run `call` of `session`. A long job is cancelled on lemnosd, and the run ends as cancelled
/// at once: nothing else would answer it. The caller's connection is kept, and its lease starts from
/// now. The reason (Orion's `CancelReason`) is logged and does not change the run: the session's end
/// comes as `CallerGone`, which releases the holds (see [`caller_gone`]).
fn cancel(
    clients: &mut Callers,
    running: &mut Vec<Running>,
    ended: &mut Vec<CallerKey>,
    session: u64,
    call: u64,
    reason: u8,
    shared: &Shared,
) {
    let Some(at) = running
        .iter()
        .position(|r| r.session == session && r.call == call)
    else {
        return;
    };
    let r = running.remove(at);
    log(&format!("run {call} cancelled: {}", cancel_words(reason)));
    if let Some(lemnosd) = clients.get_mut(&r.caller) {
        // Its end, cancelled or not, comes as the job's news, which finds no run and is dropped.
        let _ = lemnosd.client.cancel_job(r.job);
        lemnosd.seen = Instant::now();
    }
    let detail = noted(ended, &r.caller, "cancelled".to_owned());
    shared.finished(Done {
        session: r.session,
        call: r.call,
        ending: Ending::CANCELLED,
        output: String::new(),
        detail,
    });
}

/// Connects the caller's connection to lemnosd, when it can be made.
fn connect(socket: &Path, clients: &mut Callers, caller: &str, class: &'static str) {
    if let Ok(client) = Client::connect(socket, LEMNOSD_CALLS_NAME) {
        clients.insert(caller.to_owned(), Lemnosd::new(client, class));
    }
}

/// Runs one job: answered now, or followed as a long job.
fn run_one(
    socket: &Path,
    clients: &mut Callers,
    job: &Job,
    running: &mut Vec<Running>,
    ended: &mut Vec<CallerKey>,
    shared: &Shared,
) {
    let key = caller_key(job.token);
    let caller = key.as_str();
    let class = caller_class(&job.caller);
    if let Some(lemnosd) = clients.get_mut(caller) {
        lemnosd.seen = Instant::now();
    }
    if !clients.get(caller).is_some_and(|l| l.client.is_connected()) {
        // Jobs on the connection that went are over: lemnosd ended them with it.
        lost(caller, running, shared);
        clients.remove(caller);
        connect(socket, clients, caller, class);
    }
    let mut served = match clients.get(caller) {
        Some(lemnosd) => serve(&lemnosd.client, job, shared),
        None => Err(Failure::Failed("lemnosd is not there".to_owned())),
    };
    // A connection that went quiet with lemnosd is found dead only when used: connect
    // again and run the job once more: the call never reached the lemnosd that is there now.
    if served.is_err()
        && clients
            .get(caller)
            .is_some_and(|l| !l.client.is_connected())
    {
        lost(caller, running, shared);
        clients.remove(caller);
        if let Ok(client) = Client::connect(socket, LEMNOSD_CALLS_NAME) {
            served = serve(&client, job, shared);
            clients.insert(caller.to_owned(), Lemnosd::new(client, class));
        }
    }
    let (ending, output, detail) = match served {
        Ok(Served::Started(started)) => {
            running.push(Running {
                token: job.token,
                session: job.session,
                call: job.call,
                job: started,
                caller: caller.to_owned(),
            });
            return;
        }
        Ok(Served::Output(output)) => (Ending::SUCCEEDED, output, String::new()),
        Err(Failure::Refused(why)) => (Ending::REJECTED, String::new(), why),
        Err(Failure::Status(status, why)) => (ending_of_status(status), String::new(), why),
        Err(Failure::Failed(why)) => (Ending::FAILED, String::new(), why),
    };
    let detail = noted(ended, caller, detail);
    shared.finished(Done {
        session: job.session,
        call: job.call,
        ending,
        output,
        detail,
    });
}

/// Takes the news of the long jobs on every caller's connection, waiting a little for it.
fn follow(
    clients: &mut Callers,
    running: &mut Vec<Running>,
    ended: &mut Vec<CallerKey>,
    shared: &Shared,
) {
    if clients.is_empty() {
        lost_all(running, shared);
        return;
    }
    let wait = JOB_WAIT / u32::try_from(clients.len()).unwrap_or(u32::MAX);
    let mut news = Vec::new();
    let mut gone = Vec::new();
    for (caller, lemnosd) in clients.iter() {
        match lemnosd.client.wait_job_event(wait) {
            Some(event) => news.push((caller.clone(), event)),
            None if !lemnosd.client.is_connected() => gone.push(caller.clone()),
            None => {}
        }
    }
    for caller in gone {
        lost(&caller, running, shared);
        clients.remove(&caller);
    }
    for (caller, event) in news {
        match event {
            JobEvent::Progress { job, done, total } => {
                // A long job that reports progress is its caller alive: the progress renews its lease.
                if let Some(r) = running.iter().find(|r| r.job == job && r.caller == caller) {
                    shared.progressed(Progress {
                        session: r.session,
                        call: r.call,
                        done,
                        total,
                    });
                    if let Some(lemnosd) = clients.get_mut(&caller) {
                        lemnosd.seen = Instant::now();
                    }
                }
            }
            JobEvent::Done {
                job,
                status,
                result,
            } => {
                let Some(at) = running
                    .iter()
                    .position(|r| r.job == job && r.caller == caller)
                else {
                    continue;
                };
                let r = running.remove(at);
                if let Some(lemnosd) = clients.get_mut(&caller) {
                    lemnosd.seen = Instant::now();
                }
                let mut done = job_done(&r, status, &result);
                done.detail = noted(ended, &r.caller, done.detail);
                shared.finished(done);
            }
        }
    }
}

/// How a run that was a long job ended.
fn job_done(run: &Running, status: Status, result: &[u8]) -> Done {
    let (ending, output, detail) = if status.is_ok() {
        (Ending::SUCCEEDED, result_text(result), String::new())
    } else {
        // The job ran, so a refusal's ending does not apply to it: only a cancel and a timeout keep theirs.
        let named = ending_of_status(status);
        let ending = if named.outcome == Outcome::Cancelled || named.outcome == Outcome::TimedOut {
            named
        } else {
            Ending::FAILED
        };
        let detail = if status == Status::CANCELLED {
            "cancelled".to_owned()
        } else {
            format!("the job ended with status {}", status.0)
        };
        (ending, String::new(), detail)
    };
    Done {
        session: run.session,
        call: run.call,
        ending,
        output,
        detail,
    }
}

/// A job's result as text: itself when it is UTF-8, hex otherwise.
pub fn result_text(result: &[u8]) -> String {
    match core::str::from_utf8(result) {
        Ok(text) => text.to_owned(),
        Err(_) => result.iter().map(|b| format!("{b:02x}")).collect(),
    }
}

/// Ends the followed jobs of one caller as failed: its lemnosd connection went, and its jobs
/// with it.
fn lost(caller: &str, running: &mut Vec<Running>, shared: &Shared) {
    ended_with(
        caller,
        running,
        shared,
        "lemnosd went away; the job ended with it",
    );
}

/// Orion's session `token` ended (`Request::CallerGone`). Orion cancels the session's attached runs
/// before it says so (`Cancel`, reason 2), so a run of the session still in flight is a detached one,
/// which goes on past its caller. Its connection stays: lemnosd ends a job with the connection it was
/// started on. The connection is let go when its last run ends ([`release_departed`]). With nothing in
/// flight, the session's connection is released now, as a dropped one is: its holds go, and lemnosd's
/// drivers hear the caller is gone. A session with no connection here is a no-op.
fn caller_gone(
    clients: &mut Callers,
    running: &[Running],
    ended: &mut Vec<CallerKey>,
    departed: &mut Vec<CallerKey>,
    token: u64,
    shared: &Shared,
) {
    let caller = caller_key(token);
    match departure(running, &caller) {
        Departure::Keep => {
            if !departed.contains(&caller) {
                departed.push(caller);
            }
        }
        Departure::Release => release(clients, ended, &caller, "its session ended", shared),
    }
}

/// What a session's end does to its connection (see [`caller_gone`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Departure {
    /// Nothing of the session is in flight: its connection is released now.
    Release,
    /// A run of the session is still in flight. Its connection is kept until that run ends.
    Keep,
}

/// The decision at `CallerGone`. Orion has already cancelled the session's attached runs, with reason 2,
/// and [`cancel`] removes each one it cancels from `running`. So a run still in `running` at `CallerGone`
/// is one that never got its reason-2 cancel: a detached run, or an ambiguous one. It is always kept
/// (`Keep`), because ending a run that might be detached is the unsafe error. Only the order Orion sends
/// the two messages in makes the rest of this safe; `live_orion_order_*` in `orion_bridge.rs` pins it at
/// the pinned revision.
fn departure(running: &[Running], caller: &str) -> Departure {
    if running.iter().any(|r| r.caller == caller) {
        Departure::Keep
    } else {
        Departure::Release
    }
}

/// Releases each departed session whose last detached run has ended (see [`caller_gone`]).
fn release_departed(
    clients: &mut Callers,
    running: &[Running],
    ended: &mut Vec<CallerKey>,
    departed: &mut Vec<CallerKey>,
    shared: &Shared,
) {
    departed.retain(|caller| {
        if running.iter().any(|r| r.caller == *caller) {
            return true;
        }
        release(
            clients,
            ended,
            caller,
            "its session ended, and its detached run ended",
            shared,
        );
        false
    });
}

/// Ends the followed jobs of one caller as failed, with `detail` as the reason.
fn ended_with(caller: &str, running: &mut Vec<Running>, shared: &Shared, detail: &str) {
    let mut kept = Vec::with_capacity(running.len());
    for r in running.drain(..) {
        if r.caller != caller {
            kept.push(r);
            continue;
        }
        shared.finished(Done {
            session: r.session,
            call: r.call,
            ending: Ending::FAILED,
            output: String::new(),
            detail: detail.to_owned(),
        });
    }
    *running = kept;
}

/// Ends every followed job as failed, when there is no connection at all.
fn lost_all(running: &mut Vec<Running>, shared: &Shared) {
    let callers: Vec<String> = running.iter().map(|r| r.caller.clone()).collect();
    for caller in callers {
        lost(&caller, running, shared);
    }
}

/// What a run did when it didn't fail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Served {
    /// It is over: the reply as text, or empty.
    Output(String),
    /// It started this long job.
    Started(u64),
}

/// Why a run did not succeed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// It was not run: arguments that are not what the device takes, or no such item.
    Refused(String),
    /// lemnosd answered with this status: its ending is [`ending_of_status`].
    Status(Status, String),
    /// It ran, or tried to, and failed: lemnosd is not there, or the bridge could not do it.
    Failed(String),
}

fn failed(error: Error) -> Failure {
    match &error {
        Error::Refused { status, .. } => Failure::Status(*status, error.to_string()),
        _ => Failure::Failed(error.to_string()),
    }
}

/// A call's failure. A reply that did not fit the out buffer is named as such, with how to ask
/// for more; the bridge never sends a partial reply.
fn refused_reply(error: Error, call: &Call) -> Failure {
    match &error {
        Error::Refused { status, .. } if *status == Status::BUFFER_TOO_SMALL => Failure::Status(
            Status::BUFFER_TOO_SMALL,
            format!("{error}: {}", too_small(call)),
        ),
        _ => failed(error),
    }
}

/// Does one job: its reply as text, or the long job it started.
///
/// # Errors
///
/// Why it did not succeed.
pub fn serve(client: &Client, job: &Job, shared: &Shared) -> Result<Served, Failure> {
    let words: Vec<&str> = job.args.split_whitespace().collect();
    let device = client
        .list()
        .map_err(failed)?
        .into_iter()
        .find(|d| d.name == job.device)
        .ok_or_else(|| Failure::Failed(format!("lemnosd has no device {:?}", job.device)))?;
    if job.verb == Verb::Power {
        let on = match words.as_slice() {
            ["on"] => true,
            ["off"] => false,
            _ => return Err(Failure::Refused("power takes on or off".to_owned())),
        };
        client.set_power(device.index, on).map_err(failed)?;
        return Ok(Served::Output(String::new()));
    }
    let Some((wanted, values)) = words.split_first() else {
        let what = if job.verb == Verb::Set {
            "an item"
        } else {
            "a method"
        };
        return Err(Failure::Refused(format!(
            "{} needs {what}",
            job.verb.name()
        )));
    };
    let description = crate::source::describe(client, device.index).map_err(failed)?;
    if job.verb == Verb::Call && *wanted == DESCRIBE {
        if !values.is_empty() {
            return Err(Failure::Refused("describe takes no arguments".to_owned()));
        }
        // The generation comes from the mirror, the field the device's topic publishes.
        let (generation, lease) = shared.with(|mirror| {
            (
                mirror.description_generation(&device.name),
                mirror.lease_seconds(),
            )
        });
        // A description the format cannot take in full (a bound, a control character) is refused by name.
        let text =
            crate::description::render(&description, generation, lease).map_err(Failure::Failed)?;
        if text.len() > ORION_REPLY_MAX as usize {
            return Err(Failure::Failed(format!(
                "the description is {} bytes; the reply carries at most {ORION_REPLY_MAX}, and a partial one is never sent",
                text.len()
            )));
        }
        return Ok(Served::Output(text));
    }
    let (contract, method) = if job.verb == Verb::Set {
        let item = find_item(&description, wanted).map_err(Failure::Refused)?;
        if item.kind == ItemKind::Value {
            return Err(Failure::Refused(format!("{wanted} is read-only")));
        }
        method_of(&description, item).map_err(Failure::Refused)?
    } else {
        find_method(&description, wanted).map_err(Failure::Refused)?
    };
    let call = encode_call_within(method, values, ORION_REPLY_MAX).map_err(Failure::Refused)?;
    let tag = &description.contracts[contract].tag;
    let mut handle = client.open_tag(&device.name, tag).map_err(failed)?;
    if method.job {
        let started = client
            .start_job(&mut handle, method.id, &call.args)
            .map_err(failed)?;
        return Ok(Served::Started(started));
    }
    let mut reply = vec![0; REPLY_MAX.max(call.reply_len())];
    let len = client
        .call(&mut handle, method.id, &call.args, &mut reply)
        .map_err(|error| refused_reply(error, &call))?;
    if job.verb == Verb::Set || method.returns == OwnedShape::Unit {
        return Ok(Served::Output(String::new()));
    }
    render_reply(method, &reply[..len])
        .map(Served::Output)
        .map_err(Failure::Failed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_caller_is_named_by_its_class_never_its_key() {
        assert_eq!(caller_class("local:1000"), "local");
        assert_eq!(caller_class("operator:0f3a-secret"), "operator");
        assert_eq!(caller_class("peer:node-7"), "peer");
        assert_eq!(caller_class("device:imu"), "device");
        assert_eq!(caller_class("anonymous"), "anonymous");
        assert_eq!(caller_class(""), "anonymous");
        assert_eq!(
            caller_class("root:0"),
            "anonymous",
            "an unknown class is anonymous"
        );
    }

    #[test]
    fn each_verb_has_its_own_name() {
        let names: Vec<&str> = Verb::ALL.iter().map(|verb| verb.name()).collect();
        assert_eq!(names, ["set", "call", "power"]);
    }

    #[test]
    fn progress_is_a_percent_and_its_numbers() {
        let at = |done, total| Progress {
            session: 1,
            call: 2,
            done,
            total,
        };
        assert_eq!(at(50, 200).percent(), 25);
        assert_eq!(at(7, 0).percent(), 255);
        assert_eq!(at(9, 4).percent(), 100);
        assert_eq!(at(u64::MAX, u64::MAX).percent(), 100);
        let data = at(3, 9).data();
        assert_eq!(u64::from_le_bytes(data[..8].try_into().unwrap()), 3);
        assert_eq!(u64::from_le_bytes(data[8..].try_into().unwrap()), 9);
    }

    #[test]
    fn a_result_is_text_or_hex() {
        assert_eq!(result_text(b"os-2"), "os-2");
        assert_eq!(result_text(&[0xff, 0x01]), "ff01");
        assert_eq!(result_text(b""), "");
    }
}

#[cfg(test)]
mod cancel_reason_tests {
    use super::*;

    /// Both reasons the coordinator named are said in words, and each one differs.
    #[test]
    fn each_reason_says_who_cancelled() {
        assert_eq!(cancel_words(1), "its caller asked");
        assert_eq!(cancel_words(2), "its caller's session ended");
        assert_ne!(cancel_words(1), cancel_words(2));
        assert_eq!(cancel_words(9), "a reason this bridge does not know");
    }
}

#[cfg(test)]
mod departure_tests {
    use super::*;

    fn run(caller: &str, call: u64) -> Running {
        Running {
            session: 1,
            token: 2,
            call,
            job: call,
            caller: caller.to_owned(),
        }
    }

    /// Attached: Orion's reason-2 cancel comes first and removes the run, so `CallerGone` finds nothing
    /// in flight and the session is released. Order recorded from Orion's `gone()` at 671d29d
    /// (`crates/node/action-list/src/lib.rs`), pinned live by `orion_order_pins_*`.
    #[test]
    fn a_session_whose_attached_runs_were_cancelled_is_released() {
        let running: Vec<Running> = Vec::new();
        assert_eq!(departure(&running, &caller_key(2)), Departure::Release);
    }

    /// Detached: no cancel is sent, the run stays in flight at `CallerGone`, so the connection is kept
    /// until the run ends.
    #[test]
    fn a_session_with_a_run_in_flight_is_kept() {
        let running = vec![run(&caller_key(2), 7)];
        assert_eq!(departure(&running, &caller_key(2)), Departure::Keep);
    }

    /// The ambiguous case: a run in flight at `CallerGone` with no reason-2 cancel for it. Whether it is
    /// detached cannot be told from here, so it is kept. A run of another session does not keep this one.
    #[test]
    fn an_ambiguous_run_is_kept_and_another_sessions_run_does_not_keep_it() {
        let running = vec![run(&caller_key(9), 3)];
        assert_eq!(departure(&running, &caller_key(2)), Departure::Release);
        let running = vec![run(&caller_key(2), 3), run(&caller_key(9), 4)];
        assert_eq!(departure(&running, &caller_key(2)), Departure::Keep);
    }
}

#[cfg(test)]
mod ending_tests {
    use super::*;

    fn run() -> Running {
        Running {
            session: 1,
            token: 2,
            call: 3,
            job: 4,
            caller: caller_key(2),
        }
    }

    /// The table: each lemnosd status that has a better-fitting ending maps to it, with its magnitude
    /// as the code. This is the table in docs/daemon.md.
    #[test]
    fn each_named_status_has_its_ending_and_its_code() {
        let rows = [
            (Status::CANCELLED, Outcome::Cancelled, 125),
            (Status::TIMED_OUT, Outcome::TimedOut, 110),
            (Status::NOT_PERMITTED, Outcome::Rejected, 1),
            (Status::NOT_FOUND, Outcome::Rejected, 2),
            (Status::AGAIN, Outcome::Rejected, 11),
            (Status::BUSY, Outcome::Rejected, 16),
            (Status::NO_DEVICE, Outcome::Rejected, 19),
            (Status::INVALID_ARGUMENT, Outcome::Rejected, 22),
            (Status::NOT_SUPPORTED, Outcome::Rejected, 95),
            (Status::BUFFER_TOO_SMALL, Outcome::Rejected, 4098),
        ];
        for (status, outcome, code) in rows {
            assert_eq!(
                ending_of_status(status),
                Ending { outcome, code },
                "{status:?}"
            );
        }
    }

    /// Every other status ran and failed: `Failed`, with no code (zero).
    #[test]
    fn the_other_statuses_are_failed_with_no_code() {
        for status in [
            Status::IO,
            Status::INTERRUPTED,
            Status::NO_MEMORY,
            Status::ACCESS,
            Status::STALE_HANDLE,
            Status::VERSION_MISMATCH,
            Status::PANIC,
            Status::LAYOUT_MISMATCH,
            // A torn read comes from a frame stream, which no action reads, so no action can end
            // with it. Were a device ever to return it, it is a failure, like any other.
            Status::TORN,
            Status(-999),
        ] {
            assert_eq!(ending_of_status(status), Ending::FAILED, "{status:?}");
        }
    }

    /// The plain endings carry their outcome and no code.
    #[test]
    fn the_plain_endings_carry_no_code() {
        assert_eq!(Ending::SUCCEEDED.outcome, Outcome::Succeeded);
        assert_eq!(Ending::FAILED.outcome, Outcome::Failed);
        assert_eq!(Ending::REJECTED.outcome, Outcome::Rejected);
        assert_eq!(Ending::CANCELLED.outcome, Outcome::Cancelled);
        for ending in [
            Ending::SUCCEEDED,
            Ending::FAILED,
            Ending::REJECTED,
            Ending::CANCELLED,
        ] {
            assert_eq!(ending.code, 0, "{ending:?}");
        }
    }

    /// A lemnosd refusal is kept with its status, so its ending is the table's.
    #[test]
    fn a_lemnosd_refusal_keeps_its_status() {
        let error = Error::Refused {
            status: Status::BUSY,
            text: "held by another caller".to_owned(),
        };
        assert!(matches!(
            failed(error),
            Failure::Status(Status::BUSY, why) if why.contains("held by another caller")
        ));
        assert!(matches!(
            failed(Error::Refused {
                status: Status::IO,
                text: String::new()
            }),
            Failure::Status(Status::IO, _)
        ));
    }

    /// A job that ran and succeeds is `Succeeded`, with its result as the output.
    #[test]
    fn a_job_that_succeeds_is_succeeded() {
        let done = job_done(&run(), Status::OK, b"done");
        assert_eq!(done.ending, Ending::SUCCEEDED);
        assert_eq!(done.output, "done");
    }

    /// A job cancelled on lemnosd is `Cancelled`, with lemnosd's CANCELLED code.
    #[test]
    fn a_job_that_is_cancelled_is_cancelled() {
        let done = job_done(&run(), Status::CANCELLED, b"");
        assert_eq!(
            done.ending,
            Ending {
                outcome: Outcome::Cancelled,
                code: 125
            }
        );
        assert_eq!(done.detail, "cancelled");
    }

    /// A job that ends `TIMED_OUT` is `TimedOut`, with its code.
    #[test]
    fn a_job_that_times_out_is_timed_out() {
        let done = job_done(&run(), Status::TIMED_OUT, b"");
        assert_eq!(
            done.ending,
            Ending {
                outcome: Outcome::TimedOut,
                code: 110
            }
        );
    }

    /// A job ran, so a refusal's status does not make it `Rejected`: it is `Failed`, with no code.
    #[test]
    fn a_job_that_fails_is_failed_whatever_the_status() {
        for status in [Status::IO, Status::BUSY, Status::NOT_FOUND] {
            assert_eq!(
                job_done(&run(), status, b"").ending,
                Ending::FAILED,
                "{status:?}"
            );
        }
    }
}
