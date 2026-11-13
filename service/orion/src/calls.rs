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
//! connection stays for now, so the drivers keep what the caller holds. At Orion's pinned revision a
//! cancel does not say why it was sent. Orion sends the same `Cancel` when the caller stops the run,
//! when the caller's session ends, when the run's deadline passes and when the node stops, and a caller
//! that is still there cannot be told from one that is gone. So the connection is closed when the
//! caller's lease runs out (see [`expire`]): a caller silent for the lease is gone, and lemnosd then
//! tells the drivers so. Only silence expires a lease: a request, a transfer chunk or a run's progress
//! renews it (see [`expire`] for the list). The `ORION-R2 (C33)` notes mark where Orion's
//! reason replaces the lease.

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
    /// Stop run `call` of session `session`, if it is a long job still going.
    Cancel { session: u64, call: u64 },
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
    /// The caller the run came from: the key of its lemnosd connection.
    caller: String,
}

/// A caller's lemnosd connection, and when the caller last had a request: a run, a run's end or a
/// cancel. Its holds last for the lease after that, while no run of the caller is in progress.
struct Lemnosd {
    client: Client,
    seen: Instant,
}

impl Lemnosd {
    fn new(client: Client) -> Self {
        Self {
            client,
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
/// Each caller's connection is made when a run from it comes and none is up, so a call made just
/// after lemnosd comes back reaches it, rather than waiting out a retry delay from while it was
/// away. A connection is kept for as long as the caller's lease lasts ([`expire`]).
pub fn run(socket: &Path, messages: &Receiver<Message>, shared: &Shared, lease: Option<Duration>) {
    let mut clients: Callers = HashMap::new();
    let mut running: Vec<Running> = Vec::new();
    loop {
        expire(&mut clients, &mut running, lease, Instant::now(), shared);
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
                run_one(socket, &mut clients, &job, &mut running, shared);
            }
            Some(Message::Cancel { session, call }) => {
                cancel(&mut clients, &mut running, session, call, shared);
            }
            None if !running.is_empty() => follow(&mut clients, &mut running, shared),
            None => {}
        }
    }
}

/// The key a caller's lemnosd connection is kept under: the Orion caller's identity.
// ORION-R2 (C33): the key becomes the session token Orion gives each Claim (`Claim.session`), so a
// session's connection is kept apart from another session of the same identity. This alias is the
// one place the key type is named.
pub type CallerKey = String;

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
/// neither does a topic published to a caller. A caller with a run in flight is not dropped for silence
/// (see the rule below), and the run's own progress renews it.
///
/// A holder keeps its holds by renewing them: `<resource>.call describe` is the renewal with nothing
/// else to do.
// ORION-R2 (C33): the lease is removed at R2. Orion's CALLER_GONE, sent once for each session that
// ends, releases that session's connection at once, idle or busy; this function is then not called.
// The lease is kept only as the documented behaviour against an Orion older than R2.
fn expire(
    clients: &mut Callers,
    running: &mut Vec<Running>,
    lease: Option<Duration>,
    now: Instant,
    shared: &Shared,
) {
    let Some(lease) = lease else {
        return;
    };
    let transferred = shared.last_transfer();
    let ended: Vec<CallerKey> = clients
        .iter()
        .filter(|(caller, held)| {
            // ORION-R2 (C33): a caller with any run in flight is alive, whether or not the run reports
            // progress. The comm core cancels the runs of a session that ends (a reason-less Cancel for
            // a non-detached run), and a cancelled run no longer counts; a DETACHED run from a dead
            // caller is not cancelled and the bridge cannot see the flag, so it holds that caller's
            // holds until the run itself ends. The later protocol revision closes that.
            if running.iter().any(|r| r.caller == **caller) {
                return false;
            }
            let renewed = transferred.map_or(held.seen, |at| at.max(held.seen));
            now.saturating_duration_since(renewed) >= lease
        })
        .map(|(caller, _)| caller.clone())
        .collect();
    for caller in ended {
        ended_with(
            &caller,
            running,
            shared,
            "its lease ran out; the job ended with its connection",
        );
        release(clients, &caller, "its lease ran out", shared);
    }
}

// ORION-R2 (C33): the one trigger that releases a caller's holds. Dropping the connection closes it
// on lemnosd, which calls `caller_gone` on the drivers. At R2 the trigger is CALLER_GONE for the
// session token; the drop, the log and the count below stay.
fn release(clients: &mut Callers, caller: &str, why: &str, shared: &Shared) {
    if clients.remove(caller).is_some() {
        log(&format!(
            "caller of class {} dropped: {why}; its connection to lemnosd closed, and lemnosd released what it held (1 connection)",
            caller_class(caller)
        ));
        shared.with(|mirror| mirror.note_lease_end());
    }
}

/// Stops run `call` of `session`. A long job is cancelled on lemnosd, and the run ends as cancelled
/// at once: nothing else would answer it. The caller's connection is kept, and its lease starts
/// from now: a caller whose session ended is gone within a lease of its cancel.
// ORION-R2 (C33): Orion's Cancel gains `reason` (0 not said, 1 caller asked, 2 caller's session ended,
// 3 deadline, 4 node stopping). With reason 2 only, the caller's lemnosd connection is closed here
// after its job is cancelled (`clients.remove`), which is what tells lemnosd's drivers the caller
// is gone. Every other reason keeps the connection, as this does now.
fn cancel(
    clients: &mut Callers,
    running: &mut Vec<Running>,
    session: u64,
    call: u64,
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
        lemnosd.seen = Instant::now();
    }
    shared.finished(Done {
        session: r.session,
        call: r.call,
        outcome: Outcome::Cancelled,
        output: String::new(),
        detail: "cancelled".to_owned(),
    });
}

/// Connects the caller's connection to lemnosd, when it can be made.
fn connect(socket: &Path, clients: &mut Callers, caller: &str) {
    if let Ok(client) = Client::connect(socket, LEMNOSD_CALLS_NAME) {
        clients.insert(caller.to_owned(), Lemnosd::new(client));
    }
}

/// Runs one job: answered now, or followed as a long job.
fn run_one(
    socket: &Path,
    clients: &mut Callers,
    job: &Job,
    running: &mut Vec<Running>,
    shared: &Shared,
) {
    let caller = job.caller.as_str();
    if let Some(lemnosd) = clients.get_mut(caller) {
        lemnosd.seen = Instant::now();
    }
    if !clients.get(caller).is_some_and(|l| l.client.is_connected()) {
        // Jobs on the connection that went are over: lemnosd ended them with it.
        lost(caller, running, shared);
        clients.remove(caller);
        connect(socket, clients, caller);
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
            clients.insert(caller.to_owned(), Lemnosd::new(client));
        }
    }
    let (outcome, output, detail) = match served {
        Ok(Served::Started(started)) => {
            running.push(Running {
                session: job.session,
                call: job.call,
                job: started,
                caller: caller.to_owned(),
            });
            return;
        }
        Ok(Served::Output(output)) => (Outcome::Succeeded, output, String::new()),
        Err(Failure::Refused(why)) => (Outcome::Rejected, String::new(), why),
        Err(Failure::Failed(why)) => (Outcome::Failed, String::new(), why),
    };
    shared.finished(Done {
        session: job.session,
        call: job.call,
        outcome,
        output,
        detail,
    });
}

/// Takes the news of the long jobs on every caller's connection, waiting a little for it.
fn follow(clients: &mut Callers, running: &mut Vec<Running>, shared: &Shared) {
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
                shared.finished(job_done(&r, status, &result));
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
            outcome: Outcome::Failed,
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
        let text = crate::description::render(&description, generation, lease);
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
