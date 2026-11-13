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

use std::path::Path;
use std::sync::mpsc::Receiver;

use lemnos_client::{Client, Error};
use lemnos_core::ItemKind;
use lemnos_wire::OwnedShape;
use lemnos_wire::text::{encode_args, find_item, find_method, method_of, render_reply};
use orion::actions::Outcome;

use crate::{LEMNOSD_CALLS_NAME, Shared};

/// The largest reply read, as `lemnos-ctl` reads it.
const REPLY_MAX: usize = 64 * 1024;

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

/// Runs jobs until the sender is dropped.
///
/// The connection is made when a job comes and none is up, so a call made just after lemnosd
/// comes back reaches it, rather than waiting out a retry delay from while it was away.
pub fn run(socket: &Path, jobs: &Receiver<Job>, shared: &Shared) {
    let mut client: Option<Client> = None;
    while let Ok(job) = jobs.recv() {
        if !client.as_ref().is_some_and(Client::is_connected) {
            client = Client::connect(socket, LEMNOSD_CALLS_NAME).ok();
        }
        let mut served = match &client {
            Some(client) => serve(client, &job),
            None => Err(Failure::Failed("lemnosd is not there".to_owned())),
        };
        // A connection that went quiet with lemnosd is found dead only when used: connect
        // again and run the job once more: the call never reached the lemnosd that is there now.
        if served.is_err() && client.as_ref().is_some_and(|c| !c.is_connected()) {
            client = Client::connect(socket, LEMNOSD_CALLS_NAME).ok();
            if let Some(client) = &client {
                served = serve(client, &job);
            }
        }
        let (outcome, output, detail) = match served {
            Ok(output) => (Outcome::Succeeded, output, String::new()),
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

/// Does one job. The output is the reply as text, or empty.
///
/// # Errors
///
/// Why it did not succeed.
pub fn serve(client: &Client, job: &Job) -> Result<String, Failure> {
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
        return Ok(String::new());
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
    let description = client.describe(device.index).map_err(failed)?;
    let (contract, method) = if job.verb == Verb::Set {
        let item = find_item(&description, wanted).map_err(Failure::Refused)?;
        if item.kind == ItemKind::Value {
            return Err(Failure::Refused(format!("{wanted} is read-only")));
        }
        method_of(&description, item).map_err(Failure::Refused)?
    } else {
        find_method(&description, wanted).map_err(Failure::Refused)?
    };
    if method.job {
        // A long job's end comes later, on the connection that started it, and some jobs wait
        // for more calls from that connection: a client runs them, not an action.
        return Err(Failure::Refused(format!(
            "{wanted} is a long job: a lemnos client starts it (start_job) and follows it"
        )));
    }
    let args = encode_args(&method.params, values).map_err(Failure::Refused)?;
    let tag = &description.contracts[contract].tag;
    let mut handle = client.open_tag(&device.name, tag).map_err(failed)?;
    let mut reply = vec![0; REPLY_MAX];
    let len = client
        .call(&mut handle, method.id, &args, &mut reply)
        .map_err(failed)?;
    if job.verb == Verb::Set || method.returns == OwnedShape::Unit {
        return Ok(String::new());
    }
    render_reply(method, &reply[..len]).map_err(Failure::Failed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_verb_has_its_own_name() {
        let names: Vec<&str> = Verb::ALL.iter().map(|verb| verb.name()).collect();
        assert_eq!(names, ["set", "call", "power"]);
    }
}
