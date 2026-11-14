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
//! - the topic `lemnos.<device>.status` has the fields `state`, `health` and `reason`, as
//!   `lemnos-ctl list` names them, published as they change;
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
//! bridge opens that session itself rather than with `Orion::reconnect`, and each device's actions
//! are registered before its resource is published, so a resource is never listed without its
//! actions. (`Orion::reconnect` restores topics, then actions, then records, which is the same
//! order.) Each caller session whose runs came over a lost session is released as ended: its end
//! is not said again to a session that is gone (see [`Session::tokens`]).

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fs::File;
use std::io::Read;
use std::os::fd::AsFd;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use orion::actions::{ActionDecl, Admits, Outcome, Policy, Registration, Request, Target};
use orion::provider::{Provider, Resource, ResourceState};
use orion::topics::{
    Field, FieldId, FieldType, OnSubscribe, Publisher, SampleTime, TopicDecl, TopicError,
};
use orion::types::Error as Refusal;
use orion::{Address, ClientError, Error, Options, Orion, Roles};
use rustix::event::{PollFd, PollFlags, Timespec, poll};

use lemnos_client::{DeviceHealth, DeviceState};
use lemnos_linux_sys::inotify;

use crate::backoff::Backoff;
use crate::calls::{Done, Job, Message, Progress, Verb};
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
/// Samples of each field a topic keeps.
const LATEST: u32 = 2;
/// The longest status text published. A longer reason is cut at a character boundary.
const STATUS_MAX: usize = 256;
/// How long to wait with no devices and no facts for lemnosd to have some.
const IDLE: Duration = Duration::from_secs(1);
/// How soon a resource state Orion refused is written again.
const RETRY: Duration = Duration::from_secs(1);
/// How soon a session is opened again after the comm core went away, before the retry delays of
/// a session Orion refuses. The comm core is usually back within this.
/// (Used for the lost session's names only: the first try after a loss is made at once.)
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
///
/// Every state is declared with zero: a state is current until it changes, and the bridge says
/// `gone` (or withdraws the topic) when it stops being true. The two topics the bridge declares
/// itself, `hello.<fact>` and `lemnos.devices`, also start a subscriber on the network from their
/// newest value (`on_subscribe` = `Latest`, see [`OnSubscribe`]). A device's topics (`values`,
/// `status`, `description`) are declared by the provider's `topic`, which at Orion R2 takes
/// neither an expiry nor an `on_subscribe`: they are zero and `FromNow` there. The table is documented
/// in `docs/daemon.md`.
#[derive(Clone, Copy, Debug)]
pub struct Class {
    pub name: &'static str,
    pub kind: Kind,
    pub expiry_ms: u32,
    /// Samples Orion keeps per field: 2 for a latest-value topic (a state), which is all a state
    /// needs, since the newest sample is the value.
    pub history: u32,
    /// What a network subscriber gets first. `Latest` for the topics the bridge declares itself; the
    /// provider's device topics are `FromNow` whatever this says (see the module's table).
    pub on_subscribe: OnSubscribe,
}

/// A device's readings: the last reading is the value, published as the driver reads a change.
pub const VALUES: Class = Class {
    name: "values",
    kind: Kind::State,
    expiry_ms: 0,
    history: LATEST,
    on_subscribe: OnSubscribe::FromNow,
};
/// A device's state, health and reason: current until it changes; the bridge says `gone` when
/// lemnosd stops listing the device.
pub const STATUS: Class = Class {
    name: "status",
    kind: Kind::State,
    expiry_ms: 0,
    history: LATEST,
    on_subscribe: OnSubscribe::FromNow,
};
/// A device's description generation: a counter that moves each time what `describe` says of the
/// device changes (its state or reason). A client that sees it move asks `describe` again.
pub const DESCRIPTION: Class = Class {
    name: "description",
    kind: Kind::State,
    expiry_ms: 0,
    history: LATEST,
    on_subscribe: OnSubscribe::FromNow,
};
/// The device set's generation: a counter that moves when a device comes or goes.
pub const DEVICE_SET: Class = Class {
    name: "lemnos.devices",
    kind: Kind::State,
    expiry_ms: 0,
    history: LATEST,
    on_subscribe: OnSubscribe::Latest,
};
/// A board fact (`hello.<fact>`): current until the fact changes or the session ends.
pub const FACT: Class = Class {
    name: "hello.<fact>",
    kind: Kind::State,
    expiry_ms: 0,
    history: LATEST,
    on_subscribe: OnSubscribe::Latest,
};
/// Every topic the bridge publishes, and how it is classified.
pub const CLASSES: [Class; 5] = [VALUES, STATUS, DESCRIPTION, DEVICE_SET, FACT];

/// A generation counter's one field.
const GENERATION_FIELDS: [Field<'static>; 1] = [Field {
    name: "generation",
    ty: FieldType::Int,
    unit: "",
    type_name: "",
}];

/// The status topic's fields.
const STATUS_FIELDS: [Field<'static>; 3] = [
    text_field("state"),
    text_field("health"),
    text_field("reason"),
];

const fn text_field(name: &'static str) -> Field<'static> {
    Field {
        name,
        ty: FieldType::Text,
        unit: "",
        type_name: "",
    }
}

/// The named state a refusal of the Orion session is logged as. Every refusal, whatever Orion
/// said, takes this one path, so it is never a timeout or a silent retry: the bridge logs it, waits
/// its backoff, and tries again. (Facts are published only inside a session, so a refused bridge has
/// none to say: the log line is its state.)
pub fn refusal(why: &str) -> String {
    format!("orion: refused: {why}")
}

/// The named state a lost session is logged as, see [`refusal`].
pub fn lost(why: &str) -> String {
    format!("orion: lost: {why}; connecting again")
}

/// Publishes what lemnosd has, for as long as the process runs or until `shared` is stopped.
/// Runs of the devices' actions go to `jobs`, and come back through `shared`.
pub fn run(config: &Config, shared: &Shared, jobs: &Sender<Message>) {
    shared.with(|mirror| mirror.set_lease(config.lease.map(|lease| lease.as_secs())));
    let mut backoff = Backoff::new();
    let mut number = 0u64;
    let mut changes = Changes::default();
    let mut done = Vec::new();
    // What the lost session still holds on the comm core, until it is gone: see [`Held`].
    let mut held: Option<Held> = None;
    // The comm core's socket directory, watched for its socket to appear: a restarting comm core is
    // then connected to when its socket is there, not when a delay ends.
    let watch = SocketWatch::new(&config.orion);
    while !shared.stopping() {
        let (devices, facts, set) = shared.with(|mirror| {
            let (devices, facts) = mirror.snapshot();
            (devices, facts, mirror.set_generation())
        });
        if devices.is_empty() && facts.is_empty() {
            // Nothing to publish, so no session: wait for lemnosd's devices.
            wait_bell(shared, IDLE);
            shared.take_done(&mut done);
            done.clear();
            shared.take_progress(&mut Vec::new());
            continue;
        }
        number += 1;
        // Before the try, so a socket that appears during it wakes the next wait.
        if let Some(watch) = &watch {
            watch.appeared();
        }
        let mut session = match Session::open(config, number, &devices, &facts, set, held.as_ref())
        {
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
                log(&message);
                if pause_for_socket(shared, backoff.next_delay(), watch.as_ref()) {
                    // The socket was just made, and is not listening yet or not ready: try soon
                    // again, not after a delay grown while it was absent.
                    backoff.reset();
                }
                continue;
            }
        };
        held = None;
        log(&format!(
            "connected to Orion node {}; {} devices published",
            session.node,
            session.devices.len()
        ));
        shared.connected(session.devices.len());
        backoff.reset();
        shared.with(|mirror| mirror.mark_all());
        match session.serve(shared, jobs, &mut changes, &mut done) {
            End::Stopped => session.withdraw("lemnos-orion stopped"),
            End::Lost(message) => {
                log(&message);
                // The sessions this one served are not told they ended: say so to the call thread,
                // which releases what they held. A session that is still alive gets its holds back
                // only by asking again.
                for token in session.tokens.drain() {
                    let _ = jobs.send(Message::CallerGone { token });
                }
                // Orion ended every run of this session with it, and cannot say so: their jobs are
                // cancelled, so the device is free for the next session.
                let _ = jobs.send(Message::SessionLost {
                    session: session.number,
                });
                // Dropped first: the next session says the same names, which the old one holds
                // until it is gone. [`Held`] keeps those names and until when.
                held = Some(Held::of(&session));
                drop(session);
                // The first try after a loss is made at once, by the loop.
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

/// Why a session could not be opened. A name the lost session still holds is retried on the
/// short interval, not a failure: see [`Held`].
enum Opening {
    StillHeld(String),
    Failed(String),
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

/// What an Orion run asks for: its action, target and arguments, its deadline in milliseconds (zero
/// for none), who asked and the token of the session they asked on.
struct Asked {
    name: String,
    target: Target,
    args: Vec<u8>,
    deadline_ms: u64,
    caller: String,
    token: u64,
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
    number: u64,
    /// By the mirror's slot. Only devices Orion took are here.
    devices: HashMap<u64, Published>,
    /// Full action name to the device's slot and what the action does.
    actions: HashMap<String, (u64, Verb)>,
    /// Runs handed to the call thread and not yet answered.
    pending: HashSet<u64>,
    /// When a refused resource state is written again.
    retry_at: Option<Instant>,
    /// The board's `hello.*` topics.
    facts: Vec<Fact>,
    /// `lemnos.devices`: the device set's generation.
    device_set: Publisher,
    /// The tokens of the caller sessions whose runs this session has been asked for, and whose end
    /// Orion has not said yet. A session that is lost takes them with it: their end is never said
    /// to this program, so the call thread is told it here instead.
    tokens: HashSet<u64>,
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
        let orion = Orion::connect_with(&config.orion, CLIENT_NAME, options).map_err(|error| {
            Opening::Failed(format!("Orion not available at {}: {error}", config.orion))
        })?;
        let node = orion.welcomed().node.as_str().to_owned();
        check_node(&node, config.node_id.as_deref()).map_err(Opening::Failed)?;
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
                opening(held, ANCHOR, error, &format!("Orion did not take {ANCHOR}"))
            })?;
        let mut device_set = orion
            .declare(&TopicDecl {
                name: DEVICE_SET.name,
                fields: &GENERATION_FIELDS,
                history: DEVICE_SET.history,
                expiry_ms: DEVICE_SET.expiry_ms,
                max_payload: 8,
                on_subscribe: DEVICE_SET.on_subscribe,
            })
            .map_err(|error| {
                opening(
                    held,
                    DEVICE_SET.name,
                    error,
                    &format!("Orion did not take {}", DEVICE_SET.name),
                )
            })?;
        sample(&mut device_set, 0, now(), &generation_bytes(set)).map_err(|error| {
            Opening::Failed(format!("{}: not written: {error}", DEVICE_SET.name))
        })?;
        let mut session = Self {
            orion,
            node,
            anchor,
            number,
            devices: HashMap::new(),
            actions: HashMap::new(),
            pending: HashSet::new(),
            retry_at: None,
            facts: Vec::new(),
            device_set,
            tokens: HashSet::new(),
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
                        "Orion did not take {}: {error}",
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
                        "Orion did not take hello.{name}: {}",
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
        let (published, verbs) = describe(&self.orion.provider(), spec)?;
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
    ) -> End {
        let mut progress = Vec::new();
        loop {
            if shared.stopping() {
                return End::Stopped;
            }
            shared.take_done(done);
            shared.take_progress(&mut progress);
            let turned = self
                .runs(jobs, done, &mut progress)
                .and_then(|()| {
                    shared.with(|mirror| mirror.take(changes));
                    self.publish(changes)
                })
                .and_then(|()| self.retry_states());
            if let Err(error) = turned {
                // Node logic away (`Unavailable`) is the same as a lost connection: the program is
                // not served until it is back, so the session is given up and connected again on
                // the one backoff path, with the reason logged.
                if is_connecting(&error) {
                    return End::Lost(lost(&error.to_string()));
                }
                log(&refusal(&error.to_string()));
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
    ) -> Result<(), Error> {
        self.orion.client().drain()?;
        for p in progress.drain(..) {
            if p.session != self.number || !self.pending.contains(&p.call) {
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
            if finished.session != self.number || !self.pending.remove(&finished.call) {
                continue;
            }
            if let Some(requests) = self.requests() {
                requests.finish(
                    finished.call,
                    finished.outcome,
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
            let (call, run) = match request {
                Request::Run {
                    call,
                    name,
                    target,
                    args,
                    deadline_ms,
                    caller,
                    session,
                } => (
                    call,
                    Asked {
                        name,
                        target,
                        args,
                        deadline_ms,
                        caller,
                        token: session,
                    },
                ),
                // A long job is cancelled; a plain call cannot be stopped part-way, so it ends as
                // the call does. The reason is passed on: a cancel for a session that ended renews
                // nothing, and the session's end is what releases its holds (see `CallerGone`).
                Request::Cancel { call, reason } => {
                    if self.pending.contains(&call) {
                        let _ = jobs.send(Message::Cancel {
                            session: self.number,
                            call,
                            reason,
                        });
                    }
                    continue;
                }
                // The session with this token ended, for whatever cause. Its runs end, and its
                // connection closes at once: see the call thread.
                Request::CallerGone { session } => {
                    self.tokens.remove(&session);
                    let _ = jobs.send(Message::CallerGone { token: session });
                    continue;
                }
            };
            let job = self.job(call, run);
            match job {
                Ok(job) => {
                    if job.token != 0 {
                        self.tokens.insert(job.token);
                    }
                    if jobs.send(Message::Run(job)).is_ok() {
                        self.pending.insert(call);
                    } else if let Some(requests) = self.requests() {
                        requests.finish(
                            call,
                            Outcome::Failed,
                            &[],
                            "the call thread has stopped",
                        )?;
                    }
                }
                Err(why) => {
                    if let Some(requests) = self.requests() {
                        requests.finish(call, Outcome::Rejected, &[], &why)?;
                    }
                }
            }
        }
    }

    /// Where runs of every action of this session arrive: the anchor's queue, which every
    /// device's registration shares.
    fn requests(&mut self) -> Option<&mut Registration> {
        Some(&mut self.anchor)
    }

    /// The job for a run, or why it is refused.
    fn job(&self, call: u64, run: Asked) -> Result<Job, String> {
        let Asked {
            name,
            target,
            args,
            deadline_ms,
            caller,
            token,
        } = run;
        let name = name.as_str();
        let &(slot, verb) = self
            .actions
            .get(name)
            .ok_or_else(|| format!("no action {name:?} here"))?;
        let device = self
            .devices
            .get(&slot)
            .ok_or_else(|| format!("no action {name:?} here"))?;
        let resource = device.resource.name();
        if target != Target::Resource(resource.clone()) {
            return Err(format!(
                "{name} runs against the resource {}",
                resource.as_str()
            ));
        }
        let args = String::from_utf8(args).map_err(|_| "the arguments are not text".to_owned())?;
        Ok(Job {
            session: self.number,
            call,
            device: device_name(device),
            verb,
            args,
            caller,
            token,
            deadline_ms,
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
            let texts = [
                state_name(status.state),
                health_name(status.health),
                cut(&status.reason, STATUS_MAX),
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
        for (field, text) in [GONE, "", DEVICE_GONE].iter().enumerate() {
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
            for (field, text) in [GONE, "", why].iter().enumerate() {
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
        on_subscribe: FACT.on_subscribe,
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

/// Describes one device to Orion: its topics and actions, then its resource, which is published
/// last so that it is never listed before its actions are there (see the module's notes).
fn describe(provider: &Provider, spec: &DeviceSpec) -> Result<(Published, Vec<Verb>), Refused> {
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
                .topic(VALUES.name, &fields, VALUES.history, 8)
                .map_err(topic_refused)?,
        )
    };
    let status = describing
        .topic(
            STATUS.name,
            &STATUS_FIELDS,
            STATUS.history,
            STATUS_MAX as u32,
        )
        .map_err(topic_refused)?;
    let description = describing
        .topic(DESCRIPTION.name, &GENERATION_FIELDS, DESCRIPTION.history, 8)
        .map_err(topic_refused)?;
    let verbs: Vec<Verb> = Verb::ALL
        .into_iter()
        .filter(|verb| *verb != Verb::Set || !spec.controls.is_empty())
        .collect();
    for verb in &verbs {
        describing
            .action(verb.name(), verb.description(), POLICY)
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
fn is_taken(error: &Error) -> bool {
    matches!(
        error,
        Error::Client(ClientError::Refused(Refusal::AlreadyExists))
            | Error::Topic(TopicError::Client(ClientError::Refused(
                Refusal::AlreadyExists
            )))
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

/// ORION-PIN(C36): the wire code of Orion's `Error::Restoring`, which C36 adds at code 20. A listed name
/// whose owner is away is answered with it until the owner is connected again. At the pinned revision no
/// case has this code (a refusal with an unknown code is `Internal`), so the arm below is inert until the
/// pin moves. Matched by code, not by name, so the same arm compiles at both revisions.
const RESTORING_CODE: u16 = 20;

/// Whether an error means the bridge must connect again rather than carry on: a lost connection, or Orion
/// refusing with `Unavailable` (node logic is away) or with `Restoring` (ORION-PIN(C36): a name's owner is
/// away and is not back yet). Both are the one backoff path. Nothing else is retried in place: a `NotFound`
/// is an answer about the names, not the session.
fn is_connecting(error: &Error) -> bool {
    if error.is_lost() {
        return true;
    }
    let refusal = match error {
        Error::Client(ClientError::Refused(refusal))
        | Error::Topic(TopicError::Client(ClientError::Refused(refusal))) => *refusal,
        _ => return false,
    };
    refusal == Refusal::Unavailable || refusal.code() == RESTORING_CODE
}

/// A name Orion will not take is this device's problem, and so is a name another session holds
/// (`Taken`, which the caller may retry: see [`Held`]); any other refusal or a lost connection
/// ends the session.
fn client_refused(error: ClientError) -> Refused {
    match error {
        ClientError::Refused(why @ Refusal::AlreadyExists) => {
            Refused::Taken(why.as_str().to_owned())
        }
        ClientError::Refused(
            why @ (Refusal::InvalidName | Refusal::NameTooLong | Refusal::EmptyName),
        ) => Refused::Device(why.as_str().to_owned()),
        ClientError::Protocol(why) => Refused::Device(why.to_owned()),
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

/// The comm core's socket name in its directory, watched with inotify (`lemnos-linux-sys`).
struct SocketWatch {
    file: File,
    name: OsString,
}

impl SocketWatch {
    /// Watches the directory of a local comm core's socket; `None` for a network address, or where
    /// the directory cannot be watched (the backoff alone is then the wait).
    fn new(address: &Address) -> Option<Self> {
        let Address::Local(path) = address else {
            return None;
        };
        let dir = path.parent().filter(|dir| !dir.as_os_str().is_empty())?;
        let fd = inotify::init().ok()?;
        inotify::add_watch(
            fd.as_fd(),
            dir,
            inotify::mask::CREATE | inotify::mask::MOVED_TO,
        )
        .ok()?;
        Some(Self {
            file: File::from(fd),
            name: path.file_name()?.to_owned(),
        })
    }

    /// Reads what happened since the last call, and says whether the socket's name was created.
    fn appeared(&self) -> bool {
        let mut seen = false;
        let mut buf = [0u8; 4096];
        // `WouldBlock` (the descriptor is non-blocking) or any other error ends the reading.
        while let Ok(n) = (&self.file).read(&mut buf) {
            if n == 0 {
                break;
            }
            inotify::for_each_event(&buf[..n], |_, _, name| {
                seen |= name == Some(self.name.as_os_str());
            });
        }
        seen
    }
}

/// [`pause`], ended early by the comm core's socket appearing (when it can be watched); says whether
/// it was.
fn pause_for_socket(shared: &Shared, wait: Duration, watch: Option<&SocketWatch>) -> bool {
    let Some(watch) = watch else {
        pause(shared, wait);
        return false;
    };
    let until = Instant::now() + wait;
    while !shared.stopping() {
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        let mut fds = [
            PollFd::from_borrowed_fd(watch.file.as_fd(), PollFlags::IN),
            PollFd::from_borrowed_fd(shared.bell_fd(), PollFlags::IN),
        ];
        let _ = poll(&mut fds, Some(&timespec(left)));
        // A change in the mirror is no reason to try again (see [`pause`]); a stop is.
        shared.quiet();
        if watch.appeared() {
            return true;
        }
    }
    false
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
    #[test]
    fn an_unavailable_refusal_is_connecting() {
        // Node logic away: connect again, on the one backoff path.
        let unavailable = Error::Client(ClientError::Refused(Refusal::Unavailable));
        assert!(is_connecting(&unavailable));
        let topic = Error::Topic(TopicError::Client(ClientError::Refused(
            Refusal::Unavailable,
        )));
        assert!(is_connecting(&topic));
        // A lost connection is connecting too.
        assert!(is_connecting(&Error::Client(ClientError::Lost)));
        // A name the comm core will not give is an answer about the name, not a reason to connect.
        let not_found = Error::Client(ClientError::Refused(Refusal::NotFound));
        assert!(!is_connecting(&not_found));
    }

    /// ORION-PIN(C36): `Error::Restoring` (code 20) is connecting, on the same path as `Unavailable`. This
    /// needs the pinned Orion revision with C36, where code 20 exists: at dbd5fc08 `from_code(20)` is `None`.
    #[test]
    #[ignore = "needs the Orion rev with C36: Error::Restoring"]
    fn a_restoring_refusal_is_connecting_and_a_not_found_is_not() {
        let restoring =
            Refusal::from_code(RESTORING_CODE).expect("Orion C36 has code 20 (Restoring)");
        assert!(is_connecting(&Error::Client(ClientError::Refused(
            restoring
        ))));
        assert!(is_connecting(&Error::Topic(TopicError::Client(
            ClientError::Refused(restoring)
        ))));
        assert!(!is_connecting(&Error::Client(ClientError::Refused(
            Refusal::NotFound
        ))));
    }

    /// A refusal with no code this build knows is not connecting: the arm matches only the code it names.
    #[test]
    fn a_refusal_with_another_code_is_not_connecting() {
        let denied = Error::Client(ClientError::Refused(Refusal::Denied));
        assert!(!is_connecting(&denied));
        assert_eq!(Refusal::Unavailable.code(), 14);
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
    fn every_refusal_and_loss_is_a_named_state_in_the_log() {
        assert_eq!(
            refusal("not available at /run/orion/orion.sock: refused"),
            "orion: refused: not available at /run/orion/orion.sock: refused"
        );
        assert_eq!(
            lost("the connection was lost"),
            "orion: lost: the connection was lost; connecting again"
        );
    }

    #[test]
    fn a_long_reason_is_cut_at_a_character() {
        assert_eq!(cut("abc", 8), "abc");
        assert_eq!(cut("ab²c", 3), "ab");
        assert_eq!(cut("ab²c", 4), "ab²");
    }
}
