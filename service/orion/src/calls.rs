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

use std::path::Path;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Duration;

use lemnos_client::{Client, Error, JobEvent};
use lemnos_core::{ItemKind, Status};
use lemnos_wire::OwnedShape;
use lemnos_wire::text::{
    Call, encode_call_within, find_item, find_method, method_of, render_reply, too_small,
};
use orion::actions::Outcome;

use crate::{LEMNOSD_CALLS_NAME, Shared};

/// The largest reply read, as `lemnos-ctl` reads it.
const REPLY_MAX: usize = 64 * 1024;
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
                "Calls any method of the device. Arguments: METHOD ARG..., the method as NAME or TAG:NAME, as lemnos-ctl call takes them. The output is the reply as text"
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Running {
    session: u64,
    call: u64,
    job: u64,
}

/// How long the call thread waits for a job's news before it looks for new runs again.
const JOB_WAIT: Duration = Duration::from_millis(50);

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
/// The connection is made when a run comes and none is up, so a call made just after lemnosd
/// comes back reaches it, rather than waiting out a retry delay from while it was away.
pub fn run(socket: &Path, messages: &Receiver<Message>, shared: &Shared) {
    let mut client: Option<Client> = None;
    let mut running: Vec<Running> = Vec::new();
    loop {
        let message = if running.is_empty() {
            match messages.recv() {
                Ok(message) => Some(message),
                Err(_) => return,
            }
        } else {
            match messages.try_recv() {
                Ok(message) => Some(message),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => return,
            }
        };
        match message {
            Some(Message::Run(job)) => run_one(socket, &mut client, &job, &mut running, shared),
            Some(Message::Cancel { session, call }) => {
                let found = running
                    .iter()
                    .find(|r| r.session == session && r.call == call);
                if let (Some(found), Some(client)) = (found, &client) {
                    // Its end, cancelled or not, comes as the job's news.
                    let _ = client.cancel_job(found.job);
                }
            }
            None => follow(client.as_ref(), &mut running, shared),
        }
    }
}

/// Runs one job: answered now, or followed as a long job.
fn run_one(
    socket: &Path,
    client: &mut Option<Client>,
    job: &Job,
    running: &mut Vec<Running>,
    shared: &Shared,
) {
    if !client.as_ref().is_some_and(Client::is_connected) {
        // Jobs on the connection that went are over: lemnosd ended them with it.
        lost(running, shared);
        *client = Client::connect(socket, LEMNOSD_CALLS_NAME).ok();
    }
    let mut served = match client.as_ref() {
        Some(client) => serve(client, job),
        None => Err(Failure::Failed("lemnosd is not there".to_owned())),
    };
    // A connection that went quiet with lemnosd is found dead only when used: connect
    // again and run the job once more: the call never reached the lemnosd that is there now.
    if served.is_err() && client.as_ref().is_some_and(|c| !c.is_connected()) {
        lost(running, shared);
        *client = Client::connect(socket, LEMNOSD_CALLS_NAME).ok();
        if let Some(client) = client.as_ref() {
            served = serve(client, job);
        }
    }
    let (outcome, output, detail) = match served {
        Ok(Served::Started(started)) => {
            running.push(Running {
                session: job.session,
                call: job.call,
                job: started,
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

/// Takes the news of the long jobs, waiting a little for it.
fn follow(client: Option<&Client>, running: &mut Vec<Running>, shared: &Shared) {
    let Some(client) = client else {
        lost(running, shared);
        return;
    };
    let Some(event) = client.wait_job_event(JOB_WAIT) else {
        if !client.is_connected() {
            lost(running, shared);
        }
        return;
    };
    match event {
        JobEvent::Progress { job, done, total } => {
            if let Some(r) = running.iter().find(|r| r.job == job) {
                shared.progressed(Progress {
                    session: r.session,
                    call: r.call,
                    done,
                    total,
                });
            }
        }
        JobEvent::Done {
            job,
            status,
            result,
        } => {
            let Some(at) = running.iter().position(|r| r.job == job) else {
                return;
            };
            let r = running.remove(at);
            shared.finished(job_done(&r, status, &result));
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

/// Ends every followed job as failed: lemnosd went, and its jobs with it.
fn lost(running: &mut Vec<Running>, shared: &Shared) {
    for r in running.drain(..) {
        shared.finished(Done {
            session: r.session,
            call: r.call,
            outcome: Outcome::Failed,
            output: String::new(),
            detail: "lemnosd went away; the job ended with it".to_owned(),
        });
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
pub fn serve(client: &Client, job: &Job) -> Result<Served, Failure> {
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
