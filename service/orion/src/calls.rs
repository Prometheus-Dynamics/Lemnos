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
//! Each Orion session has a lemnosd connection of its own, keyed by the session's token (see
//! [`CallerKey`]). lemnosd keeps the callers of a driver apart by connection, so a driver that tracks
//! callers sees each Orion session as its own, also when one identity runs two.
//!
//! A cancel stops the run and nothing else: the job is cancelled on lemnosd, and the caller's
//! connection stays, so the drivers keep what the caller holds. Orion says why a cancel was sent
//! ([`CancelReason`]). A cancel for a session that ended ([`CancelReason::CallerGone`]) does not renew
//! anything: the session's end is then said by [`Message::CallerGone`], which closes its connection
//! at once (see [`gone`]).
//!
//! The session's end releases its plain holds. Orion's `CallerGone`, sent once for each session that
//! ends for any cause, closes the session's connection: lemnosd releases what that connection held, and
//! tells the drivers the caller is gone. Orion also cancels each run of that session that is not
//! detached (`CancelReason::CallerGone`). A detached run belongs to the run, not to the session: its job
//! and its holds are on the run's own connection ([`CallerKey::Run`]), which stays until the job ends, is
//! cancelled, or reaches its deadline. A new session of the same identity finds such a run with Orion's
//! `attach` and reads its progress and result (see docs/daemon.md, "Detached runs").
//!
//! The timed lease ([`lapsed`]) remains only for a host that does not say which session asked (session
//! 0, an Orion older than the revision that names sessions): a silent caller of that kind is dropped
//! when its lease runs out. A session 0 caller's silence is renewed as documented in [`lapsed`].

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
use orion_msg_actions::CancelReason;

use crate::{LEMNOSD_CALLS_NAME, Shared, log};

/// The largest reply read, as `lemnos-ctl` reads it.
const REPLY_MAX: usize = 64 * 1024;
/// The word `call` answers with the device's description, as text. No device method takes it.
pub const DESCRIBE: &str = "describe";
/// The deadline a run passes when it is detached: Orion refuses a detached run without one (`NoDeadline`)
/// and one longer than the node takes (`actions.max_detached_ms`, an hour by default, so `DeadlineTooLong`).
/// The bridge's own callers (its tests) pass this one. It is a decision, not a measurement: thirty minutes
/// is under the default hour, and a longer job is asked with a longer deadline on a node that allows it.
pub const RUN_DEADLINE_MS: u64 = 30 * 60 * 1000;
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
    /// The bridge's own Orion session the run came on. An answer for a session that has ended is
    /// dropped.
    pub session: u64,
    /// The run, as Orion numbered it.
    pub call: u64,
    pub device: String,
    pub verb: Verb,
    pub args: String,
    /// Who asked for the run, as Orion names the caller (`local:<uid>`, `operator:<key>`,
    /// `peer:<node>`, `device:<name>` or `anonymous`). Empty when Orion did not say.
    pub caller: String,
    /// The token of the Orion session that asked, as Orion gives it: zero when Orion does not say.
    pub token: u64,
    /// The run's deadline in milliseconds, as the caller gave it to Orion; zero for none. It goes
    /// to lemnosd with a long job's start, where a driver that sizes its work (an install, a
    /// flash) refuses a start whose deadline is under its floor. Zero: no deadline, no floor.
    pub deadline_ms: u64,
}

/// What the publisher hands the call thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    /// Run this.
    Run(Job),
    /// Stop run `call` of session `session`, for `reason`, if it is a long job still going.
    Cancel {
        session: u64,
        call: u64,
        reason: CancelReason,
    },
    /// The Orion session with this token ended, for whatever cause: release what it held.
    CallerGone { token: u64 },
    /// The bridge's own Orion session, number `session`, was lost: the comm core went, or the node
    /// logic did. Orion ends every run that session was doing (as failed, "the program that runs it
    /// went away"), detached or not, and cannot say so to the bridge, so each of those runs' jobs is
    /// cancelled on lemnosd, with [`LOST_SESSION`] as the reason, and the device is free again.
    SessionLost { session: u64 },
}

/// Why a job is cancelled when the session that started its run is lost: said in the bridge's log, in the
/// run's detail, and kept by the mirror ([`crate::mirror::Mirror::last_cancel`]).
pub const LOST_SESSION: &str = "cancelled: the session that started this run was lost";

/// The key a caller's lemnosd connection is kept under.
///
/// A session that Orion names by its token is kept by that token: each session is a caller of its
/// own, also when one identity runs two. A session 0 (a host that does not say) has no token, so it is
/// kept by its identity, and only that kind is dropped for silence (see [`lapsed`]).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CallerKey {
    /// The Orion session's token, never zero.
    Session(u64),
    /// The identity of a session 0 caller, as Orion names it.
    Unsaid(String),
    /// A long job's own lemnosd connection, kept until the job ends, is cancelled, or passes its
    /// deadline. A job is owned by its run, not by its caller's session: when the session ends, a
    /// detached run carries on on this connection, and a run that is not detached is cancelled by Orion
    /// (`Cancel` with `CallerGone`), which closes it.
    Run { session: u64, call: u64 },
}

/// The key of the caller a run came from.
pub fn key_of(token: u64, caller: &str) -> CallerKey {
    if token == 0 {
        CallerKey::Unsaid(caller.to_owned())
    } else {
        CallerKey::Session(token)
    }
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
    call: u64,
    job: u64,
    /// The run's own lemnosd connection: the key its news and its cancel are found by.
    caller: CallerKey,
    /// The caller that asked (its session or identity): what keeps that caller alive while the run is
    /// in flight (see [`lapsed`]).
    owner: CallerKey,
}

/// The callers that have a run in flight, by who asked: a session 0 caller with a run going is alive.
fn in_flight(running: &[Running]) -> Vec<CallerKey> {
    running.iter().map(|r| r.owner.clone()).collect()
}

/// A caller's lemnosd connection, its class for the log, and when the caller last had a request: a
/// run, a run's end or a cancel. A session 0 caller's holds last for the lease after that, while no
/// run of the caller is in progress.
struct Lemnosd {
    client: Client,
    class: &'static str,
    seen: Instant,
}

impl Lemnosd {
    fn new(client: Client, class: &'static str) -> Self {
        Self {
            client,
            class,
            seen: Instant::now(),
        }
    }
}

/// The lemnosd connections the calls go on, by caller.
type Callers = HashMap<CallerKey, Lemnosd>;

/// How long the call thread waits for a job's news before it looks for new runs again.
const JOB_WAIT: Duration = Duration::from_millis(50);
/// How often the call thread looks for a caller whose lease has run out, when no run is going.
const LEASE_TICK: Duration = Duration::from_millis(200);

/// How a run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Done {
    pub session: u64,
    pub call: u64,
    pub outcome: Outcome,
    /// The reply as text, for `call`.
    pub output: String,
    /// Why, when it did not succeed.
    pub detail: String,
}

/// Runs what the publisher hands over until it stops sending.
///
/// Each session's connection is made when a run from it comes and none is up, so a call made just
/// after lemnosd comes back reaches it, rather than waiting out a retry delay from while it was
/// away. A connection is kept until its session ends ([`gone`]), or, for a session 0 caller, until
/// its lease runs out ([`lapsed`]).
pub fn run(socket: &Path, messages: &Receiver<Message>, shared: &Shared, lease: Option<Duration>) {
    let mut clients: Callers = HashMap::new();
    let mut running: Vec<Running> = Vec::new();
    let mut ended: Vec<CallerKey> = Vec::new();
    loop {
        expire(
            &mut clients,
            &running,
            &mut ended,
            lease,
            Instant::now(),
            shared,
        );
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
            Some(Message::CallerGone { token }) => {
                gone(&mut clients, &mut ended, token, shared);
            }
            Some(Message::SessionLost { session }) => {
                while let Some(call) = running
                    .iter()
                    .find(|r| r.session == session)
                    .map(|r| r.call)
                {
                    cancel(
                        &mut clients,
                        &mut running,
                        &mut ended,
                        session,
                        call,
                        CancelReason::CallerGone,
                        shared,
                    );
                }
            }
            None if !running.is_empty() => follow(&mut clients, &mut running, &mut ended, shared),
            None => {}
        }
    }
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

/// Whether a cancel renews its caller's lease. Every reason does but a session's end: a session that
/// ended is not alive, and its cancel is not a sign of life.
pub fn renews(reason: CancelReason) -> bool {
    reason != CancelReason::CallerGone
}

/// The callers whose lease has run out, of the callers that are kept by their identity only (session
/// 0). `held` is each kept caller with when it last had a request; `in_flight` the callers with a run
/// going; `transferred` when the bridge last accepted a file or stream chunk.
///
/// A session 0 caller is alive while it has a run in flight, whether or not the run reports progress.
/// Otherwise its last renewal is the latest of:
///
/// - a request of it: a `call`, `set` or `power` run, or a `cancel` of its run that renews (see
///   [`renews`]);
/// - the end of one of its runs (its job's last news);
/// - a progress report of a long job it started (`Progress` news of its run);
/// - a file or stream chunk the bridge accepted. A host that does not say which session sent a
///   chunk gets every session 0 caller's lease renewed, as it always was: Orion's file events do not
///   name the sender at the revision this bridge is pinned to.
///
/// The bridge's own housekeeping (its publishing, its hello document, its ticks) renews nothing, and
/// neither does a topic published to a caller. A holder keeps its holds by renewing them:
/// `<resource>.call describe` is the renewal with nothing else to do.
///
/// A caller with a token is never dropped for silence: its session's end releases it ([`gone`]).
pub fn lapsed(
    held: &[(CallerKey, Instant)],
    in_flight: &[CallerKey],
    transferred: Option<Instant>,
    now: Instant,
    lease: Duration,
) -> Vec<CallerKey> {
    held.iter()
        .filter(|(key, _)| matches!(key, CallerKey::Unsaid(_)))
        .filter(|(key, _)| !in_flight.contains(key))
        .filter(|(_, seen)| {
            let renewed = transferred.map_or(*seen, |at| at.max(*seen));
            now.saturating_duration_since(renewed) >= lease
        })
        .map(|(key, _)| key.clone())
        .collect()
}

/// Drops the connection of each session 0 caller whose lease has run out, when the lease is on. Each
/// one is released through [`release`]. A caller with a run in flight is not lapsed ([`lapsed`]), so no
/// run of its is cut short here.
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
    let held: Vec<(CallerKey, Instant)> = clients
        .iter()
        .map(|(key, lemnosd)| (key.clone(), lemnosd.seen))
        .collect();
    for caller in lapsed(
        &held,
        &in_flight(running),
        shared.last_transfer(),
        now,
        lease,
    ) {
        release(clients, ended, &caller, "its lease ran out", shared);
    }
}

/// The most callers whose lease ended and who have had no reply since. The oldest is forgotten
/// first, so a caller that comes back much later sees no note.
pub const ENDED_MAX: usize = 64;

/// The note a caller's first reply after its lease ended carries, at the start of its `detail`. A
/// client matches the prefix `lease-ended:`; the rest of the detail is free text. Only a session 0
/// caller's lease ends this way: a session that ends is not sent a reply.
pub const LEASE_ENDED_NOTE: &str = "lease-ended: holds released";

/// Remembers that `caller`'s lease ended, so its next reply carries [`LEASE_ENDED_NOTE`].
fn remember(ended: &mut Vec<CallerKey>, caller: &CallerKey) {
    if ended.contains(caller) {
        return;
    }
    if ended.len() == ENDED_MAX {
        ended.remove(0);
    }
    ended.push(caller.clone());
}

/// `detail` with the note in front when `caller`'s lease ended and it has had no reply since; the
/// note is given once.
fn noted(ended: &mut Vec<CallerKey>, caller: &CallerKey, detail: String) -> String {
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

/// The one trigger that releases a caller's holds: dropping its connection closes it on lemnosd,
/// which calls `caller_gone` on the drivers. It is the session's end ([`gone`]) or a session 0
/// caller's lease running out ([`expire`]).
fn release(
    clients: &mut Callers,
    ended: &mut Vec<CallerKey>,
    caller: &CallerKey,
    why: &str,
    shared: &Shared,
) {
    let Some(lemnosd) = clients.remove(caller) else {
        return;
    };
    if matches!(caller, CallerKey::Unsaid(_)) {
        remember(ended, caller);
    }
    log(&format!(
        "caller of class {} dropped: {why}; its connection to lemnosd closed, and lemnosd released what it held (1 connection)",
        lemnosd.class
    ));
    shared.with(|mirror| mirror.note_lease_end());
}

/// The Orion session with `token` ended, for whatever cause. Its connection is closed at once, which
/// releases the plain holds it made: nothing is renewed, since the session cannot ask again. The runs
/// are not touched here. A run that is not detached is cancelled by Orion (`CancelReason::CallerGone`),
/// and a detached run keeps its own connection and its job until they end. (When the bridge's own
/// session is lost, every run of it is cancelled: [`Message::SessionLost`].)
fn gone(clients: &mut Callers, ended: &mut Vec<CallerKey>, token: u64, shared: &Shared) {
    release(
        clients,
        ended,
        &CallerKey::Session(token),
        "its session ended",
        shared,
    );
}

/// Stops run `call` of `session`, for `reason`. A long job is cancelled on lemnosd, and the run ends
/// as cancelled at once: nothing else would answer it. The caller's connection is kept, and a cancel
/// that renews ([`renews`]) starts the lease of a session 0 caller from now.
fn cancel(
    clients: &mut Callers,
    running: &mut Vec<Running>,
    ended: &mut Vec<CallerKey>,
    session: u64,
    call: u64,
    reason: CancelReason,
    shared: &Shared,
) {
    let Some(at) = running
        .iter()
        .position(|r| r.session == session && r.call == call)
    else {
        return;
    };
    let r = running.remove(at);
    if let Some(lemnosd) = clients.get_mut(&r.caller) {
        // Its end, cancelled or not, comes as the job's news, which finds no run and is dropped.
        let _ = lemnosd.client.cancel_job(r.job);
        if renews(reason) {
            lemnosd.seen = Instant::now();
        }
    }
    // A job's own connection ends with it: its holds are released now.
    clients.remove(&r.caller);
    // A cancel for a session that is gone says why in the run's detail and in the log; any other cancel
    // is the caller's own and needs no reason.
    let why = if reason == CancelReason::CallerGone {
        log(&format!(
            "run {} of session {}, job {}: {LOST_SESSION}",
            r.call, r.session, r.job
        ));
        shared.with(|mirror| mirror.note_cancel(LOST_SESSION));
        LOST_SESSION
    } else {
        "cancelled"
    };
    let detail = noted(ended, &r.owner, why.to_owned());
    shared.finished(Done {
        session: r.session,
        call: r.call,
        outcome: Outcome::Cancelled,
        output: String::new(),
        detail,
    });
}

/// Connects the caller's connection to lemnosd, when it can be made.
fn connect(socket: &Path, clients: &mut Callers, caller: &CallerKey, class: &'static str) {
    if let Ok(client) = Client::connect(socket, LEMNOSD_CALLS_NAME) {
        clients.insert(caller.clone(), Lemnosd::new(client, class));
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
    let caller = key_of(job.token, &job.caller);
    let class = caller_class(&job.caller);
    let mut run_client = None;
    if let Some(lemnosd) = clients.get_mut(&caller) {
        lemnosd.seen = Instant::now();
    }
    if !clients
        .get(&caller)
        .is_some_and(|l| l.client.is_connected())
    {
        // A session's connection holds plain holds only: a run's job is on the run's own connection.
        lost(&caller, running, shared);
        clients.remove(&caller);
        connect(socket, clients, &caller, class);
    }
    let connect_run = || Client::connect(socket, LEMNOSD_CALLS_NAME).map_err(failed);
    let mut served = match clients.get(&caller) {
        Some(lemnosd) => serve(&lemnosd.client, job, shared, &connect_run, &mut run_client),
        None => Err(Failure::Failed("lemnosd is not there".to_owned())),
    };
    // A connection that went quiet with lemnosd is found dead only when used: connect
    // again and run the job once more: the call never reached the lemnosd that is there now.
    if served.is_err()
        && clients
            .get(&caller)
            .is_some_and(|l| !l.client.is_connected())
    {
        lost(&caller, running, shared);
        clients.remove(&caller);
        if let Ok(client) = Client::connect(socket, LEMNOSD_CALLS_NAME) {
            served = serve(&client, job, shared, &connect_run, &mut run_client);
            clients.insert(caller.clone(), Lemnosd::new(client, class));
        }
    }
    let (outcome, output, detail) = match served {
        Ok(Served::Started(started)) => {
            // The job's own connection: its holds and its news are that run's, not the session's.
            let run = CallerKey::Run {
                session: job.session,
                call: job.call,
            };
            if let Some(client) = run_client.take() {
                clients.insert(run.clone(), Lemnosd::new(client, class));
            }
            running.push(Running {
                session: job.session,
                call: job.call,
                job: started,
                caller: run,
                owner: caller.clone(),
            });
            return;
        }
        Ok(Served::Output(output)) => (Outcome::Succeeded, output, String::new()),
        Err(Failure::Refused(why)) => (Outcome::Rejected, String::new(), why),
        Err(Failure::Failed(why)) => (Outcome::Failed, String::new(), why),
    };
    let detail = noted(ended, &caller, detail);
    shared.finished(Done {
        session: job.session,
        call: job.call,
        outcome,
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
                // A job's own connection ends with the job: its holds are released.
                clients.remove(&caller);
                let mut done = job_done(&r, status, &result);
                done.detail = noted(ended, &r.owner, done.detail);
                shared.finished(done);
            }
        }
    }
}

/// How a run that was a long job ended.
fn job_done(run: &Running, status: Status, result: &[u8]) -> Done {
    let (outcome, output, detail) = if status.is_ok() {
        (Outcome::Succeeded, result_text(result), String::new())
    } else if status == Status::CANCELLED {
        (Outcome::Cancelled, String::new(), "cancelled".to_owned())
    } else {
        (
            Outcome::Failed,
            String::new(),
            format!("the job ended with status {}", status.0),
        )
    };
    Done {
        session: run.session,
        call: run.call,
        outcome,
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

/// Ends the job on a run's connection as failed: that connection went, and so did its job.
fn lost(caller: &CallerKey, running: &mut Vec<Running>, shared: &Shared) {
    ended_with(
        caller,
        running,
        shared,
        "lemnosd went away; the job ended with it",
    );
}

/// Ends the followed jobs on one connection as failed, with `detail` as the reason. Matched by the
/// run's own connection (`CallerKey::Run`): a session's connection carries no job.
fn ended_with(caller: &CallerKey, running: &mut Vec<Running>, shared: &Shared, detail: &str) {
    let mut kept = Vec::with_capacity(running.len());
    for r in running.drain(..) {
        if r.caller != *caller {
            kept.push(r);
            continue;
        }
        shared.finished(Done {
            session: r.session,
            call: r.call,
            outcome: Outcome::Failed,
            output: String::new(),
            detail: detail.to_owned(),
        });
    }
    *running = kept;
}

/// Ends every followed job as failed, when there is no connection at all.
fn lost_all(running: &mut Vec<Running>, shared: &Shared) {
    let callers: Vec<CallerKey> = running.iter().map(|r| r.caller.clone()).collect();
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
    /// It ran, or tried to, and failed: the device said no, or lemnosd is not there.
    Failed(String),
}

fn failed(error: Error) -> Failure {
    Failure::Failed(error.to_string())
}

/// A call's failure. A reply that did not fit the out buffer is named as such, with how to ask
/// for more; the bridge never sends a partial reply.
fn refused_reply(error: Error, call: &Call) -> Failure {
    match &error {
        Error::Refused { status, .. } if *status == Status::BUFFER_TOO_SMALL => {
            Failure::Failed(format!("{error}: {}", too_small(call)))
        }
        _ => failed(error),
    }
}

/// Does one job: its reply as text, or the long job it started.
///
/// # Errors
///
/// Why it did not succeed.
pub fn serve(
    client: &Client,
    job: &Job,
    shared: &Shared,
    connect_run: &dyn Fn() -> Result<Client, Failure>,
    run_out: &mut Option<Client>,
) -> Result<Served, Failure> {
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
    if method.job {
        // A long job runs on a connection of its own, kept by the run until the job ends.
        let run = connect_run()?;
        let mut handle = run.open_tag(&device.name, tag).map_err(failed)?;
        // A lemnosd from before the deadline (it says so at connect) is started without one.
        let started = if job.deadline_ms == 0 || !run.takes_job_deadline() {
            run.start_job(&mut handle, method.id, &call.args)
        } else {
            run.start_job_within(&mut handle, method.id, &call.args, job.deadline_ms)
        }
        .map_err(failed)?;
        *run_out = Some(run);
        return Ok(Served::Started(started));
    }
    let mut handle = client.open_tag(&device.name, tag).map_err(failed)?;
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

    #[test]
    fn a_session_is_kept_by_its_token_and_a_session_0_caller_by_its_identity() {
        // Two sessions of one identity are two callers: the token tells them apart.
        assert_eq!(key_of(7, "local:1000"), CallerKey::Session(7));
        assert_eq!(key_of(7, "local:1000"), key_of(7, "local:2000"));
        assert_ne!(key_of(7, "local:1000"), key_of(8, "local:1000"));
        // A host that does not say (token 0) is kept by its identity, as before.
        assert_eq!(
            key_of(0, "local:1000"),
            CallerKey::Unsaid("local:1000".to_owned())
        );
        assert_ne!(key_of(0, "local:1000"), key_of(0, "local:2000"));
    }

    #[test]
    fn a_cancel_for_a_session_that_ended_renews_nothing_and_the_others_renew() {
        assert!(!renews(CancelReason::CallerGone));
        for reason in [
            CancelReason::Unsaid,
            CancelReason::Asked,
            CancelReason::Deadline,
            CancelReason::Stopping,
        ] {
            assert!(renews(reason), "{reason:?} renews");
        }
    }

    /// The lease, for a test: five seconds.
    const LEASE: Duration = Duration::from_secs(5);

    #[test]
    fn only_a_session_0_caller_is_ever_dropped_for_silence() {
        let t0 = Instant::now();
        let held = [
            (CallerKey::Session(7), t0),
            (CallerKey::Unsaid("local:1000".to_owned()), t0),
        ];
        let later = t0 + LEASE * 10;
        assert_eq!(
            lapsed(&held, &[], None, later, LEASE),
            [CallerKey::Unsaid("local:1000".to_owned())],
            "a session is released by its CALLER_GONE, never by silence"
        );
        assert!(
            lapsed(&held, &[], None, t0 + LEASE / 2, LEASE).is_empty(),
            "a silent caller is kept until its lease has run out"
        );
    }

    #[test]
    fn a_run_in_flight_is_its_askers_and_not_its_own_connection() {
        // A run keeps its own connection (`Run`), so the asker is found by `owner`: a session 0 caller
        // with a run going must be shielded from its lease, whatever connection the run is on.
        let asker = CallerKey::Unsaid("local:1000".to_owned());
        let run = Running {
            session: 1,
            call: 2,
            job: 3,
            caller: CallerKey::Run {
                session: 1,
                call: 2,
            },
            owner: asker.clone(),
        };
        assert_eq!(
            in_flight(std::slice::from_ref(&run)),
            std::slice::from_ref(&asker).to_vec()
        );
        let t0 = Instant::now();
        let held = [(asker.clone(), t0)];
        assert!(
            lapsed(&held, &in_flight(&[run]), None, t0 + LEASE * 3, LEASE).is_empty(),
            "a run in flight shields the session 0 caller that asked, not only a run keyed by it"
        );
    }

    #[test]
    fn a_session_0_caller_with_a_run_in_flight_is_alive_and_a_chunk_renews_it() {
        let t0 = Instant::now();
        let held = [(CallerKey::Unsaid("local:1000".to_owned()), t0)];
        let later = t0 + LEASE * 3;
        let running = [CallerKey::Unsaid("local:1000".to_owned())];
        assert!(
            lapsed(&held, &running, None, later, LEASE).is_empty(),
            "a run in flight shields a session 0 caller"
        );
        assert!(
            lapsed(&held, &[], Some(later), later, LEASE).is_empty(),
            "an accepted chunk renews every session 0 caller's lease"
        );
        assert_eq!(lapsed(&held, &[], None, later, LEASE).len(), 1);
    }
}
