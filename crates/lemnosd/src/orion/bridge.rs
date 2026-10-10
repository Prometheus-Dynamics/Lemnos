//! The bridge's runtime: lemnosd's devices as Orion resources (`docs/orion.md`).
//!
//! Three kinds of work meet here:
//!
//! - a thread holds one lemnosd connection, named `orion`, subscribes to the
//!   readings and forwards them (`pump`); it is blocking, as lemnosd's client is;
//! - the async side runs the Orion provider: it registers, publishes the
//!   resources and status, and takes action requests from a watch task;
//! - each caller of an action gets its own lemnosd connection, named
//!   `orion:<requested_by>`, because writes end with the writer's connection.
//!
//! The main loop owns the Orion session and the mirror of what has been
//! published. A session that fails is dropped and reconnected on a timer,
//! and the resources and status are published again on connect and on every
//! heartbeat, so an Orion restart is repaired within one heartbeat.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lemnos_ipc::{ClientEvent, ClientOptions, DeviceClient, DeviceDesc, Event, Update};
use orion_client::{ActionReporter, LocalNodeRuntime, LocalProviderApp, LocalProviderService};
use orion_control_plane::{
    ActionRequest, ActionTarget, ProviderRecord, ResourceRecord, StatusEntry, TypedConfigValue,
};
use orion_core::{NodeId, ProviderId, ResourceType};
use tokio::sync::mpsc;

use super::mirror::{Cadence, Mirror, PROVIDER_ID, RESOURCE_TYPE};
use super::ops::{self, Op, Outcome};

/// The lemnosd client name of the bridge's readings connection.
pub const LEMNOSD_CLIENT: &str = "orion";
/// The Orion client name (its provider and action handler identity).
pub const ORION_CLIENT: &str = "lemnos-orion";

/// Where and how the bridge runs.
#[derive(Debug, Clone)]
pub struct Config {
    /// lemnosd's socket.
    pub lemnosd_socket: PathBuf,
    /// Orion's local unary IPC socket.
    pub orion_socket: PathBuf,
    /// Orion's local stream IPC socket.
    pub orion_stream: PathBuf,
    /// The node the provider runs on (`ORION_NODE_ID`).
    pub node_id: String,
    /// The board definition, for the `lemnos.driver` labels (optional).
    pub board_file: Option<PathBuf>,
    /// Readings per device per second at most (`LEMNOS_ORION_RATE_HZ`).
    pub rate_hz: f64,
    /// Every status entry is republished this often (30 s).
    pub heartbeat: Duration,
    /// The TTL of every status entry (90 s).
    pub ttl: Duration,
    /// How long to wait between connection attempts to lemnosd or Orion.
    pub retry: Duration,
    /// Devices whose readings are subscribed to only for some channels
    /// (`LEMNOS_ORION_CHANNELS`, for example `imu=angular_rate.z`): the
    /// device's other channels are not read. Other devices are whole.
    pub channels: Vec<(String, Vec<String>)>,
}

impl Config {
    /// The bridge's environment: `LEMNOSD_SOCKET`, `LEMNOSD_BOARD`,
    /// `LEMNOS_ORION_RATE_HZ`, `ORION_NODE_ID` and Orion's own socket variables
    /// (`ORION_NODE_IPC_SOCKET`, `ORION_NODE_IPC_STREAM_SOCKET`).
    pub fn from_env() -> Result<Self, String> {
        let rate_hz = match std::env::var("LEMNOS_ORION_RATE_HZ") {
            Ok(text) => text
                .trim()
                .parse::<f64>()
                .ok()
                .filter(|hz| hz.is_finite() && *hz > 0.0 && *hz <= 50.0)
                .ok_or_else(|| {
                    format!("LEMNOS_ORION_RATE_HZ `{text}`: expected a rate in (0, 50]")
                })?,
            Err(_) => 2.0,
        };
        let node_id = std::env::var("ORION_NODE_ID")
            .ok()
            .filter(|id| !id.trim().is_empty())
            .unwrap_or_else(|| "node.local".to_owned());
        Ok(Self {
            lemnosd_socket: std::env::var_os("LEMNOSD_SOCKET")
                .map_or_else(|| PathBuf::from(crate::DEFAULT_SOCKET), PathBuf::from),
            orion_socket: orion_client::default_ipc_socket_path(),
            orion_stream: orion_client::default_ipc_stream_socket_path(),
            node_id,
            board_file: Some(
                std::env::var_os("LEMNOSD_BOARD")
                    .map_or_else(|| PathBuf::from(crate::DEFAULT_BOARD), PathBuf::from),
            ),
            rate_hz,
            heartbeat: Duration::from_secs(30),
            ttl: Duration::from_secs(90),
            retry: Duration::from_secs(1),
            channels: parse_channels(&std::env::var("LEMNOS_ORION_CHANNELS").unwrap_or_default()),
        })
    }

    fn cadence(&self) -> Cadence {
        Cadence::new(self.rate_hz, self.heartbeat, self.ttl)
    }
}

/// What the lemnosd pump thread sends the async side.
enum Feed {
    /// Connected (or reconnected): the board, its devices, and the subscriptions
    /// are in place.
    Connected {
        board: String,
        devices: Vec<DeviceDesc>,
    },
    /// A control's value, read at connection.
    Control {
        device: String,
        control: String,
        value: f64,
    },
    /// A reading or an event from lemnosd.
    Update(Update),
    /// A calibration status, polled from lemnosd (see `poll_calibration`).
    Calibration {
        device: String,
        status: lemnos_device::CalibrationStatus,
    },
}

/// What the watch tasks send the main loop.
enum Inbound {
    /// An action request for a session's provider.
    Request {
        request: Box<ActionRequest>,
        reporter: ActionReporter,
    },
    /// The session's watch ended (Orion went away).
    Lost { generation: u64, reason: String },
}

/// Connections the action handlers share.
struct Shared {
    lemnosd_socket: PathBuf,
    /// One lemnosd connection per caller, by its client name.
    callers: Mutex<HashMap<String, Arc<Mutex<DeviceClient>>>>,
    /// The mirror (the main loop updates it; actions read the resource ids).
    mirror: Mutex<Option<Mirror>>,
    /// Feeds the main loop (control values read after an action).
    feed: mpsc::UnboundedSender<Feed>,
}

/// One Orion session: the registered provider, which publishes the resources
/// and their status. Action results go through the watch's reporter.
struct Session {
    generation: u64,
    provider: LocalProviderApp,
}

/// Runs the bridge until the process ends. Errors are start-up failures only;
/// lost connections are retried.
pub async fn run(config: Config) -> Result<(), String> {
    let drivers = drivers(config.board_file.as_deref());
    let cadence = config.cadence();
    let (feed_tx, mut feed_rx) = mpsc::unbounded_channel();
    let shared = Arc::new(Shared {
        lemnosd_socket: config.lemnosd_socket.clone(),
        callers: Mutex::new(HashMap::new()),
        mirror: Mutex::new(None),
        feed: feed_tx.clone(),
    });
    let pump_socket = config.lemnosd_socket.clone();
    let pump_retry = config.retry;
    let pump_channels = config.channels.clone();
    let period_ms = u32::try_from(cadence.min_interval.as_millis()).unwrap_or(u32::MAX);
    std::thread::spawn(move || {
        pump(
            &pump_socket,
            period_ms,
            pump_retry,
            &feed_tx,
            &pump_channels,
        );
    });
    let (inbound_tx, mut inbound_rx) = mpsc::unbounded_channel::<Inbound>();
    let started = Instant::now();
    let mut session: Option<Session> = None;
    let mut generation = 0u64;
    let mut connect = tokio::time::interval(config.retry);
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    let mut last_resources = Instant::now();
    loop {
        tokio::select! {
            Some(feed) = feed_rx.recv() => {
                let devices_changed = matches!(feed, Feed::Connected { .. });
                let entries = apply_feed(&shared, feed, &drivers, cadence, started.elapsed());
                let mut lost = false;
                if let Some(current) = session.as_mut() {
                    if devices_changed {
                        let resources = resources(&shared);
                        lost |= current.provider.publish_resources(resources).await.is_err();
                    }
                    if !entries.is_empty() {
                        lost |= current.provider.publish_status(entries).await.is_err();
                    }
                }
                if lost {
                    log("Orion lost while publishing; reconnecting");
                    session = None;
                }
            }
            Some(inbound) = inbound_rx.recv() => match inbound {
                Inbound::Request { request, reporter } => {
                    // A request from a session that ended still runs: its reporter
                    // fails, and the action's owner sees the result.
                    let shared = shared.clone();
                    tokio::spawn(async move {
                        let id = request.action_id.clone();
                        let outcome = handle(&shared, *request).await;
                        report(&reporter, id, outcome).await;
                    });
                }
                Inbound::Lost { generation: from, reason } => {
                    if session.as_ref().is_some_and(|s| s.generation == from) {
                        log(&format!("Orion session ended: {reason}"));
                        session = None;
                    }
                }
            },
            _ = connect.tick(), if session.is_none() => {
                generation += 1;
                match connect_session(&config, generation, inbound_tx.clone()).await {
                    Ok(new) => match publish_all(&shared, &new, started.elapsed()).await {
                        Ok(()) => {
                            log(&format!("connected to Orion as {ORION_CLIENT}"));
                            last_resources = Instant::now();
                            session = Some(new);
                        }
                        Err(error) => log(&format!("Orion publish failed: {error}")),
                    },
                    Err(error) => log(&format!("Orion not available: {error}")),
                }
            }
            _ = tick.tick() => {
                if let Some(current) = session.as_mut() {
                    let mut lost = false;
                    let heartbeat = take_heartbeat(&shared, started.elapsed());
                    if !heartbeat.is_empty() {
                        lost |= current.provider.publish_status(heartbeat).await.is_err();
                    }
                    if last_resources.elapsed() >= config.heartbeat {
                        last_resources = Instant::now();
                        lost |= current.provider.publish_resources(resources(&shared)).await.is_err();
                    }
                    if lost {
                        log("Orion lost during the heartbeat; reconnecting");
                        session = None;
                    }
                }
            }
        }
    }
}

/// The lemnosd thread: one connection, readings forwarded, reconnected on
/// failure. Ends when the bridge does.
#[allow(clippy::print_stderr)]
fn pump(
    socket: &Path,
    period_ms: u32,
    retry: Duration,
    feed: &mpsc::UnboundedSender<Feed>,
    channels: &[(String, Vec<String>)],
) {
    loop {
        if feed.is_closed() {
            return;
        }
        let mut client = match ClientOptions::new(socket, LEMNOSD_CLIENT)
            .events(true)
            .reconnecting()
            .devices()
        {
            Ok(client) => client,
            Err(error) => {
                eprintln!("lemnos-orion: lemnosd not available: {error}");
                std::thread::sleep(retry);
                continue;
            }
        };
        if !setup(&mut client, period_ms, feed, channels) {
            std::thread::sleep(retry);
            continue;
        }
        let mut next_calibration = Instant::now();
        loop {
            if feed.is_closed() {
                return;
            }
            if Instant::now() >= next_calibration {
                next_calibration = Instant::now() + CALIBRATION_POLL;
                if !poll_calibration(&mut client, feed) {
                    return;
                }
            }
            match client.next_event_timeout(Duration::from_millis(200)) {
                Ok(None) => {}
                Ok(Some(ClientEvent::Connected { reconnects })) => {
                    if reconnects > 0 && !setup(&mut client, period_ms, feed, channels) {
                        break;
                    }
                }
                Ok(Some(ClientEvent::Disconnected { error })) => {
                    eprintln!("lemnos-orion: lemnosd connection lost: {error}");
                }
                Ok(Some(ClientEvent::Data(update))) => {
                    if feed.send(Feed::Update(update)).is_err() {
                        return;
                    }
                }
                Err(error) => {
                    eprintln!("lemnos-orion: lemnosd: {error}");
                    break;
                }
            }
        }
        std::thread::sleep(retry);
    }
}

/// How often the calibration status of the IMUs and magnetometers is read.
const CALIBRATION_POLL: Duration = Duration::from_secs(10);

/// Reads the calibration status of each IMU and magnetometer and feeds it to
/// the mirror. `false` when the bridge has ended.
fn poll_calibration(client: &mut DeviceClient, feed: &mpsc::UnboundedSender<Feed>) -> bool {
    let Ok(devices) = client.list() else {
        return true;
    };
    for device in devices.iter().filter(|d| {
        matches!(
            d.class,
            lemnos_device::DeviceClass::Imu | lemnos_device::DeviceClass::Magnetometer
        )
    }) {
        if let Ok(status) = client.calibration_status(&device.id)
            && feed
                .send(Feed::Calibration {
                    device: device.id.clone(),
                    status,
                })
                .is_err()
        {
            return false;
        }
    }
    true
}

/// `device=channel,channel;device=...` (`LEMNOS_ORION_CHANNELS`): each device
/// with the channel names to subscribe to (see `DeviceClient::subscribe_channels`).
pub fn parse_channels(text: &str) -> Vec<(String, Vec<String>)> {
    text.split(';')
        .filter_map(|entry| {
            let (device, names) = entry.split_once('=')?;
            let names: Vec<String> = names
                .split(',')
                .map(str::trim)
                .filter(|n| !n.is_empty())
                .map(str::to_owned)
                .collect();
            let device = device.trim();
            (!device.is_empty() && !names.is_empty()).then(|| (device.to_owned(), names))
        })
        .collect()
}

/// Lists the devices, subscribes to each and reads its controls. False when
/// lemnosd did not answer (the caller reconnects).
#[allow(clippy::print_stderr)]
fn setup(
    client: &mut DeviceClient,
    period_ms: u32,
    feed: &mpsc::UnboundedSender<Feed>,
    channels: &[(String, Vec<String>)],
) -> bool {
    let devices = match client.list() {
        Ok(devices) => devices,
        Err(error) => {
            eprintln!("lemnos-orion: lemnosd list: {error}");
            return false;
        }
    };
    if feed
        .send(Feed::Connected {
            board: client.board().to_owned(),
            devices: devices.clone(),
        })
        .is_err()
    {
        return true;
    }
    for device in &devices {
        let only = channels.iter().find(|(d, _)| *d == device.id);
        let subscribed = match only {
            Some((_, names)) => {
                let names: Vec<&str> = names.iter().map(String::as_str).collect();
                client.subscribe_channels(&device.id, &names, period_ms)
            }
            None => client.subscribe(&device.id, period_ms),
        };
        if let Err(error) = subscribed {
            eprintln!("lemnos-orion: subscribe {}: {error}", device.id);
        }
        for control in &device.controls {
            if let Ok(value) = client.get(&device.id, &control.name) {
                let _ = feed.send(Feed::Control {
                    device: device.id.clone(),
                    control: control.name.clone(),
                    value,
                });
            }
        }
    }
    true
}

/// Applies one feed message to the mirror; the status entries to publish.
fn apply_feed(
    shared: &Shared,
    feed: Feed,
    drivers: &BTreeMap<String, String>,
    cadence: Cadence,
    now: Duration,
) -> Vec<StatusEntry> {
    let mut slot = shared.mirror.lock().unwrap_or_else(|e| e.into_inner());
    match feed {
        Feed::Connected { board, devices } => {
            match slot.as_mut() {
                Some(mirror) => mirror.reset(devices, drivers),
                None => *slot = Some(Mirror::new(&board, devices, drivers, cadence)),
            }
            Vec::new()
        }
        Feed::Control {
            device,
            control,
            value,
        } => slot
            .as_mut()
            .map(|mirror| mirror.control(&device, &control, value))
            .unwrap_or_default(),
        Feed::Calibration { device, status } => slot
            .as_mut()
            .map(|mirror| mirror.calibration(&device, &status))
            .unwrap_or_default(),
        Feed::Update(update) => match slot.as_mut() {
            None => Vec::new(),
            Some(mirror) => match update {
                Update::Reading(reading) => mirror.reading(
                    &reading.device,
                    reading.status,
                    reading.timestamp_us,
                    &reading.raw,
                    now,
                ),
                Update::Event(Event::Status {
                    device,
                    status,
                    reason,
                    ..
                }) => mirror.status(&device, status, &reason),
                Update::Event(Event::Control {
                    device,
                    control,
                    value,
                    ..
                }) => mirror.control(&device, &control, value),
                Update::Event(_) => Vec::new(),
            },
        },
    }
}

/// The resources, from the mirror.
fn resources(shared: &Shared) -> Vec<ResourceRecord> {
    shared
        .mirror
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map(Mirror::resources)
        .unwrap_or_default()
}

/// The heartbeat entries that are due.
fn take_heartbeat(shared: &Shared, now: Duration) -> Vec<StatusEntry> {
    shared
        .mirror
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_mut()
        .map(|mirror| mirror.heartbeat(now))
        .unwrap_or_default()
}

/// Publishes the resources and every value the mirror knows, to a new session.
async fn publish_all(
    shared: &Shared,
    session: &Session,
    now: Duration,
) -> Result<(), orion_client::ClientError> {
    let (resources, snapshot) = {
        let mut slot = shared.mirror.lock().unwrap_or_else(|e| e.into_inner());
        match slot.as_mut() {
            Some(mirror) => (mirror.resources(), mirror.snapshot(now)),
            None => (Vec::new(), Vec::new()),
        }
    };
    if !resources.is_empty() {
        session.provider.publish_resources(resources).await?;
    }
    if !snapshot.is_empty() {
        session.provider.publish_status(snapshot).await?;
    }
    Ok(())
}

/// Connects Orion: registers the provider and watches its action requests.
async fn connect_session(
    config: &Config,
    generation: u64,
    inbound: mpsc::UnboundedSender<Inbound>,
) -> Result<Session, String> {
    let runtime = LocalNodeRuntime::new(&config.orion_socket, &config.orion_stream);
    let provider = ProviderRecord::builder(
        ProviderId::new(PROVIDER_ID),
        NodeId::new(config.node_id.clone()),
    )
    .resource_type(ResourceType::new(RESOURCE_TYPE))
    .build();
    let service = LocalProviderService::new(runtime.clone(), ORION_CLIENT, provider.clone());
    service.register().await.map_err(|e| e.to_string())?;
    let provider_client = runtime
        .provider(ORION_CLIENT.to_owned(), provider)
        .map_err(|e| e.to_string())?;
    let mut watch = service
        .watch_action_requests()
        .await
        .map_err(|e| e.to_string())?;
    let watch_reporter = watch.reporter();
    tokio::spawn(async move {
        loop {
            match watch.next().await {
                Ok(request) => {
                    let sent = inbound.send(Inbound::Request {
                        request: Box::new(request),
                        reporter: watch_reporter.clone(),
                    });
                    if sent.is_err() {
                        return;
                    }
                }
                Err(error) => {
                    let _ = inbound.send(Inbound::Lost {
                        generation,
                        reason: error.to_string(),
                    });
                    return;
                }
            }
        }
    });
    Ok(Session {
        generation,
        provider: provider_client,
    })
}

/// Runs one action on the resource's device, on the caller's connection.
async fn handle(shared: &Arc<Shared>, request: ActionRequest) -> Outcome {
    let ActionTarget::Resource(resource) = &request.target else {
        return Outcome::Rejected("only resources take lemnos actions".to_owned());
    };
    let device = {
        let slot = shared.mirror.lock().unwrap_or_else(|e| e.into_inner());
        match slot
            .as_ref()
            .and_then(|m| m.device_of_resource(resource.as_str()))
        {
            Some(device) => device,
            None => return Outcome::Rejected(format!("no lemnos device `{resource}`")),
        }
    };
    let op = match ops::parse(&request.name, &request.args) {
        Ok(op) => op,
        Err(reason) => return Outcome::Rejected(reason),
    };
    let caller = format!("orion:{}", request.requested_by);
    let controls = {
        let slot = shared.mirror.lock().unwrap_or_else(|e| e.into_inner());
        slot.as_ref()
            .map(|m| m.control_names(&device))
            .unwrap_or_default()
    };
    let shared_for_block = shared.clone();
    let device_for_block = device.clone();
    let (outcome, values) = tokio::task::spawn_blocking(move || {
        let client = match caller_client(&shared_for_block, &caller) {
            Ok(client) => client,
            Err(error) => return (Outcome::Failed(format!("lemnosd: {error}")), Vec::new()),
        };
        let mut client = client.lock().unwrap_or_else(|e| e.into_inner());
        let outcome = run_op(&mut client, &device_for_block, op);
        // lemnosd's restore and release do not name the control they changed,
        // so the controls are read back after every action.
        let values = controls
            .iter()
            .filter_map(|name| {
                client
                    .get(&device_for_block, name)
                    .ok()
                    .map(|value| (name.clone(), value))
            })
            .collect::<Vec<_>>();
        (outcome, values)
    })
    .await
    .unwrap_or_else(|error| {
        (
            Outcome::Failed(format!("action panicked: {error}")),
            Vec::new(),
        )
    });
    for (control, value) in values {
        let _ = shared.feed.send(Feed::Control {
            device: device.clone(),
            control,
            value,
        });
    }
    outcome
}

/// The caller's connection, created on first use.
fn caller_client(shared: &Shared, caller: &str) -> Result<Arc<Mutex<DeviceClient>>, String> {
    let mut callers = shared.callers.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(client) = callers.get(caller) {
        return Ok(client.clone());
    }
    let client = ClientOptions::new(&shared.lemnosd_socket, caller)
        .reconnecting()
        .devices()
        .map_err(|e| e.to_string())?;
    let client = Arc::new(Mutex::new(client));
    callers.insert(caller.to_owned(), client.clone());
    Ok(client)
}

/// One operation on lemnosd.
fn run_op(client: &mut DeviceClient, device: &str, op: Op) -> Outcome {
    match op {
        Op::Set { control, value } => match client.set(device, &control, value) {
            Ok(applied) => Outcome::Succeeded(ops::set_output(&control, applied)),
            Err(error) => ops::client_error(error),
        },
        Op::Restore { control } => match client.restore(device, control.as_deref()) {
            Ok(()) => Outcome::Succeeded(BTreeMap::new()),
            Err(error) => ops::client_error(error),
        },
        Op::Release => match client.release(device) {
            Ok(()) => Outcome::Succeeded(BTreeMap::new()),
            Err(error) => ops::client_error(error),
        },
        Op::Calibration(ops::CalibrationOp::Command(command)) => {
            match client.calibration(device, command) {
                Ok(()) => Outcome::Succeeded(BTreeMap::new()),
                Err(error) => ops::client_error(error),
            }
        }
        Op::Calibration(ops::CalibrationOp::Status) => match client.calibration_status(device) {
            Ok(status) => Outcome::Succeeded(ops::calibration_output(&status)),
            Err(error) => ops::client_error(error),
        },
        Op::Read => match client.read(device) {
            Ok(reading) => {
                let mut output = BTreeMap::new();
                output.insert(
                    "status".to_owned(),
                    TypedConfigValue::String(super::mirror::status_name(reading.status).to_owned()),
                );
                output.insert(
                    "read_us".to_owned(),
                    TypedConfigValue::UInt(reading.timestamp_us),
                );
                for (name, value) in reading.values() {
                    if let Some(value) = value {
                        output.insert(name.to_owned(), TypedConfigValue::F64(value));
                    }
                }
                Outcome::Succeeded(output)
            }
            Err(error) => ops::client_error(error),
        },
    }
}

/// Reports an action's outcome to Orion. A failed report (Orion went away)
/// is dropped: the node fails the action when its handler disconnects.
async fn report(reporter: &ActionReporter, id: String, outcome: Outcome) {
    let _ = match outcome {
        Outcome::Succeeded(output) => reporter.succeed(id, output).await,
        Outcome::Failed(reason) => reporter.fail(id, reason).await,
        Outcome::Rejected(reason) => reporter.reject(id, reason).await,
    };
}

/// The board's device drivers by device id (`lemnos.driver` labels); empty
/// when the board definition is missing or unreadable.
#[allow(clippy::print_stderr)]
fn drivers(path: Option<&std::path::Path>) -> BTreeMap<String, String> {
    let Some(path) = path else {
        return BTreeMap::new();
    };
    if !path.exists() {
        return BTreeMap::new();
    }
    match lemnos_board::BoardDefinition::from_path(path) {
        Ok(board) => board
            .devices
            .iter()
            .map(|spec| (spec.id.clone(), spec.driver.clone()))
            .collect(),
        Err(error) => {
            eprintln!(
                "lemnos-orion: {}: {error}; no driver labels",
                path.display()
            );
            BTreeMap::new()
        }
    }
}

#[allow(clippy::print_stderr)]
fn log(message: &str) {
    eprintln!("lemnos-orion: {message}");
}

#[cfg(test)]
mod channel_tests {
    use super::parse_channels;

    #[test]
    fn channel_selections_parse_per_device() {
        assert_eq!(
            parse_channels("imu=angular_rate.z, acceleration.* ;lights=;=x;fan=*"),
            vec![
                (
                    "imu".to_string(),
                    vec!["angular_rate.z".to_string(), "acceleration.*".to_string()]
                ),
                ("fan".to_string(), vec!["*".to_string()]),
            ]
        );
        assert!(parse_channels("").is_empty());
    }
}
