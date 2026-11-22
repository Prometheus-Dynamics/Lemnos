//! The Orion side: one session on the comm core, owned by this thread.
//!
//! Each device is a resource named `lemnos.<device>`, of the kind `lemnos.device`:
//!
//! - its labels are `lemnos.driver`, one `lemnos.tag` per tag, and `lemnos.control.<item>` for
//!   each control or action item, with the shapes it takes;
//! - its state is `Ready` while it runs, `Failed` while it retries or reports failed health,
//!   and `Pending` while it is switched off;
//! - the topic `lemnos.<device>.values` has a field per value item that is a number (a float,
//!   in the item's unit, scaled by its exponent) or a flag (a bool), published as it changes,
//!   each sample at the time the driver read it;
//! - the topic `lemnos.<device>.status` has the fields `state`, `health`, `reason` and `refusal`, as
//!   `lemnos-ctl list` names them, published as they change. `refusal` is the number the wire gives
//!   the refusal kind in decimal: 0 none, 1 absent, 2 failed, and any other number as it came;
//! - the actions `set`, `call` and `power` (see [`crate::calls`]) drive it.
//!
//! The board's facts (`hello.<fact>`, see [`crate::hello`]) are topics of the same session, one
//! each, with a text field `value`.
//!
//! The session is one for as long as Orion keeps it, and every change is said on its own:
//!
//! - a device that comes is described once: its resource, topics and actions, and its state;
//! - a device that goes has its status said `gone` and its resource withdrawn, once. Its
//!   topics are withdrawn with it, so their subscribers hear the publisher is gone;
//! - a fact that comes is declared once, and one that goes is withdrawn once;
//! - a value or a state that moves is published as it moves.
//!
//! Nothing else on the session is said again. When Orion goes away and comes back, a new session
//! says every topic, action and resource again, and the values and states are sent again. The
//! bridge opens that session itself rather than with `Orion::reconnect`: that restores resource
//! records before their actions, so a run in between finds a resource with no action (NotFound).
//! Here each device's actions are registered before its resource is published, so a resource is
//! never listed without its actions.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use orion::actions::{ActionDecl, Admits, Policy, Registration, Request, Target};
use orion::provider::{Provider, Resource, ResourceState};
use orion::topics::{
    Field, FieldId, FieldType, OnSubscribe, Publisher, SampleTime, TopicDecl, TopicError,
};
use orion::types::Error as Refusal;
use orion::{ClientError, Error, Options, Orion, Roles};
use orion_connect::Feature;
use orion_msg_session::RejectReason;
use rustix::event::{PollFd, PollFlags, Timespec, poll};

use lemnos_client::{DeviceHealth, DeviceState};

use crate::backoff::Backoff;
use crate::calls::{Done, Ending, Job, Message, Progress, Verb};
use crate::config::Config;
use crate::hello;
use crate::mirror::{Changes, DeviceSpec, Status, health_name, state_name};
use crate::values::{Plain, Reading};
use crate::{CLIENT_NAME, RESOURCE_KIND, RESOURCE_PREFIX, Shared, log};

/// Who may run a device's actions: programs on this board, and operators granted it.
const POLICY: Policy = Policy::new(Admits::LOCAL.and(Admits::OPERATOR));
/// The session's anchor action: registered once, and never run (nobody is admitted). Its
/// registration is where every run of this session's actions arrives, so the queue is polled
/// whichever devices are published, and a run for a device that has gone is answered.
const ANCHOR: &str = "lemnos-orion.requests";
const ANCHOR_POLICY: Policy = Policy::new(Admits::NOBODY);
/// How long node logic keeps a detached run of ours for us while our session is lost, for a kept
/// action (`action_kept`). The old session's names are held for its idle time and a heartbeat (6 s
/// and 2 s by default), and this bridge's retries of those names come at most every idle/4 (1.5 s),
/// so 30 s is that with room to spare. Node logic caps a window at a minute.
const KEPT: Duration = Duration::from_secs(30);
/// Slack past `KEPT` before the bridge lets go of a run node logic has not given back. The bridge's
/// window is never shorter than node logic's, so a run it still holds is one node logic may return.
const KEPT_SLACK: Duration = Duration::from_secs(10);
/// Samples of each field a topic keeps.
const LATEST: u32 = 2;
/// The longest status text published. A longer reason is cut at a character boundary.
const STATUS_MAX: usize = 256;
/// How long to wait with no devices and no facts for lemnosd to have some.
const IDLE: Duration = Duration::from_secs(1);
/// How soon a resource state Orion refused is written again.
const RETRY: Duration = Duration::from_secs(1);
/// The refusals an opening has said. The same refusal again is not said at every retry: it is said
/// once, and again only every [`SAID_EVERY`] with how many times it came since.
#[derive(Default)]
struct Said {
    last: Option<(String, Instant, u64)>,
}

/// How long an unchanged refusal is left unsaid before it is said again, with its count.
const SAID_EVERY: Duration = Duration::from_secs(60);

impl Said {
    /// The line to say for `message` now, if it is to be said: the first time, and then again every
    /// [`SAID_EVERY`] with the count of retries since.
    fn line(&mut self, message: &str, now: Instant) -> Option<String> {
        match &mut self.last {
            Some((last, at, repeats)) if last == message => {
                *repeats += 1;
                if now.duration_since(*at) < SAID_EVERY {
                    return None;
                }
                let line = format!("{message} (again: {repeats} retries since it was first said)");
                *at = now;
                *repeats = 0;
                Some(line)
            }
            _ => {
                self.last = Some((message.to_owned(), now, 0));
                Some(message.to_owned())
            }
        }
    }

    fn say(&mut self, message: &str) {
        if let Some(line) = self.line(message, Instant::now()) {
            log(&line);
        }
    }
}

#[cfg(test)]
mod said_tests {
    use super::*;

    /// The refusal of a retry is said once, not at every retry; it is said again only after
    /// [`SAID_EVERY`], with how many retries came since. A different refusal is said at once.
    #[test]
    fn a_repeated_refusal_is_said_once_then_once_a_minute() {
        let mut said = Said::default();
        let start = Instant::now();
        assert!(said.line("refused: not supported", start).is_some());
        for step in 1..=7 {
            assert_eq!(
                said.line("refused: not supported", start + Duration::from_secs(step)),
                None
            );
        }
        let again = said
            .line("refused: not supported", start + SAID_EVERY)
            .expect("said again after a minute");
        assert!(again.contains("(again: 8 retries"), "{again}");
        assert!(said.line("another refusal", start + SAID_EVERY).is_some());
    }
}

/// How soon a session is opened again after the comm core went away, before the retry delays of
/// a session Orion refuses. The comm core is usually back within this.
const RECONNECT: Duration = Duration::from_millis(100);

/// The state a status topic says when its device is withdrawn.
const GONE: &str = "gone";
/// Why a status topic says its device is gone, when lemnosd no longer lists it.
const DEVICE_GONE: &str = "the device is no longer listed by lemnosd";

/// How a topic's samples are read. A **state** is a value that is current until it changes: its
/// newest sample is what matters. An **event** is a sample each of which matters. Every topic the
/// bridge publishes is a state today.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    State,
    Event,
}

/// What the bridge says of one of its topics: its name (a device's topics are named within its
/// resource, a fact's with `hello.`), its kind, and `expiry_ms`, how long a sample stays valid
/// after it is sent: zero for always, that is until the bridge changes it or withdraws the topic.
/// Orion's device topics are declared with an expiry of zero whatever is given, so the device
/// classes say zero: a device's state is served as current until the bridge publishes a change
/// or withdraws the device's topics. The table is documented in `docs/daemon.md`.
#[derive(Clone, Copy, Debug)]
pub struct Class {
    pub name: &'static str,
    pub kind: Kind,
    pub expiry_ms: u32,
    /// Samples Orion keeps per field: 2 for a latest-value topic (a state), which is all a state
    /// needs, since the newest sample is the value.
    pub history: u32,
}

/// A device's readings: the last reading is the value, published as the driver reads a change.
pub const VALUES: Class = Class {
    name: "values",
    kind: Kind::State,
    expiry_ms: 0,
    history: LATEST,
};
/// A device's state, health and reason: current until it changes; the bridge says `gone` when
/// lemnosd stops listing the device.
pub const STATUS: Class = Class {
    name: "status",
    kind: Kind::State,
    expiry_ms: 0,
    history: LATEST,
};
/// A device's description generation: a counter that moves each time what `describe` says of the
/// device changes (its state or reason). A client that sees it move asks `describe` again.
pub const DESCRIPTION: Class = Class {
    name: "description",
    kind: Kind::State,
    expiry_ms: 0,
    history: LATEST,
};
/// The device set's generation: a counter that moves when a device comes or goes.
pub const DEVICE_SET: Class = Class {
    name: "lemnos.devices",
    kind: Kind::State,
    expiry_ms: 0,
    history: LATEST,
};
/// A board fact (`hello.<fact>`): current until the fact changes or the session ends.
pub const FACT: Class = Class {
    name: "hello.<fact>",
    kind: Kind::State,
    expiry_ms: 0,
    history: LATEST,
};
/// Every topic the bridge publishes, and how it is classified.
pub const CLASSES: [Class; 5] = [VALUES, STATUS, DESCRIPTION, DEVICE_SET, FACT];

/// What a late subscriber gets, by the kind of topic: a state topic (a device's current state,
/// config, presence) gives its last value, so a subscriber that arrives later is correct at once;
/// an event or stream topic does not replay, and a subscriber gets what is published from then on.
/// This is the one rule; every declaration takes its mode from it.
pub const fn on_subscribe_for(kind: Kind) -> OnSubscribe {
    match kind {
        Kind::State => OnSubscribe::Latest,
        Kind::Event => OnSubscribe::FromNow,
    }
}

impl Class {
    /// The mode this class's topics are declared with ([`on_subscribe_for`]).
    pub const fn on_subscribe(&self) -> OnSubscribe {
        on_subscribe_for(self.kind)
    }
}

/// A generation counter's one field.
const GENERATION_FIELDS: [Field<'static>; 1] = [Field {
    name: "generation",
    ty: FieldType::Int,
    unit: "",
    type_name: "",
}];

/// The status topic's fields.
const STATUS_FIELDS: [Field<'static>; 4] = [
    text_field("state"),
    text_field("health"),
    text_field("reason"),
    text_field("refusal"),
];

/// A refusal kind's number as the status topic says it, in decimal, written into `buf`. A `u8`
/// has at most three digits, so nothing is allocated.
fn refusal_text(refusal: u8, buf: &mut [u8; 3]) -> &str {
    let digits = [refusal / 100, refusal / 10 % 10, refusal % 10];
    for (slot, digit) in buf.iter_mut().zip(digits) {
        *slot = b'0' + digit;
    }
    let start = match refusal {
        0..=9 => 2,
        10..=99 => 1,
        _ => 0,
    };
    // The digits are ASCII, so the slice is UTF-8.
    core::str::from_utf8(&buf[start..]).unwrap_or("0")
}

const fn text_field(name: &'static str) -> Field<'static> {
    Field {
        name,
        ty: FieldType::Text,
        unit: "",
        type_name: "",
    }
}

/// Publishes what lemnosd has, for as long as the process runs or until `shared` is stopped.
/// Runs of the devices' actions go to `jobs`, and come back through `shared`.
pub fn run(config: &Config, shared: &Shared, jobs: &Sender<Message>) {
    shared.with(|mirror| mirror.set_lease(config.lease.map(|lease| lease.as_secs())));
    let mut backoff = Backoff::new();
    let mut number = 0u64;
    let mut changes = Changes::default();
    let mut done = Vec::new();
    // Detached runs whose session was lost while they were under way: see [`Kept`].
    let mut kept_runs = Kept::default();
    // What the lost session still holds on the comm core, until it is gone: see [`Held`].
    let mut held: Option<Held> = None;
    let mut said = Said::default();
    while !shared.stopping() {
        let (devices, facts, set) = shared.with(|mirror| {
            let (devices, facts) = mirror.snapshot();
            (devices, facts, mirror.set_generation())
        });
        if devices.is_empty() && facts.is_empty() {
            // Nothing to publish, so no session: wait for lemnosd's devices.
            wait_bell(shared, IDLE);
            shared.take_done(&mut done);
            kept_runs.take_ended(&mut done);
            done.clear();
            shared.take_progress(&mut Vec::new());
            continue;
        }
        number += 1;
        let mut session =
            match Session::open(config, shared, number, &devices, &facts, set, held.as_ref()) {
                Ok(session) => session,
                Err(Opening::StillHeld(message)) => {
                    // The old session is still closing on the comm core. Said once per loss, then
                    // retried on the short interval, with the backoff left where it is.
                    if let Some(held) = held.as_mut()
                        && !held.told
                    {
                        held.told = true;
                        log(&message);
                    }
                    pause(shared, held.as_ref().map_or(RECONNECT, |held| held.every));
                    continue;
                }
                Err(Opening::Failed(message)) => {
                    said.say(&message);
                    pause(shared, backoff.next_delay());
                    continue;
                }
                Err(Opening::Refused(message)) => {
                    log(&message);
                    shared.refuse(message);
                    shared.stop();
                    return;
                }
            };
        held = None;
        said = Said::default();
        log(&format!(
            "connected to Orion node {}; {} devices published",
            session.node,
            session.devices.len()
        ));
        backoff.reset();
        shared.with(|mirror| mirror.mark_all());
        match session.serve(shared, jobs, &mut changes, &mut done, &mut kept_runs) {
            End::Stopped => session.withdraw("lemnos-orion stopped"),
            End::Lost(message) => {
                log(&message);
                // The detached runs under way are held for node logic to give back (see [`Kept`]).
                kept_runs.hold(&session);
                // Dropped first: the next session says the same names, which the old one holds
                // until it is gone. [`Held`] keeps those names and until when.
                held = Some(Held::of(&session));
                drop(session);
                wait_bell(shared, RECONNECT);
            }
        }
    }
}

/// What the session that was lost still holds on the comm core, and until when.
///
/// The comm core frees a session's names when it closes the session, and it closes a session it
/// has not heard from for `idle`. A session the bridge gave up on keeps its names for up to that
/// long, so a new session's declarations and action registrations of those names are refused
/// until then. Orion's answer is `AlreadyExists` (`ClientError::Refused`, wire value 9) for a name
/// another session holds, whoever it is: the value does not say who holds it. What tells the two
/// apart is the bridge's own record. A refusal of a name this bridge published in the session
/// that was lost, while `until` has not passed, is that old session still closing: it is retried
/// every `every` (heartbeat / 2, bounded by idle / 4, as the publisher's own wait), and the backoff
/// does not grow. Any other refusal is a genuine conflict and backs off as before. Nothing gives
/// up. `until` is the loss plus `idle` and a heartbeat: the comm core closes the old session at
/// most a heartbeat after the bridge notices the loss, so it is gone by then.
struct Held {
    names: HashSet<String>,
    until: Instant,
    every: Duration,
    /// Whether the first retry of this loss has been logged.
    told: bool,
}

impl Held {
    /// What `session` holds on the comm core, from the moment its loss is noticed.
    fn of(session: &Session) -> Self {
        let welcomed = session.orion.welcomed();
        let mut names = HashSet::new();
        names.insert(ANCHOR.to_owned());
        names.insert(DEVICE_SET.name.to_owned());
        names.extend(
            session
                .devices
                .values()
                .map(|device| device.resource.name().as_str().to_owned()),
        );
        names.extend(
            session
                .facts
                .iter()
                .map(|fact| format!("{}{}", hello::TOPIC_PREFIX, fact.name)),
        );
        Self {
            names,
            until: Instant::now() + welcomed.idle + welcomed.heartbeat,
            every: (welcomed.heartbeat / 2)
                .min(welcomed.idle / 4)
                .max(Duration::from_millis(10)),
            told: false,
        }
    }

    /// Whether `name` may still be held by the session that was lost.
    fn covers(&self, name: &str) -> bool {
        Instant::now() < self.until && self.names.contains(name)
    }
}

/// The detached runs of kept actions whose session was lost while they were under way.
///
/// Node logic holds each one for this program, and gives it back when a session of the same identity
/// registers the action again: a `Request::Run` with `kept`, the same call number. The job goes on
/// on the call thread meanwhile, and its answer waits here until the run is given back, or until
/// node logic has let the run go (`until`). Keyed by the call number, which node logic issues for the
/// life of the node.
#[derive(Default)]
struct Kept {
    runs: HashMap<u64, KeptRun>,
}

struct KeptRun {
    /// The session the job was handed out on: the tag its answers carry until it is given back.
    session: u64,
    /// When the bridge lets the run go, if node logic has not given it back by then.
    until: Instant,
    /// How the job ended, if it ended while the session was lost.
    ended: Option<Done>,
}

impl Kept {
    /// Holds the detached runs `session` had under way, when its node keeps kept actions at all.
    fn hold(&mut self, session: &Session) {
        if !session.keep {
            return;
        }
        let until = Instant::now() + KEPT + KEPT_SLACK;
        for (&call, flight) in &session.pending {
            if flight.detached {
                self.runs.insert(
                    call,
                    KeptRun {
                        session: flight.session,
                        until,
                        ended: None,
                    },
                );
            }
        }
    }

    /// Takes the answers of held runs out of `done`: each is kept until its run is given back.
    fn take_ended(&mut self, done: &mut Vec<Done>) {
        let mut at = 0;
        while at < done.len() {
            let held = self
                .runs
                .get_mut(&done[at].call)
                .filter(|run| run.session == done[at].session && run.ended.is_none());
            match held {
                Some(run) => run.ended = Some(done.remove(at)),
                None => at += 1,
            }
        }
    }

    /// Lets go of each held run node logic has not given back by its time, and logs it.
    fn expire(&mut self, now: Instant) {
        self.runs.retain(|call, run| {
            if run.until > now {
                return true;
            }
            log(&format!(
                "run {call}: not given back in time, so its end is not known here; it is let go"
            ));
            false
        });
    }
}

/// Why a session could not be opened. A name the lost session still holds is retried on the
/// short interval, not a failure: see [`Held`].
enum Opening {
    StillHeld(String),
    Failed(String),
    /// The comm core will not give this session the role the bridge publishes with: a configuration
    /// error, reported once and not retried.
    Refused(String),
}

/// Why a session ended.
enum End {
    /// Orion went away and did not come back, or refused what it took before.
    Lost(String),
    Stopped,
}

/// One `hello.<fact>` topic: its publisher, and the value it last carried. The publisher is
/// `None` for a fact Orion refused, which is never published.
struct Fact {
    name: String,
    publisher: Option<Publisher>,
    value: Option<String>,
}

/// One device as published.
struct Published {
    resource: Resource,
    values: Option<Publisher>,
    status: Publisher,
    description: Publisher,
    /// The state the record says, and the one it should say.
    written: ResourceState,
    wanted: ResourceState,
}

struct Session {
    orion: Orion,
    node: String,
    /// Where runs of every action of this session arrive. See [`ANCHOR`].
    anchor: Registration,
    /// Whether this node keeps a kept action's detached runs (level 6): device actions are then
    /// registered with `action_kept`, and their runs held on a loss (see [`Kept`]).
    keep: bool,
    number: u64,
    /// By the mirror's slot. Only devices Orion took are here.
    devices: HashMap<u64, Published>,
    /// Full action name to the device's slot and what the action does.
    actions: HashMap<String, (u64, Verb)>,
    /// Runs handed to the call thread and not yet answered, by call number: the session each was
    /// handed out on (its answers carry it) and whether it is detached.
    pending: HashMap<u64, Flight>,
    /// When a refused resource state is written again.
    retry_at: Option<Instant>,
    /// The board's `hello.*` topics.
    facts: Vec<Fact>,
    /// `lemnos.devices`: the device set's generation.
    device_set: Publisher,
}

/// A run handed to the call thread that has not been answered.
#[derive(Clone, Copy, Debug)]
struct Flight {
    session: u64,
    detached: bool,
}

/// Why a device or fact could not be described.
enum Refused {
    /// Orion will not take this device or fact: its name, or a refusal that says so.
    Device(String),
    /// Orion answers `already exists`: the name is held by some session. See [`Held`].
    Taken(String),
    /// The session cannot go on: a lost connection, or a refusal of the session.
    Session(Error),
}

impl Refused {
    fn why(&self) -> String {
        match self {
            Self::Device(why) | Self::Taken(why) => why.clone(),
            Self::Session(error) => error.to_string(),
        }
    }
}

impl Session {
    /// Connects to the comm core and describes every device and fact. It works only when the
    /// node the comm core welcomes this session from is the one `ORION_NODE_ID` names, if it
    /// names one.
    fn open(
        config: &Config,
        shared: &Shared,
        number: u64,
        devices: &[(u64, DeviceSpec)],
        facts: &[(String, String)],
        set: u64,
        held: Option<&Held>,
    ) -> Result<Self, Opening> {
        let options = Options {
            roles: Roles::CLIENT.and(Roles::PROVIDER),
            ..Options::default()
        };
        // Orion's patience keeps trying while nobody answers, paced by Orion (never longer than 8 s
        // between tries), so the bridge needs no loop of its own for a comm core that is not up yet.
        // The first failure is said; the rest are not. The trying stops when the service is stopping:
        // a stop is an answer, so the session ends and the service's own loop sees the stop.
        let orion = Orion::connect_watching(&config.orion, CLIENT_NAME, options, |status| {
            if status.failures == 1 {
                log(&format!(
                    "Orion not available at {}: waiting for it",
                    config.orion
                ));
            }
            !shared.stopping()
        })
        .map_err(|error| {
            match error {
                // The reason is `RoleNotOffered`: the carrier does not offer the provider role. The
                // reason type is not re-exported, so it is matched by its name.
                Error::Client(ClientError::Rejected(reason))
                    if format!("{reason:?}") == "RoleNotOffered" =>
                {
                    Opening::Refused(format!(
                        "Orion at {} does not offer the provider role this bridge publishes with: \
                         that carrier cannot publish. Leave ORION_ADDRESS unset to use the board's own \
                         socket. Stopping; a configuration error is not retried",
                        config.orion
                    ))
                }
                // A path too long for a socket cannot be fixed by trying again (see `config::publishes_on`).
                Error::Client(ClientError::PathTooLong { path, len }) => Opening::Refused(format!(
                    "Orion's socket path {} is {len} bytes, too long for a Unix socket. Stopping; a configuration error is not retried",
                    path.display()
                )),
                // Not a transient: the connect keeps trying through those (see above). What is left is
                // a refusal, which is retried on the backoff like any other failure.
                error => Opening::Failed(format!("Orion at {} refused the session: {error}", config.orion)),
            }
        })?;
        let node = orion.welcomed().node.as_str().to_owned();
        check_node(&node, config.node_id.as_deref()).map_err(Opening::Failed)?;
        // A node below the level that knows kept runs refuses `action_kept` (nothing sent), so its
        // device actions are registered without a window and its runs are not held.
        let keep = orion.welcomed().needs(Feature::KeptRuns).is_ok();
        // Said with each refusal: what was asked, of which node, over which carrier.
        let at = format!("node {node} on {}", config.orion);
        let anchor = orion
            .register_action(
                &ActionDecl {
                    name: ANCHOR,
                    target: Target::Node(orion.welcomed().node),
                    description: "Where lemnos-orion's runs arrive; nothing runs here",
                },
                ANCHOR_POLICY,
            )
            .map_err(|error| {
                opening(
                    held,
                    ANCHOR,
                    error,
                    &format!("Orion did not take {ANCHOR} (register action, {at})"),
                )
            })?;
        let mut device_set = orion
            .declare(&TopicDecl {
                name: DEVICE_SET.name,
                fields: &GENERATION_FIELDS,
                history: DEVICE_SET.history,
                expiry_ms: DEVICE_SET.expiry_ms,
                max_payload: 8,
                on_subscribe: DEVICE_SET.on_subscribe(),
            })
            .map_err(|error| {
                opening(
                    held,
                    DEVICE_SET.name,
                    error,
                    &format!(
                        "Orion did not take {} (declare topic, {at})",
                        DEVICE_SET.name
                    ),
                )
            })?;
        sample(&mut device_set, 0, now(), &generation_bytes(set)).map_err(|error| {
            Opening::Failed(format!("{}: not written: {error}", DEVICE_SET.name))
        })?;
        let mut session = Self {
            orion,
            node,
            anchor,
            keep,
            number,
            devices: HashMap::new(),
            actions: HashMap::new(),
            pending: HashMap::new(),
            retry_at: None,
            facts: Vec::new(),
            device_set,
        };
        for (slot, spec) in devices {
            match session.adopt(*slot, spec) {
                Ok(()) => {}
                Err(Refused::Taken(why))
                    if held.is_some_and(|held| {
                        held.covers(&format!("{RESOURCE_PREFIX}{}", spec.name))
                    }) =>
                {
                    return Err(Opening::StillHeld(format!(
                        "{}: still held by the lost session: {why}",
                        spec.name
                    )));
                }
                Err(Refused::Session(error)) => {
                    return Err(Opening::Failed(format!(
                        "Orion did not take {} (declare topic, {at}): {error}",
                        spec.name
                    )));
                }
                Err(refused) => log(&format!("{}: not published: {}", spec.name, refused.why())),
            }
        }
        for (name, value) in facts {
            match declare_fact(&session.orion, name, value, held) {
                Ok(fact) => session.facts.push(fact),
                Err(Refused::Taken(why)) => {
                    return Err(Opening::StillHeld(format!(
                        "hello.{name}: still held by the lost session: {why}"
                    )));
                }
                Err(refused) => {
                    return Err(Opening::Failed(format!(
                        "Orion did not take hello.{name} (declare topic, {at}): {}",
                        refused.why()
                    )));
                }
            }
        }
        Ok(session)
    }

    /// Describes a device Orion has not seen: its resource, topics and actions. A refusal comes
    /// back as it is, for the caller to decide: a session that is opening may retry a name the
    /// lost session holds (see [`Held`]), and otherwise the device is logged and left out.
    fn adopt(&mut self, slot: u64, spec: &DeviceSpec) -> Result<(), Refused> {
        let (published, verbs) = describe(&self.orion.provider(), spec, self.keep)?;
        for verb in verbs {
            self.actions.insert(
                format!("{}.{}", published.resource.name().as_str(), verb.name()),
                (slot, verb),
            );
        }
        self.devices.insert(slot, published);
        Ok(())
    }

    /// Serves the session until Orion is lost for good, or the service stops.
    fn serve(
        &mut self,
        shared: &Shared,
        jobs: &Sender<Message>,
        changes: &mut Changes,
        done: &mut Vec<Done>,
        kept_runs: &mut Kept,
    ) -> End {
        let mut progress = Vec::new();
        loop {
            if shared.stopping() {
                return End::Stopped;
            }
            shared.take_done(done);
            shared.take_progress(&mut progress);
            let turned = self
                .runs(jobs, done, &mut progress, kept_runs)
                .and_then(|()| {
                    shared.with(|mirror| mirror.take(changes));
                    self.publish(changes)
                })
                .and_then(|()| self.retry_states());
            if let Err(error) = turned {
                if error.is_lost() {
                    return End::Lost(format!("Orion lost ({error}); connecting again"));
                }
                log(&format!("Orion refused: {error}"));
                continue;
            }
            self.wait(shared);
        }
    }

    /// Takes in what the comm core sent, hands new runs to the call thread, and answers the
    /// runs that ended.
    fn runs(
        &mut self,
        jobs: &Sender<Message>,
        done: &mut Vec<Done>,
        progress: &mut Vec<Progress>,
        kept_runs: &mut Kept,
    ) -> Result<(), Error> {
        self.orion.client().drain()?;
        kept_runs.take_ended(done);
        kept_runs.expire(Instant::now());
        for p in progress.drain(..) {
            if self
                .pending
                .get(&p.call)
                .is_none_or(|flight| flight.session != p.session)
            {
                continue;
            }
            if let Some(requests) = self.requests() {
                requests.progress(
                    p.call,
                    p.percent(),
                    &format!("{} of {}", p.done, p.total),
                    &p.data(),
                )?;
            }
        }
        for finished in done.drain(..) {
            // A run is answered on the session it was handed out on: a kept run keeps its first one.
            if self
                .pending
                .get(&finished.call)
                .is_none_or(|flight| flight.session != finished.session)
            {
                continue;
            }
            self.pending.remove(&finished.call);
            if let Some(requests) = self.requests() {
                finish_run(
                    requests,
                    finished.call,
                    finished.ending,
                    finished.output.as_bytes(),
                    &finished.detail,
                )?;
            }
        }
        loop {
            let Some(requests) = self.requests() else {
                return Ok(());
            };
            let Some(request) = requests.poll()? else {
                return Ok(());
            };
            if let Request::Run {
                kept: true,
                call,
                deadline_ms,
                ..
            } = request
            {
                self.give_back(call, deadline_ms, kept_runs, jobs)?;
                continue;
            }
            let (call, name, target, args, caller, token, detached, deadline_ms) = match request {
                Request::Run {
                    call,
                    name,
                    target,
                    args,
                    caller,
                    session,
                    detached,
                    deadline_ms,
                    ..
                } => (
                    call,
                    name,
                    target,
                    args,
                    caller,
                    session,
                    detached,
                    deadline_ms,
                ),
                // The session with this token ended: whatever its callers hold is released
                // (see `calls::caller_gone`).
                Request::CallerGone { session } => {
                    let _ = jobs.send(Message::CallerGone { token: session });
                    continue;
                }
                // A long job is cancelled; a plain call cannot be stopped part-way, so it ends as
                // the call does.
                // The cancel's reason is passed on to calls.rs, which logs it. It releases nothing: a
                // session's end comes as `CallerGone`.
                Request::Cancel { call, reason } => {
                    if self.pending.contains_key(&call) {
                        let _ = jobs.send(Message::Cancel {
                            call,
                            reason: reason.code(),
                        });
                    }
                    continue;
                }
            };
            let job = self.job(call, &name, &target, args, caller).map(|job| Job {
                token,
                detached,
                deadline_ms,
                ..job
            });
            match job {
                Ok(job) => {
                    let flight = Flight {
                        session: self.number,
                        detached,
                    };
                    if jobs.send(Message::Run(job)).is_ok() {
                        self.pending.insert(call, flight);
                    } else if let Some(requests) = self.requests() {
                        finish_run(
                            requests,
                            call,
                            Ending::FAILED,
                            &[],
                            "the call thread has stopped",
                        )?;
                    }
                }
                Err(why) => {
                    if let Some(requests) = self.requests() {
                        finish_run(requests, call, Ending::REJECTED, &[], &why)?;
                    }
                }
            }
        }
    }

    /// A kept run node logic gives back to this session, by its call number: answered at once if it
    /// ended while the session was lost, else carried on with the time left it is given. A run this
    /// bridge does not hold (it was restarted since) is ended indeterminate, never as failed.
    fn give_back(
        &mut self,
        call: u64,
        deadline_ms: u64,
        kept_runs: &mut Kept,
        jobs: &Sender<Message>,
    ) -> Result<(), Error> {
        let Some(run) = kept_runs.runs.remove(&call) else {
            log(&format!(
                "run {call}: given back, but not held by this bridge: its end is not known"
            ));
            return finish_run(
                &mut self.anchor,
                call,
                Ending::INDETERMINATE,
                &[],
                "the bridge that held the run is gone, so how it ended is not known",
            )
            .map_err(Error::from);
        };
        match run.ended {
            Some(done) => {
                log(&format!(
                    "run {call}: given back, and it had ended: answered"
                ));
                finish_run(
                    &mut self.anchor,
                    call,
                    done.ending,
                    done.output.as_bytes(),
                    &done.detail,
                )
                .map_err(Error::from)
            }
            None => {
                log(&format!(
                    "run {call}: given back, and still going: carried on"
                ));
                self.pending.insert(
                    call,
                    Flight {
                        session: run.session,
                        detached: true,
                    },
                );
                // A send to a call thread that has gone fails; the run then never ends here, and
                // node logic ends it at its window. Nothing is sent to node logic for it.
                let _ = jobs.send(Message::Deadline { call, deadline_ms });
                Ok(())
            }
        }
    }

    /// Where runs of every action of this session arrive: the anchor's queue, which every
    /// device's registration shares.
    fn requests(&mut self) -> Option<&mut Registration> {
        Some(&mut self.anchor)
    }

    /// The job for a run, or why it is refused.
    fn job(
        &self,
        call: u64,
        name: &str,
        target: &Target,
        args: Vec<u8>,
        caller: String,
    ) -> Result<Job, String> {
        let &(slot, verb) = self
            .actions
            .get(name)
            .ok_or_else(|| format!("no action {name:?} here"))?;
        let device = self
            .devices
            .get(&slot)
            .ok_or_else(|| format!("no action {name:?} here"))?;
        let resource = device.resource.name();
        if *target != Target::Resource(resource.clone()) {
            return Err(format!(
                "{name} runs against the resource {}",
                resource.as_str()
            ));
        }
        let args = String::from_utf8(args).map_err(|_| "the arguments are not text".to_owned())?;
        Ok(Job {
            session: self.number,
            token: 0,
            detached: false,
            call,
            device: device_name(device),
            verb,
            args,
            caller,
            deadline_ms: 0,
        })
    }

    /// Publishes what changed, in order: devices that went, devices that came, facts, then
    /// states and values.
    fn publish(&mut self, changes: &Changes) -> Result<(), Error> {
        for &slot in &changes.removed {
            self.remove(slot)?;
        }
        for (slot, spec) in &changes.added {
            match self.adopt(*slot, spec) {
                Ok(()) => {}
                Err(Refused::Session(error)) => return Err(error),
                Err(refused) => log(&format!("{}: not published: {}", spec.name, refused.why())),
            }
        }
        if let Some(facts) = &changes.hello {
            self.publish_facts(facts)?;
        }
        let time = now();
        if let Some(set) = changes.set {
            sample(&mut self.device_set, 0, time, &generation_bytes(set))?;
        }
        for &(slot, generation) in &changes.described {
            let Some(device) = self.devices.get_mut(&slot) else {
                continue;
            };
            sample(
                &mut device.description,
                0,
                time,
                &generation_bytes(generation),
            )?;
        }
        for (slot, status) in &changes.status {
            let Some(device) = self.devices.get_mut(slot) else {
                continue;
            };
            let mut refusal = [0u8; 3];
            let texts = [
                state_name(status.state),
                health_name(status.health),
                cut(&status.reason, STATUS_MAX),
                refusal_text(status.refusal, &mut refusal),
            ];
            for (field, text) in texts.iter().enumerate() {
                sample(&mut device.status, field, time, text.as_bytes())?;
            }
            device.wanted = resource_state(status);
            if device.wanted != device.written {
                match device.resource.set_state(device.wanted) {
                    Ok(()) => device.written = device.wanted,
                    Err(error) if error.is_lost() => return Err(error.into()),
                    Err(error) => {
                        log(&format!(
                            "{}: state not written: {error}",
                            device_name(device)
                        ));
                        self.retry_at.get_or_insert_with(|| Instant::now() + RETRY);
                    }
                }
            }
        }
        for &(slot, item, reading, stamp) in &changes.readings {
            let Some(device) = self.devices.get_mut(&slot) else {
                continue;
            };
            let Some(values) = device.values.as_mut() else {
                continue;
            };
            let bytes;
            let payload: &[u8] = match reading {
                Reading::Number(number) => {
                    bytes = number.to_le_bytes();
                    &bytes
                }
                Reading::Flag(flag) => &[u8::from(flag)],
            };
            sample(values, item, SampleTime::from_nanos(stamp), payload)?;
        }
        Ok(())
    }

    /// Takes a device away: its status says it is gone, and its resource is withdrawn with its
    /// topics. Its actions stop being offered here; the run of one that arrives after is refused.
    fn remove(&mut self, slot: u64) -> Result<(), Error> {
        let Some(mut device) = self.devices.remove(&slot) else {
            return Ok(());
        };
        let time = now();
        // A device that is gone is refused by nothing: its refusal is `0`.
        for (field, text) in [GONE, "", DEVICE_GONE, "0"].iter().enumerate() {
            if sample(&mut device.status, field, time, text.as_bytes()).is_err() {
                break;
            }
        }
        self.actions.retain(|_, (owner, _)| *owner != slot);
        let name = device_name(&device);
        match device.resource.withdraw() {
            Ok(()) => {
                log(&format!("{name}: withdrawn from Orion"));
                Ok(())
            }
            Err(error) if error.is_lost() => Err(error.into()),
            Err(error) => {
                log(&format!("{name}: not withdrawn: {error}"));
                Ok(())
            }
        }
    }

    /// Writes again the resource states Orion refused, once their time has come.
    fn retry_states(&mut self) -> Result<(), Error> {
        if self.retry_at.is_none_or(|at| Instant::now() < at) {
            return Ok(());
        }
        self.retry_at = None;
        for device in self.devices.values_mut() {
            if device.wanted == device.written {
                continue;
            }
            match device.resource.set_state(device.wanted) {
                Ok(()) => device.written = device.wanted,
                Err(error) if error.is_lost() => return Err(error.into()),
                Err(_) => {
                    self.retry_at = Some(Instant::now() + RETRY);
                }
            }
        }
        Ok(())
    }

    /// Declares each fact that came, withdraws each that went, and says each value that moved.
    fn publish_facts(&mut self, facts: &[(String, String)]) -> Result<(), Error> {
        let mut index = 0;
        while index < self.facts.len() {
            if facts
                .iter()
                .any(|(name, _)| *name == self.facts[index].name)
            {
                index += 1;
                continue;
            }
            let fact = self.facts.remove(index);
            if let Some(publisher) = fact.publisher {
                match publisher.withdraw() {
                    Ok(()) => {}
                    Err(error) if error.is_lost() => return Err(error.into()),
                    Err(error) => log(&format!("hello.{}: not withdrawn: {error}", fact.name)),
                }
            }
        }
        for (name, value) in facts {
            let Some(at) = self.facts.iter().position(|fact| fact.name == *name) else {
                match declare_fact(&self.orion, name, value, None) {
                    Ok(fact) => self.facts.push(fact),
                    Err(Refused::Session(error)) => return Err(error),
                    // Without a held name, `declare_fact` answers only a session error.
                    Err(_) => {}
                }
                continue;
            };
            let fact = &mut self.facts[at];
            if fact.value.as_deref() == Some(value.as_str()) {
                continue;
            }
            if let Some(publisher) = fact.publisher.as_mut() {
                sample(publisher, 0, now(), value.as_bytes())?;
            }
            fact.value = Some(value.clone());
        }
        Ok(())
    }

    /// Waits for the comm core, a wake-up from the other threads, a ping falling due or a
    /// refused state's retry, whichever is first. The wait is never longer than a quarter of the
    /// idle time, so a comm core that stops answering is found within it: the client then gives
    /// the session up as lost.
    fn wait(&self, shared: &Shared) {
        let welcomed = self.orion.welcomed();
        let mut wait = (welcomed.heartbeat / 2)
            .min(welcomed.idle / 4)
            .max(Duration::from_millis(10));
        if let Some(at) = self.retry_at {
            wait = wait.min(at.saturating_duration_since(Instant::now()));
        }
        let timeout = timespec(wait);
        self.orion.client().session(|session| {
            let mut fds = [
                PollFd::from_borrowed_fd(session.as_fd(), PollFlags::IN),
                PollFd::from_borrowed_fd(shared.bell_fd(), PollFlags::IN),
            ];
            // An error here is an interrupted wait; the loop looks again.
            let _ = poll(&mut fds, Some(&timeout));
        });
        shared.quiet();
    }

    /// Takes every device away, each with its status said `gone`, and ends the session. The
    /// facts' topics end with it: their subscribers are told the publisher is gone.
    fn withdraw(mut self, why: &str) {
        let time = now();
        for device in self.devices.values_mut() {
            for (field, text) in [GONE, "", why, "0"].iter().enumerate() {
                if sample(&mut device.status, field, time, text.as_bytes()).is_err() {
                    break;
                }
            }
        }
        for (_, device) in self.devices.drain() {
            let name = device_name(&device);
            if let Err(error) = device.resource.withdraw() {
                log(&format!("{name}: not withdrawn: {error}"));
            }
        }
    }
}

/// The device's name, from its resource's: `lemnos.<device>` without the prefix.
fn device_name(device: &Published) -> String {
    device
        .resource
        .name()
        .as_str()
        .strip_prefix(RESOURCE_PREFIX)
        .unwrap_or_else(|| device.resource.name().as_str())
        .to_owned()
}

/// Declares the `hello.<fact>` topic and says its value. A fact another program already
/// publishes, or one Orion will not take, is logged and held as not published; a lost
/// connection is an error.
fn declare_fact(
    orion: &Orion,
    name: &str,
    value: &str,
    held: Option<&Held>,
) -> Result<Fact, Refused> {
    let topic = format!("{}{name}", hello::TOPIC_PREFIX);
    let fields = [text_field("value")];
    let decl = TopicDecl {
        name: &topic,
        fields: &fields,
        history: FACT.history,
        expiry_ms: FACT.expiry_ms,
        max_payload: hello::MAX_VALUE_BYTES as u32,
        on_subscribe: FACT.on_subscribe(),
    };
    match orion.declare(&decl) {
        Ok(mut publisher) => {
            sample(&mut publisher, 0, now(), value.as_bytes()).map_err(Refused::Session)?;
            Ok(Fact {
                name: name.to_owned(),
                publisher: Some(publisher),
                value: Some(value.to_owned()),
            })
        }
        Err(error) => match fact_refused(error) {
            Refused::Taken(why) if held.is_some_and(|held| held.covers(&topic)) => {
                Err(Refused::Taken(why))
            }
            Refused::Device(why) | Refused::Taken(why) => {
                log(&format!("{topic}: not published: {why}"));
                Ok(Fact {
                    name: name.to_owned(),
                    publisher: None,
                    value: None,
                })
            }
            Refused::Session(error) => Err(Refused::Session(error)),
        },
    }
}

fn fact_refused(error: Error) -> Refused {
    match error {
        Error::Topic(error) => topic_refused(error),
        Error::Client(error) => client_refused(error),
    }
}

/// A device's topic as its class says it is declared: a state takes [`Class::on_subscribe`] (its newest
/// value for a late subscriber), and not the resource's shorthand, which starts it from now.
fn class_topic<'a>(class: &Class, fields: &'a [Field<'a>], max_payload: u32) -> TopicDecl<'a> {
    TopicDecl {
        name: class.name,
        fields,
        history: class.history,
        expiry_ms: class.expiry_ms,
        max_payload,
        on_subscribe: class.on_subscribe(),
    }
}

/// Describes one device to Orion: its topics and actions, then its resource, which is published
/// last so that it is never listed before its actions are there (see the module's notes).
fn describe(
    provider: &Provider,
    spec: &DeviceSpec,
    keep: bool,
) -> Result<(Published, Vec<Verb>), Refused> {
    let name = format!("{RESOURCE_PREFIX}{}", spec.name);
    let mut describing = provider
        .resource(&name, RESOURCE_KIND)
        .map_err(topic_refused)?
        .label("lemnos.driver", &spec.driver);
    for tag in &spec.tags {
        describing = describing.label("lemnos.tag", tag);
    }
    for control in &spec.controls {
        describing = describing.label(&format!("lemnos.control.{}", control.name), &control.takes);
    }
    let values = if spec.items.is_empty() {
        None
    } else {
        let fields: Vec<Field<'_>> = spec
            .items
            .iter()
            .map(|item| Field {
                name: &item.name,
                ty: match item.plain {
                    Plain::Number => FieldType::Float,
                    Plain::Flag => FieldType::Bool,
                },
                unit: &item.unit,
                type_name: "",
            })
            .collect();
        Some(
            describing
                .topic_with(&class_topic(&VALUES, &fields, 8))
                .map_err(topic_refused)?,
        )
    };
    let status = describing
        .topic_with(&class_topic(&STATUS, &STATUS_FIELDS, STATUS_MAX as u32))
        .map_err(topic_refused)?;
    let description = describing
        .topic_with(&class_topic(&DESCRIPTION, &GENERATION_FIELDS, 8))
        .map_err(topic_refused)?;
    let verbs: Vec<Verb> = Verb::ALL
        .into_iter()
        .filter(|verb| *verb != Verb::Set || !spec.controls.is_empty())
        .collect();
    for verb in &verbs {
        // A device's verb may be a long job: on a node that keeps kept runs, its detached runs are
        // kept for us while our session is lost (see [`Kept`]).
        if keep {
            describing.action_kept(verb.name(), verb.description(), POLICY, KEPT)
        } else {
            describing.action(verb.name(), verb.description(), POLICY)
        }
        .map_err(client_refused)?;
    }
    let state = resource_state(&spec.status);
    let resource = describing.publish(state).map_err(client_refused)?;
    Ok((
        Published {
            resource,
            values,
            status,
            description,
            written: state,
            wanted: state,
        },
        verbs,
    ))
}

/// Whether Orion refused a session's declaration or registration because the name is held.
/// Ends a run with the bridge's ending: Orion's outcome, and the status code a caller branches on.
fn finish_run(
    requests: &mut Registration,
    call: u64,
    ending: Ending,
    output: &[u8],
    detail: &str,
) -> Result<(), ClientError> {
    requests.finish_with_code(call, ending.outcome, ending.code, output, detail)
}

fn is_taken(error: &Error) -> bool {
    matches!(
        error,
        Error::Client(ClientError::Refused {
            error: Refusal::AlreadyExists,
            ..
        }) | Error::Topic(TopicError::Client(ClientError::Refused {
            error: Refusal::AlreadyExists,
            ..
        }))
    )
}

/// The failure to open a session for `what` (`error` says why). Held by the lost session is
/// retried; anything else is a failure.
fn opening(held: Option<&Held>, name: &str, error: Error, what: &str) -> Opening {
    let message = format!("{what}: {error}");
    if held.is_some_and(|held| held.covers(name)) && is_taken(&error) {
        Opening::StillHeld(message)
    } else {
        Opening::Failed(message)
    }
}

fn topic_refused(error: TopicError) -> Refused {
    match error {
        TopicError::Invalid(why) => Refused::Device(why),
        TopicError::Client(error) => client_refused(error),
        other => Refused::Session(Error::Topic(other)),
    }
}

/// A name Orion will not take is this device's problem, and so is a name another session holds
/// (`Taken`, which the caller may retry: see [`Held`]); any other refusal or a lost connection
/// ends the session.
fn client_refused(error: ClientError) -> Refused {
    match error {
        ClientError::Refused {
            error: why @ Refusal::AlreadyExists,
            ..
        } => Refused::Taken(why.as_str().to_owned()),
        ClientError::Refused {
            error: why @ (Refusal::InvalidName | Refusal::NameTooLong | Refusal::EmptyName),
            ..
        } => Refused::Device(why.as_str().to_owned()),
        ClientError::Protocol(why) => Refused::Device(why.to_owned()),
        // The comm core answered no session where a hello was due (C57): the session is over, and the
        // bridge opens a new one, as for a lost connection.
        reject @ ClientError::Rejected(RejectReason::NoSession) => {
            Refused::Session(Error::Client(reject))
        }
        other => Refused::Session(Error::Client(other)),
    }
}

/// One sample of field `field`.
/// A generation counter's payload: an `Int` field, little-endian.
fn generation_bytes(generation: u64) -> [u8; 8] {
    (generation as i64).to_le_bytes()
}

fn sample(
    publisher: &mut Publisher,
    field: usize,
    time: SampleTime,
    payload: &[u8],
) -> Result<(), Error> {
    let Ok(id) = u16::try_from(field) else {
        return Ok(());
    };
    match publisher.publish(FieldId(id), time, payload) {
        Ok(()) => Ok(()),
        Err(TopicError::Client(error)) => Err(error.into()),
        // A sample that does not fit its own declaration is this service's mistake; it is
        // left out rather than ending the session.
        Err(error) => {
            log(&format!("a sample was left out: {error}"));
            Ok(())
        }
    }
}

/// What a device's state makes its resource.
pub const fn resource_state(status: &Status) -> ResourceState {
    match (status.state, status.health) {
        (DeviceState::Running, DeviceHealth::Failed) | (DeviceState::BackingOff, _) => {
            ResourceState::Failed
        }
        (DeviceState::Running, _) => ResourceState::Ready,
        (DeviceState::Off, _) => ResourceState::Pending,
    }
}

/// The time a sample belongs to: `CLOCK_MONOTONIC` now, as Orion's samples are stamped. A
/// sample a driver read keeps the time the driver read it, unchanged.
fn now() -> SampleTime {
    SampleTime::now()
}

/// `text` cut to at most `max` bytes, at a character boundary.
fn cut(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn timespec(wait: Duration) -> Timespec {
    Timespec {
        tv_sec: i64::try_from(wait.as_secs()).unwrap_or(i64::MAX),
        tv_nsec: i64::from(wait.subsec_nanos()),
    }
}

/// Waits `wait` before the next try of a session, and ends early only when the service stops. A
/// change in the mirror does not end it: with no session there is nothing to publish it on, and a
/// wake-up per change (a device's values stream) would turn the backoff into a retry every few
/// milliseconds.
fn pause(shared: &Shared, wait: Duration) {
    let until = Instant::now() + wait;
    while !shared.stopping() {
        let now = Instant::now();
        if now >= until {
            break;
        }
        std::thread::sleep((until - now).min(Duration::from_millis(20)));
    }
}

/// Waits up to `wait` for a wake-up, with no session.
fn wait_bell(shared: &Shared, wait: Duration) {
    if shared.stopping() {
        return;
    }
    let mut fds = [PollFd::from_borrowed_fd(shared.bell_fd(), PollFlags::IN)];
    let _ = poll(&mut fds, Some(&timespec(wait)));
    shared.quiet();
}

/// A comm core welcoming this service as another node than `ORION_NODE_ID` gets nothing.
pub fn check_node(welcomed: &str, expected: Option<&str>) -> Result<(), String> {
    match expected {
        Some(expected) if expected != welcomed => Err(format!(
            "Orion's node is {welcomed:?} but ORION_NODE_ID is {expected:?}; not publishing"
        )),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every topic the bridge publishes is classified, by name, and each is a state with an
    /// expiry of zero (current until changed or withdrawn), as the docs say.
    /// State topics replay their last value to a late subscriber; event topics do not.
    #[test]
    fn state_replays_its_last_value_and_an_event_does_not() {
        for class in CLASSES {
            assert_eq!(class.on_subscribe(), OnSubscribe::Latest, "{}", class.name);
        }
        assert_eq!(on_subscribe_for(Kind::Event), OnSubscribe::FromNow);
    }

    #[test]
    fn every_topic_is_classified_as_a_state_valid_until_changed() {
        let names: Vec<&str> = CLASSES.iter().map(|class| class.name).collect();
        assert_eq!(
            names,
            [
                "values",
                "status",
                "description",
                "lemnos.devices",
                "hello.<fact>"
            ]
        );
        for class in CLASSES {
            assert_eq!(class.kind, Kind::State, "{}", class.name);
            assert_eq!(class.expiry_ms, 0, "{}", class.name);
            assert_eq!(class.history, 2, "{}: a latest-value topic", class.name);
        }
    }

    fn status(state: DeviceState, health: DeviceHealth) -> Status {
        Status {
            state,
            health,
            reason: String::new(),
            refusal: 0,
        }
    }

    #[test]
    fn the_welcomed_node_must_be_the_configured_one_if_one_is() {
        assert!(check_node("node-a", Some("node-a")).is_ok());
        assert!(check_node("node-a", None).is_ok());
        let refused = check_node("node-b", Some("node-a")).unwrap_err();
        assert!(refused.contains("node-b"));
        assert!(refused.contains("not publishing"));
    }

    #[test]
    fn a_device_state_is_its_resource_state() {
        use DeviceHealth::{Degraded, Failed, Ok};
        use DeviceState::{BackingOff, Off, Running};
        assert_eq!(resource_state(&status(Running, Ok)), ResourceState::Ready);
        assert_eq!(
            resource_state(&status(Running, Degraded)),
            ResourceState::Ready
        );
        assert_eq!(
            resource_state(&status(Running, Failed)),
            ResourceState::Failed
        );
        assert_eq!(
            resource_state(&status(BackingOff, Ok)),
            ResourceState::Failed
        );
        assert_eq!(resource_state(&status(Off, Ok)), ResourceState::Pending);
    }

    #[test]
    fn a_long_reason_is_cut_at_a_character() {
        assert_eq!(cut("abc", 8), "abc");
        assert_eq!(cut("ab²c", 3), "ab");
        assert_eq!(cut("ab²c", 4), "ab²");
    }
}
