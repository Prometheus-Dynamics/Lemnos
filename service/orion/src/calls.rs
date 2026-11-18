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
    /// The run goes on past its caller (`Request::Run`'s `detached`): a `CallerGone` for its session
    /// does not end it. False from a node that does not say.
    pub detached: bool,
    /// The run, as Orion numbered it.
    pub call: u64,
    pub device: String,
    pub verb: Verb,
    pub args: String,
    /// Who asked for the run, as Orion names the caller (`local:<uid>`, `operator:<key>`,
    /// `peer:<node>`, `device:<name>` or `anonymous`). Empty when Orion did not say.
    pub caller: String,
    /// The run's deadline in milliseconds from when the bridge takes it (`Request::Run`'s
    /// `deadline_ms`): a run still going when it passes is cancelled and ends `TimedOut`, the
    /// caller's deadline honoured here as well as by Orion's node. Zero for none.
    pub deadline_ms: u64,
}

/// What the publisher hands the call thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    /// Run this.
    Run(Job),
    /// Stop run `call`, if it is a long job still going. `call` is the node-issued number of the run,
    /// unique for the node logic's life, so it names one run with no session. `reason` is Orion's
    /// cancel reason by its wire value (see [`cancel_words`]).
    Cancel { call: u64, reason: u8 },
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
    /// The run goes on past its caller ([`Job::detached`]).
    detached: bool,
    call: u64,
    job: u64,
    /// The caller the run came from: the key of its lemnosd connection.
    caller: String,
    /// When the run's deadline passes, if it has one ([`Job::deadline_ms`]). Once the run is
    /// cancelling, the bound of its wait for lemnosd's answer (see [`begin_cancel`]).
    due: Option<Instant>,
    /// A cancel was sent to lemnosd and the run is open until lemnosd says how the job ended. The run
    /// is reported as that end, never as a cancel of its own.
    cancelling: bool,
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

/// The fact about a run's session that a reply carries, as a typed value: the code means the fact,
/// never words. Present on every run's reply, whatever its outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionNote {
    /// Nothing about the session.
    None,
    /// The caller's lease ended before this reply, and its holds were released (see [`expire`]).
    HoldsReleased,
}

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
    /// Ran out of time: the run's deadline passed, or its job went too long without progress. The
    /// code is lemnosd's `TIMED_OUT` (110), the same as Orion's node gives a deadline.
    pub const TIMED_OUT: Self = Self {
        outcome: Outcome::TimedOut,
        code: 110,
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
    /// The session's fact about this run, typed (see [`SessionNote`]).
    pub session_note: SessionNote,
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
        for done in expire_runs(&mut clients, &mut running, &mut ended, Instant::now()) {
            shared.finished(done);
        }
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
                    &mut running,
                    &mut ended,
                    &mut departed,
                    token,
                    shared,
                );
            }
            Some(Message::Cancel { call, reason }) => {
                cancel(&mut clients, &mut running, &mut ended, call, reason, shared);
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
/// note is given once. The second value is the typed fact for the same reply.
fn noted(ended: &mut Vec<CallerKey>, caller: &str, detail: String) -> (String, SessionNote) {
    let Some(at) = ended.iter().position(|c| c == caller) else {
        return (detail, SessionNote::None);
    };
    ended.remove(at);
    let text = if detail.is_empty() {
        LEASE_ENDED_NOTE.to_owned()
    } else {
        format!("{LEASE_ENDED_NOTE}; {detail}")
    };
    (text, SessionNote::HoldsReleased)
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

/// How long a cancelling run waits for lemnosd to say how its job ended. Past it the run ends as
/// failed, and never as cancelled: the bridge does not know whether the install stopped or finished.
pub const CANCEL_WAIT: Duration = Duration::from_secs(5);

/// Sends the cancel of run `r`'s job to lemnosd, and leaves the run open: lemnosd's own end of the job
/// is the run's report, so the run says what really happened (the driver's contract: a job cancelled
/// stops and ends as cancelled, one that got there first ends as it really ended). `Err` is the
/// reason the run ends now, as failed: there is no connection to ask, or lemnosd refused the cancel.
/// `NOT_FOUND` is not a refusal: the job is over in lemnosd and its end is on its way.
fn begin_cancel(clients: &mut Callers, r: &mut Running, now: Instant) -> Result<(), String> {
    let Some(lemnosd) = clients.get_mut(&r.caller) else {
        return Err("lemnosd is not there: the job ended with its connection, and it is not known how it ended".to_owned());
    };
    lemnosd.seen = now;
    match lemnosd.client.cancel_job(r.job) {
        Ok(()) => {}
        Err(Error::Refused { status, .. }) if status == Status::NOT_FOUND => {}
        Err(error) => {
            return Err(format!(
                "lemnosd did not take the cancel ({error}): it is not known how the job ended"
            ));
        }
    }
    r.cancelling = true;
    r.due = Some(now + CANCEL_WAIT);
    Ok(())
}

/// The run ends as failed, because its cancel could not be carried out or answered: never as cancelled.
fn failed_cancel(r: &Running, ended: &mut Vec<CallerKey>, detail: String) -> Done {
    let (detail, session_note) = noted(ended, &r.caller, detail);
    Done {
        session: r.session,
        call: r.call,
        ending: Ending::FAILED,
        output: String::new(),
        detail,
        session_note,
    }
}

/// Stops run `call`. A long job is cancelled on lemnosd, and the run ends as lemnosd says the job did:
/// cancelled when the cancel stopped it, or as it really ended when the job got there first (see
/// [`begin_cancel`] and [`follow`]). The run is never reported as cancelled before lemnosd answers. The
/// reason (Orion's `CancelReason`) is logged and does not change the run: the session's end comes as
/// `CallerGone`, which releases the holds (see [`caller_gone`]). A cancel carries no session: the
/// node's number for the run is unique, so the call alone names it.
fn cancel(
    clients: &mut Callers,
    running: &mut Vec<Running>,
    ended: &mut Vec<CallerKey>,
    call: u64,
    reason: u8,
    shared: &Shared,
) {
    let Some(at) = running.iter().position(|r| r.call == call) else {
        return;
    };
    log(&format!("run {call} cancelled: {}", cancel_words(reason)));
    if running[at].cancelling {
        return;
    }
    match begin_cancel(clients, &mut running[at], Instant::now()) {
        Ok(()) => {}
        Err(detail) => {
            let r = running.remove(at);
            shared.finished(failed_cancel(&r, ended, detail));
        }
    }
}

/// Ends the runs whose deadline has passed at `now`: each is cancelled on lemnosd and ends
/// `TimedOut` with code 110, as Orion's node ends a deadline. A run that is still going is never ended
/// here before its deadline; a run that ends first is taken off `running` by its own end, so its
/// deadline has nothing to act on.
fn expire_runs(
    clients: &mut Callers,
    running: &mut Vec<Running>,
    ended: &mut Vec<CallerKey>,
    now: Instant,
) -> Vec<Done> {
    let mut replies = Vec::new();
    let mut at = 0;
    while at < running.len() {
        if !running[at].due.is_some_and(|due| due <= now) {
            at += 1;
            continue;
        }
        let r = running.remove(at);
        if r.cancelling {
            log(&format!(
                "run {} cancelled, and lemnosd did not say how it ended",
                r.call
            ));
            replies.push(failed_cancel(
                &r,
                ended,
                format!(
                    "lemnosd did not say how the job ended within {} s: it is not reported as cancelled",
                    CANCEL_WAIT.as_secs()
                ),
            ));
            continue;
        }
        log(&format!("run {} ran out of time", r.call));
        if let Some(lemnosd) = clients.get_mut(&r.caller) {
            let _ = lemnosd.client.cancel_job(r.job);
            lemnosd.seen = now;
        }
        let (detail, session_note) = noted(ended, &r.caller, "it ran out of time".to_owned());
        replies.push(Done {
            session: r.session,
            call: r.call,
            ending: Ending::TIMED_OUT,
            output: String::new(),
            detail,
            session_note,
        });
    }
    replies
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
                detached: job.detached,
                session: job.session,
                call: job.call,
                job: started,
                caller: caller.to_owned(),
                due: (job.deadline_ms != 0)
                    .then(|| Instant::now() + Duration::from_millis(job.deadline_ms)),
                cancelling: false,
            });
            return;
        }
        Ok(Served::Output(output)) => (Ending::SUCCEEDED, output, String::new()),
        Err(Failure::Refused(why)) => (Ending::REJECTED, String::new(), why),
        Err(Failure::Status(status, why)) => (ending_of_status(status), String::new(), why),
        Err(Failure::Failed(why)) => (Ending::FAILED, String::new(), why),
    };
    let (detail, session_note) = noted(ended, caller, detail);
    shared.finished(Done {
        session: job.session,
        call: job.call,
        ending,
        output,
        detail,
        session_note,
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
                (done.detail, done.session_note) = noted(ended, &r.caller, done.detail);
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
        } else if status == Status::TIMED_OUT {
            // Lemnosd ends a job as timed out only when it goes too long without progress.
            "it went too long without progress".to_owned()
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
        session_note: SessionNote::None,
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

/// Orion's session `token` ended (`Request::CallerGone`). The session's attached runs end here: Orion
/// has already cancelled them with reason 2, and any still in flight is ended as a cancel ends one
/// ([`end_attached`]). A run the request marked detached goes on past its caller: it keeps the
/// session's connection, since lemnosd ends a job with the connection it was started on, and the
/// connection is let go when its last detached run ends ([`release_departed`]). With no detached run
/// left, the session's connection is released now, as a dropped one is: its holds go, and lemnosd's
/// drivers hear the caller is gone. A session with no connection here is a no-op.
fn caller_gone(
    clients: &mut Callers,
    running: &mut Vec<Running>,
    ended: &mut Vec<CallerKey>,
    departed: &mut Vec<CallerKey>,
    token: u64,
    shared: &Shared,
) {
    let caller = caller_key(token);
    end_attached(clients, running, ended, &caller, shared);
    match departure(running, &caller) {
        Departure::Keep => {
            if !departed.contains(&caller) {
                departed.push(caller);
            }
        }
        Departure::Release => release(clients, ended, &caller, "its session ended", shared),
    }
}

/// Ends the attached runs of `caller` as cancelled, each as [`cancel`] ends one. Orion's reason-2
/// cancel has normally ended them already, so this only does anything when that cancel did not come.
/// A detached run is never ended here.
fn end_attached(
    clients: &mut Callers,
    running: &mut Vec<Running>,
    ended: &mut Vec<CallerKey>,
    caller: &str,
    shared: &Shared,
) {
    let now = Instant::now();
    let mut kept = Vec::with_capacity(running.len());
    for mut r in running.drain(..) {
        if r.caller != caller || r.detached || r.cancelling {
            kept.push(r);
            continue;
        }
        match begin_cancel(clients, &mut r, now) {
            Ok(()) => kept.push(r),
            Err(detail) => shared.finished(failed_cancel(&r, ended, detail)),
        }
    }
    *running = kept;
}

/// What a session's end does to its connection (see [`caller_gone`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Departure {
    /// Nothing of the session is in flight that goes on past it: its connection is released now.
    Release,
    /// A detached run of the session is still in flight. Its connection is kept until that run ends.
    Keep,
}

/// The decision at `CallerGone`, once [`end_attached`] has ended the attached runs: the connection is
/// kept while a detached run of the caller is in flight, which the request said goes on, and released
/// otherwise. A run of another session does not keep this one.
fn departure(running: &[Running], caller: &str) -> Departure {
    if running
        .iter()
        .any(|r| r.caller == caller && (r.detached || r.cancelling))
    {
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
            session_note: SessionNote::None,
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

    fn run(caller: &str, call: u64, detached: bool) -> Running {
        Running {
            session: 1,
            token: 2,
            detached,
            call,
            job: call,
            caller: caller.to_owned(),
            due: None,
            cancelling: false,
        }
    }

    /// Attached runs of the session are ended before the decision (`end_attached`), so the session
    /// with nothing detached in flight is released.
    #[test]
    fn a_session_with_nothing_detached_in_flight_is_released() {
        let running: Vec<Running> = Vec::new();
        assert_eq!(departure(&running, &caller_key(2)), Departure::Release);
    }

    /// The request marked the run detached: its connection is kept until the run ends.
    #[test]
    fn a_session_with_a_detached_run_in_flight_is_kept() {
        let running = vec![run(&caller_key(2), 7, true)];
        assert_eq!(departure(&running, &caller_key(2)), Departure::Keep);
    }

    /// A run that is not detached never keeps the session, whatever is still in `running`: it is
    /// ended by `end_attached` first. A detached run of another session does not keep this one.
    #[test]
    fn an_attached_run_does_not_keep_and_another_sessions_detached_run_does_not() {
        let running = vec![run(&caller_key(2), 3, false)];
        assert_eq!(departure(&running, &caller_key(2)), Departure::Release);
        let running = vec![run(&caller_key(9), 4, true)];
        assert_eq!(departure(&running, &caller_key(2)), Departure::Release);
        let running = vec![run(&caller_key(2), 3, false), run(&caller_key(2), 5, true)];
        assert_eq!(departure(&running, &caller_key(2)), Departure::Keep);
    }
}

#[cfg(test)]
mod deadline_tests {
    use super::*;

    fn run(call: u64, due: Option<Instant>) -> Running {
        Running {
            session: 1,
            token: 2,
            detached: false,
            call,
            job: call + 100,
            caller: caller_key(2),
            due,
            cancelling: false,
        }
    }

    /// A run whose deadline has passed ends `TimedOut` with code 110, and is off the books; a run whose
    /// deadline has not passed, or that has none, is still running.
    #[test]
    fn a_run_past_its_deadline_ends_timed_out_and_the_others_keep_going() {
        let now = Instant::now();
        let mut running = vec![
            run(1, Some(now - Duration::from_millis(1))),
            run(2, Some(now + Duration::from_secs(60))),
            run(3, None),
        ];
        let replies = expire_runs(&mut Callers::new(), &mut running, &mut Vec::new(), now);
        assert_eq!(replies.len(), 1, "only the run past its deadline ends");
        assert_eq!(replies[0].call, 1);
        assert_eq!(replies[0].ending, Ending::TIMED_OUT);
        assert_eq!(replies[0].ending.code, 110);
        assert_eq!(replies[0].detail, "it ran out of time");
        assert_eq!(running.iter().map(|r| r.call).collect::<Vec<_>>(), [2, 3]);
    }

    /// The typed fact is given on the reply after a lease ended, whatever the outcome, and the code
    /// stays the outcome's: the words only say it for people.
    #[test]
    fn a_reply_after_a_lease_ended_carries_the_fact_as_a_typed_value() {
        let now = Instant::now();
        let mut ended = vec![caller_key(2)];
        let mut running = vec![run(1, Some(now))];
        let replies = expire_runs(&mut Callers::new(), &mut running, &mut ended, now);
        assert_eq!(replies[0].session_note, SessionNote::HoldsReleased);
        assert_eq!(
            replies[0].ending,
            Ending::TIMED_OUT,
            "the outcome is not changed by the note"
        );
        assert!(ended.is_empty(), "the note is given once");

        let mut running = vec![run(2, Some(now))];
        let replies = expire_runs(&mut Callers::new(), &mut running, &mut ended, now);
        assert_eq!(
            replies[0].session_note,
            SessionNote::None,
            "given once, then gone"
        );
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
            detached: false,
            caller: caller_key(2),
            due: None,
            cancelling: false,
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

#[cfg(test)]
mod cancel_tests {
    use super::*;

    fn run(call: u64) -> Running {
        Running {
            session: 1,
            token: 2,
            detached: false,
            call,
            job: call + 100,
            caller: caller_key(2),
            due: None,
            cancelling: false,
        }
    }

    #[test]
    fn a_cancel_with_no_connection_is_refused_and_the_run_is_not_marked_cancelling() {
        let mut clients: Callers = HashMap::new();
        let mut r = run(3);
        let why = begin_cancel(&mut clients, &mut r, Instant::now()).unwrap_err();
        assert!(why.contains("not known how it ended"), "{why}");
        assert!(!r.cancelling);
    }

    #[test]
    fn a_cancelling_run_is_left_open_until_its_bound() {
        let mut clients: Callers = HashMap::new();
        let now = Instant::now();
        let mut ended = Vec::new();
        let mut r = run(3);
        r.cancelling = true;
        r.due = Some(now + CANCEL_WAIT);
        let mut running = vec![r];
        assert!(expire_runs(&mut clients, &mut running, &mut ended, now).is_empty());
        assert_eq!(running.len(), 1);
    }

    #[test]
    fn a_cancelling_run_past_its_bound_ends_failed_and_never_as_cancelled() {
        let mut clients: Callers = HashMap::new();
        let now = Instant::now();
        let mut ended = Vec::new();
        let mut r = run(3);
        r.cancelling = true;
        r.due = Some(now - Duration::from_millis(1));
        let mut running = vec![r];
        let done = expire_runs(&mut clients, &mut running, &mut ended, now);
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].ending, Ending::FAILED);
        assert_ne!(done[0].ending.outcome, Outcome::Cancelled);
        assert!(
            done[0].detail.contains("not reported as cancelled"),
            "{}",
            done[0].detail
        );
        assert!(running.is_empty());
    }

    #[test]
    fn a_session_with_a_cancel_in_flight_keeps_its_connection_until_lemnosd_answers() {
        let mut r = run(3);
        r.cancelling = true;
        assert_eq!(departure(&[r], &caller_key(2)), Departure::Keep);
    }

    #[test]
    fn a_job_cancelled_by_lemnosd_is_cancelled_and_one_that_finished_is_its_result() {
        let r = run(3);
        assert_eq!(
            job_done(&r, Status::CANCELLED, &[]).ending.outcome,
            Outcome::Cancelled
        );
        assert_eq!(job_done(&r, Status::OK, b"os-3").ending, Ending::SUCCEEDED);
    }
}
