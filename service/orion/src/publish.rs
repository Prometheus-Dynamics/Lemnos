//! The Orion side: one session on the comm core, owned by this thread.
//!
//! Each device is a resource named `lemnos.<device>`, of the kind `lemnos.device`:
//!
//! - its labels are `lemnos.driver`, one `lemnos.tag` per tag, and `lemnos.control.<item>` for
//!   each control or action item, with the shapes it takes;
//! - its state is `Ready` while it runs, `Failed` while it retries or reports failed health,
//!   and `Pending` while it is switched off;
//! - the topic `lemnos.<device>.values` has a field per value item that is a number (a float,
//!   in the item's unit, scaled by its exponent) or a flag (a bool), published as it changes;
//! - the topic `lemnos.<device>.status` has the fields `state`, `health` and `reason`, as
//!   `lemnos-ctl list` names them, published as they change;
//! - the actions `set`, `call` and `power` (see [`crate::calls`]) drive it.
//!
//! The board's facts (`hello.<fact>`, see [`crate::hello`]) are topics of the same session, one
//! each, with a text field `value`. A changed value is published as it changes; a fact that
//! appears or goes away changes the set of topics, which only a new session can do, so the
//! session is published again then.
//!
//! The session lives as long as one lemnosd device list. When lemnosd goes away or its list
//! changes, every status topic says `gone`, every resource is withdrawn (its record deleted)
//! and the session ends, which is how
//! Orion tells every subscriber of its topics that their publisher is gone; Orion has no way
//! to take back a single topic. When Orion goes away, the session is connected again, and
//! every topic, action and resource is said again.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use orion::actions::{Admits, Outcome, Policy, Request, Target};
use orion::provider::{Provider, Resource, ResourceState};
use orion::topics::{Field, FieldId, FieldType, Publisher, SampleTime, TopicDecl, TopicError};
use orion::types::Error as Refusal;
use orion::{ClientError, Error, Options, Orion, Roles};
use rustix::event::{PollFd, PollFlags, Timespec, poll};

use lemnos_client::{DeviceHealth, DeviceState};

use crate::backoff::Backoff;
use crate::calls::{Done, Job, Message, Progress, Verb};
use crate::config::Config;
use crate::hello;
use crate::mirror::{Changes, DeviceSpec, Status, health_name, state_name};
use crate::values::{Plain, Reading};
use crate::{CLIENT_NAME, RESOURCE_KIND, RESOURCE_PREFIX, Shared, log};

/// Who may run a device's actions: programs on this board, and operators granted it.
const POLICY: Policy = Policy::new(Admits::LOCAL.and(Admits::OPERATOR));
/// Samples of each field a topic keeps.
const HISTORY: u32 = 16;
/// The longest status text published. A longer reason is cut at a character boundary.
const STATUS_MAX: usize = 256;
/// How long to wait with no session for lemnosd to have devices.
const IDLE: Duration = Duration::from_secs(1);
/// How soon a resource state Orion refused is written again.
const RETRY: Duration = Duration::from_secs(1);

/// The state a status topic says when its device is withdrawn.
const GONE: &str = "gone";
/// Samples of a fact's topic that are kept: its newest value is what the hello document carries.
const FACT_HISTORY: u32 = 2;

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

/// Publishes what lemnosd has, for as long as the process runs or until `shared` is stopped.
/// Runs of the devices' actions go to `jobs`, and come back through `shared`.
pub fn run(config: &Config, shared: &Shared, jobs: &Sender<Message>) {
    let mut backoff = Backoff::new();
    let mut number = 0u64;
    let mut changes = Changes::default();
    let mut done = Vec::new();
    while !shared.stopping() {
        let (generation, specs, facts) = shared.with(|mirror| mirror.snapshot());
        if specs.is_empty() {
            // Nothing to publish, so no session: wait for lemnosd's devices.
            wait_bell(shared, IDLE);
            shared.take_done(&mut done);
            done.clear();
            shared.take_progress(&mut Vec::new());
            continue;
        }
        number += 1;
        let mut session = match Session::open(config, number, generation, &specs, &facts) {
            Ok(session) => session,
            Err(message) => {
                log(&message);
                wait_bell(shared, backoff.next_delay());
                continue;
            }
        };
        log(&format!(
            "connected to Orion node {}; {} devices published",
            session.node,
            session.devices.iter().flatten().count()
        ));
        backoff.reset();
        shared.with(|mirror| mirror.mark_all());
        match session.serve(shared, jobs, &mut changes, &mut done) {
            End::NewList => {
                let why = if shared.with(|mirror| mirror.is_empty()) {
                    "lemnosd is not there"
                } else {
                    "lemnosd's device list changed"
                };
                session.withdraw(why);
                log(&format!("{why}; withdrew the devices from Orion"));
            }
            // The board's facts changed in a way a session cannot say: its topics are said again.
            End::Facts => {
                session.withdraw("the board's facts changed");
                log("the board's facts changed; the session is published again");
            }
            End::Stopped => session.withdraw("lemnos-orion stopped"),
            End::Lost(message) => {
                log(&message);
                drop(session);
                wait_bell(shared, backoff.next_delay());
            }
        }
    }
}

/// Why a session ended.
enum End {
    /// lemnosd went away, or came back with a list of its own.
    NewList,
    /// The set of the board's `hello.*` topics changed, which only a new session can do.
    Facts,
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
    name: String,
    values: Option<Publisher>,
    status: Publisher,
    /// The state the record says, and the one it should say.
    written: ResourceState,
    wanted: ResourceState,
}

struct Session {
    orion: Orion,
    node: String,
    number: u64,
    generation: u64,
    /// By position in the device list. `None` for a device Orion would not take.
    devices: Vec<Option<Published>>,
    /// Full action name to the device's position and what the action does.
    actions: HashMap<String, (usize, Verb)>,
    /// Runs handed to the call thread and not yet answered.
    pending: HashSet<u64>,
    /// When a refused resource state is written again.
    retry_at: Option<Instant>,
    /// The board's `hello.*` topics, in the order the facts came.
    facts: Vec<Fact>,
    /// The set of facts changed: the session is published again.
    facts_stale: bool,
}

/// Why a device could not be described.
enum Refused {
    /// Orion will not take this device: its name, or a name another program holds.
    Device(String),
    /// The session cannot go on.
    Session(String),
}

impl Session {
    /// Connects to the comm core and describes every device. It works only when the node the
    /// comm core welcomes this session from is the one `ORION_NODE_ID` names, if it names one.
    fn open(
        config: &Config,
        number: u64,
        generation: u64,
        specs: &[DeviceSpec],
        facts: &[(String, String)],
    ) -> Result<Self, String> {
        let options = Options {
            roles: Roles::CLIENT.and(Roles::PROVIDER),
            ..Options::default()
        };
        let orion = Orion::connect_with(&config.orion, CLIENT_NAME, options)
            .map_err(|error| format!("Orion not available at {}: {error}", config.orion))?;
        let node = orion.welcomed().node.as_str().to_owned();
        check_node(&node, config.node_id.as_deref())?;
        let provider = orion.provider();
        let mut devices = Vec::with_capacity(specs.len());
        let mut actions = HashMap::new();
        for (position, spec) in specs.iter().enumerate() {
            match describe(&provider, spec) {
                Ok((published, verbs)) => {
                    for verb in verbs {
                        actions.insert(
                            format!("{}.{}", published.resource.name().as_str(), verb.name()),
                            (position, verb),
                        );
                    }
                    devices.push(Some(published));
                }
                Err(Refused::Device(why)) => {
                    log(&format!("{}: not published: {why}", spec.name));
                    devices.push(None);
                }
                Err(Refused::Session(why)) => {
                    return Err(format!("Orion did not take {}: {why}", spec.name));
                }
            }
        }
        let facts = declare_facts(&orion, facts)?;
        Ok(Self {
            orion,
            node,
            number,
            generation,
            devices,
            actions,
            pending: HashSet::new(),
            retry_at: None,
            facts,
            facts_stale: false,
        })
    }

    /// Serves the session until the device list changes, Orion is lost for good, or the
    /// service stops.
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
                    if changes.generation == self.generation {
                        self.publish(changes)
                    } else {
                        Ok(())
                    }
                })
                .and_then(|()| self.retry_states());
            if changes.generation != self.generation {
                return End::NewList;
            }
            if let Err(error) = turned {
                if !error.is_lost() {
                    log(&format!("Orion refused: {error}"));
                } else if let Err(end) = self.reconnect(shared, &error) {
                    return end;
                }
                continue;
            }
            if self.facts_stale {
                return End::Facts;
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
            let (call, name, target, args) = match request {
                Request::Run {
                    call,
                    name,
                    target,
                    args,
                    ..
                } => (call, name, target, args),
                // A long job is cancelled; a plain call cannot be stopped part-way, so it ends as
                // the call does.
                Request::Cancel { call } => {
                    if self.pending.contains(&call) {
                        let _ = jobs.send(Message::Cancel {
                            session: self.number,
                            call,
                        });
                    }
                    continue;
                }
            };
            let job = self.job(call, &name, &target, args);
            match job {
                Ok(job) => {
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

    /// Where runs of every action of this session arrive: one queue, whichever resource
    /// registered the action.
    fn requests(&mut self) -> Option<&mut orion::actions::Registration> {
        self.devices
            .iter_mut()
            .flatten()
            .find_map(|device| device.resource.requests())
    }

    /// The job for a run, or why it is refused.
    fn job(&self, call: u64, name: &str, target: &Target, args: Vec<u8>) -> Result<Job, String> {
        let &(position, verb) = self
            .actions
            .get(name)
            .ok_or_else(|| format!("no action {name:?} here"))?;
        let device = self.devices[position]
            .as_ref()
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
            call,
            device: device.name.clone(),
            verb,
            args,
        })
    }

    /// Publishes what changed.
    fn publish(&mut self, changes: &Changes) -> Result<(), Error> {
        if let Some(facts) = &changes.hello {
            self.publish_facts(facts)?;
            if self.facts_stale {
                return Ok(());
            }
        }
        let time = now();
        for (position, status) in &changes.status {
            let Some(Some(device)) = self.devices.get_mut(*position) else {
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
                        log(&format!("{}: state not written: {error}", device.name));
                        self.retry_at.get_or_insert_with(|| Instant::now() + RETRY);
                    }
                }
            }
        }
        for &(position, item, reading) in &changes.readings {
            let Some(Some(device)) = self.devices.get_mut(position) else {
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
            sample(values, item, time, payload)?;
        }
        Ok(())
    }

    /// Publishes each fact whose value changed. A fact that appeared or went away changes the set
    /// of topics: the session is published again instead.
    fn publish_facts(&mut self, facts: &[(String, String)]) -> Result<(), Error> {
        let same_set = facts.len() == self.facts.len()
            && facts
                .iter()
                .all(|(name, _)| self.facts.iter().any(|fact| fact.name == *name));
        if !same_set {
            self.facts_stale = true;
            return Ok(());
        }
        for (name, value) in facts {
            let Some(fact) = self.facts.iter_mut().find(|fact| fact.name == *name) else {
                continue;
            };
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

    /// Writes again the resource states Orion refused, once their time has come.
    fn retry_states(&mut self) -> Result<(), Error> {
        if self.retry_at.is_none_or(|at| Instant::now() < at) {
            return Ok(());
        }
        self.retry_at = None;
        for device in self.devices.iter_mut().flatten() {
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

    /// Connects again after the comm core went away, saying every topic, action and resource
    /// again. Gives up when the device list changes meanwhile, or the comm core now refuses
    /// what it took before.
    fn reconnect(&mut self, shared: &Shared, error: &Error) -> Result<(), End> {
        log(&format!("Orion lost ({error}); connecting again"));
        let generation = self.generation;
        self.orion
            .reconnect(|_| !shared.stopping() && shared.with(|m| m.generation()) == generation)
            .map_err(|error| End::Lost(format!("Orion did not come back: {error}")))?;
        let node = self.orion.welcomed().node.as_str().to_owned();
        if node != self.node {
            return Err(End::Lost(format!(
                "Orion came back as node {node:?}, not {:?}",
                self.node
            )));
        }
        // What was asked of this service before belongs to the session that is gone.
        self.pending.clear();
        self.number += 1;
        shared.with(|mirror| mirror.mark_all());
        // The comm core has none of the facts' last values: each is said again.
        for fact in &mut self.facts {
            fact.value = None;
        }
        shared.with(|mirror| mirror.mark_hello());
        log(&format!("connected to Orion node {node} again"));
        Ok(())
    }

    /// Waits for the comm core, a wake-up from the other threads, a ping falling due or a
    /// refused state's retry, whichever is first.
    fn wait(&self, shared: &Shared) {
        let heartbeat = self.orion.welcomed().heartbeat;
        let mut wait = (heartbeat / 2).max(Duration::from_millis(10));
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

    /// Takes every resource away: their records are deleted. Each status topic says `gone`
    /// first, with `why`, so a reader that still holds its ring never reads a state from
    /// before. The session ends when this is dropped, and with it the topics and actions:
    /// their subscribers are told the publisher is gone.
    fn withdraw(mut self, why: &str) {
        let time = now();
        for device in self.devices.iter_mut().flatten() {
            for (field, text) in [GONE, "", why].iter().enumerate() {
                if sample(&mut device.status, field, time, text.as_bytes()).is_err() {
                    break;
                }
            }
        }
        for device in self.devices.into_iter().flatten() {
            if let Err(error) = device.resource.withdraw() {
                log(&format!("{}: not withdrawn: {error}", device.name));
            }
        }
    }
}

/// Declares each `hello.<fact>` topic and says its value. A fact another program already
/// publishes, or one Orion will not take, is logged and left out; a lost connection ends the
/// session.
fn declare_facts(orion: &Orion, facts: &[(String, String)]) -> Result<Vec<Fact>, String> {
    let mut declared = Vec::with_capacity(facts.len());
    for (name, value) in facts {
        let topic = format!("{}{name}", hello::TOPIC_PREFIX);
        let fields = [text_field("value")];
        let decl = TopicDecl {
            name: &topic,
            fields: &fields,
            history: FACT_HISTORY,
            expiry_ms: 0,
            max_payload: hello::MAX_VALUE_BYTES as u32,
        };
        match orion.declare(&decl).map_err(fact_refused) {
            Ok(mut publisher) => {
                sample(&mut publisher, 0, now(), value.as_bytes()).map_err(|e| e.to_string())?;
                declared.push(Fact {
                    name: name.clone(),
                    publisher: Some(publisher),
                    value: Some(value.clone()),
                });
            }
            Err(Refused::Device(why)) => {
                log(&format!("{topic}: not published: {why}"));
                declared.push(Fact {
                    name: name.clone(),
                    publisher: None,
                    value: None,
                });
            }
            Err(Refused::Session(why)) => {
                return Err(format!("Orion did not take {topic}: {why}"));
            }
        }
    }
    Ok(declared)
}

fn fact_refused(error: Error) -> Refused {
    match error {
        Error::Topic(error) => topic_refused(error),
        Error::Client(error) => client_refused(error),
    }
}

/// Describes one device to Orion: its resource, its topics and its actions.
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
                .topic("values", &fields, HISTORY, 8)
                .map_err(topic_refused)?,
        )
    };
    let status = describing
        .topic("status", &STATUS_FIELDS, HISTORY, STATUS_MAX as u32)
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
            name: spec.name.clone(),
            values,
            status,
            written: state,
            wanted: state,
        },
        verbs,
    ))
}

fn topic_refused(error: TopicError) -> Refused {
    match error {
        TopicError::Invalid(why) => Refused::Device(why),
        TopicError::Client(error) => client_refused(error),
        other => Refused::Session(other.to_string()),
    }
}

/// A name another program holds, or one Orion will not take, is this device's problem; any
/// other refusal or a lost connection ends the session.
fn client_refused(error: ClientError) -> Refused {
    match error {
        ClientError::Refused(
            why @ (Refusal::AlreadyExists
            | Refusal::InvalidName
            | Refusal::NameTooLong
            | Refusal::EmptyName),
        ) => Refused::Device(why.as_str().to_owned()),
        ClientError::Protocol(why) => Refused::Device(why.to_owned()),
        other => Refused::Session(other.to_string()),
    }
}

/// One sample of field `field`.
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

/// The time a sample belongs to: now, on the wall clock, as Orion's own publishers stamp them.
fn now() -> SampleTime {
    let since = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    SampleTime::from_nanos(u64::try_from(since.as_nanos()).unwrap_or(u64::MAX))
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
    fn a_long_reason_is_cut_at_a_character() {
        assert_eq!(cut("abc", 8), "abc");
        assert_eq!(cut("ab²c", 3), "ab");
        assert_eq!(cut("ab²c", 4), "ab²");
    }
}
