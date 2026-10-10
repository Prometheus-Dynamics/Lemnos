//! Clients of `lemnosd`: [`DeviceClient`] (devices, readings, controls) and
//! [`LedClient`] (LED intents). Both connect with a timeout, can reconnect in
//! the background, and report connection changes in order with what they
//! receive ([`ClientEvent`]), like Styx's `FrameClient` and `ControlClient`.

use crate::wire::{
    self, ChannelDesc, DeviceDesc, Event, LedRequest, Message, RawReading, Refusal, Request,
    VERSION, WireError,
};
use lemnos_device::{DeviceStatus, NO_VALUE};
use std::collections::VecDeque;
use std::fmt;
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[path = "client_led.rs"]
mod led;
#[path = "client_raw.rs"]
mod raw;
pub use led::{FrameUpdate, LedClient};
pub use raw::{I2cDevice, Line, Pwm, SpiDevice};

/// The default socket path.
pub const DEFAULT_SOCKET: &str = "/run/lemnos/lemnosd.sock";
/// How long requests wait for the service by default.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(2);
const RECONNECT_MIN: Duration = Duration::from_millis(100);
const RECONNECT_MAX: Duration = Duration::from_secs(5);

/// A failed client operation.
#[derive(Debug)]
pub enum ClientError {
    Io(io::Error),
    Protocol(WireError),
    /// The service refused the request.
    Refused(Refusal),
    /// No answer within the timeout.
    Timeout,
    /// The client is not connected (and does not reconnect).
    Closed,
    /// The service answered with a reason it could not do the request (a
    /// look that is unknown or invalid, a look file that cannot be saved).
    Rejected(String),
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "lemnosd connection: {e}"),
            Self::Protocol(e) => e.fmt(f),
            Self::Refused(r) => write!(f, "refused: {r}"),
            Self::Timeout => f.write_str("lemnosd did not answer in time"),
            Self::Closed => f.write_str("not connected to lemnosd"),
            Self::Rejected(reason) => write!(f, "lemnosd refused: {reason}"),
        }
    }
}

impl std::error::Error for ClientError {}

impl From<io::Error> for ClientError {
    fn from(e: io::Error) -> Self {
        if matches!(
            e.kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
        ) {
            Self::Timeout
        } else {
            Self::Io(e)
        }
    }
}

impl From<WireError> for ClientError {
    fn from(e: WireError) -> Self {
        Self::Protocol(e)
    }
}

/// A connection change, or what the client receives, in the order they
/// happened.
#[derive(Debug)]
pub enum ClientEvent<T> {
    /// Connected: the first connection (`reconnects: 0`) or a reconnection.
    /// Subscriptions and held LED intents were sent again.
    Connected {
        reconnects: u64,
    },
    /// The connection is gone; a reconnecting client tries again.
    Disconnected {
        error: ClientError,
    },
    Data(T),
}

/// A reading with its channel names and exponents.
#[derive(Debug, Clone, PartialEq)]
pub struct Reading {
    pub device: String,
    /// Microseconds on `CLOCK_BOOTTIME` when the read started (see
    /// [`wire`] for the contract).
    pub timestamp_us: u64,
    pub status: DeviceStatus,
    pub raw: Vec<i32>,
    channels: Arc<[ChannelDesc]>,
}

impl Reading {
    /// The channels, in order.
    pub fn channels(&self) -> &[ChannelDesc] {
        &self.channels
    }

    /// The value of channel `name` in its unit, `None` if absent or unread.
    pub fn value(&self, name: &str) -> Option<f64> {
        let index = self.channels.iter().position(|c| c.name == name)?;
        self.value_at(index)
    }

    /// The value of channel `index` in its unit.
    pub fn value_at(&self, index: usize) -> Option<f64> {
        let raw = *self.raw.get(index)?;
        let channel = self.channels.get(index)?;
        (raw != NO_VALUE).then(|| scaled(raw, channel.exponent))
    }

    /// `(name, value)` for every channel.
    pub fn values(&self) -> impl Iterator<Item = (&str, Option<f64>)> {
        (0..self.channels.len()).map(|i| (self.channels[i].name.as_str(), self.value_at(i)))
    }
}

fn scaled(raw: i32, exponent: i8) -> f64 {
    let scale = 10f64.powi(i32::from(exponent.unsigned_abs()));
    if exponent < 0 {
        f64::from(raw) / scale
    } else {
        f64::from(raw) * scale
    }
}

/// What a [`DeviceClient`] receives.
#[derive(Debug, Clone, PartialEq)]
pub enum Update {
    Reading(Reading),
    Event(Event),
}

/// How to connect.
#[derive(Debug, Clone)]
pub struct ClientOptions {
    path: PathBuf,
    name: String,
    priority: u8,
    timeout: Duration,
    reconnect: bool,
    keep: bool,
    /// `None`: the client type's default (devices yes, LEDs no).
    events: Option<bool>,
    wait: Option<Duration>,
}

impl ClientOptions {
    /// The service at `path`, as client `name`.
    pub fn new(path: impl AsRef<Path>, name: impl Into<String>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
            name: name.into(),
            priority: 50,
            timeout: DEFAULT_TIMEOUT,
            reconnect: false,
            keep: false,
            events: None,
            wait: None,
        }
    }

    /// Whether the service sends this client events (status, control and
    /// LED-owner changes, edges). Device clients get them by default, LED
    /// clients do not (an LED client that never reads them would otherwise
    /// fill its socket).
    pub fn events(mut self, events: bool) -> Self {
        self.events = Some(events);
        self
    }

    /// Keep retrying the first connection for up to `wait` (for clients that
    /// start before `lemnosd` is up). With [`reconnecting`](Self::reconnecting)
    /// and no wait, a first connection that fails is reported as
    /// `Disconnected` and retried in the background instead of failing.
    pub fn wait(mut self, wait: Duration) -> Self {
        self.wait = Some(wait);
        self
    }

    /// The priority of this client's LED intents (0-255, default 50).
    pub fn priority(mut self, priority: u8) -> Self {
        self.priority = priority;
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Reconnect after the service restarts (the default is to report
    /// `Disconnected` and stop).
    pub fn reconnecting(mut self) -> Self {
        self.reconnect = true;
        self
    }

    /// Ask the service to keep what this client set after it disconnects:
    /// LED intents, control writes and raw claims stay until replaced or
    /// released (for one-shot command-line clients such as `lemnos-ctl`).
    /// Without it, they end with the connection.
    pub fn keep_intents(mut self) -> Self {
        self.keep = true;
        self
    }

    pub fn devices(mut self) -> Result<DeviceClient, ClientError> {
        self.events.get_or_insert(true);
        Ok(DeviceClient {
            conn: Connection::open(self)?,
        })
    }

    pub fn leds(mut self) -> Result<LedClient, ClientError> {
        self.events.get_or_insert(false);
        Ok(LedClient {
            conn: Connection::open(self)?,
        })
    }
}

/// The shared connection state.
struct Connection {
    options: ClientOptions,
    stream: Option<UnixStream>,
    buf: Vec<u8>,
    board: String,
    client_id: u32,
    devices: Vec<(DeviceDesc, Arc<[ChannelDesc]>)>,
    queue: VecDeque<ClientEvent<Message>>,
    /// Device, channel names (empty: the whole device), period.
    subscriptions: Vec<(String, Vec<String>, u32)>,
    held: Vec<LedRequest>,
    next_id: u32,
    reconnects: u64,
    connected_once: bool,
    backoff: Duration,
    next_attempt: Instant,
}

impl Connection {
    fn open(options: ClientOptions) -> Result<Self, ClientError> {
        let mut conn = Self {
            options,
            stream: None,
            buf: Vec::with_capacity(4096),
            board: String::new(),
            client_id: 0,
            devices: Vec::new(),
            queue: VecDeque::new(),
            subscriptions: Vec::new(),
            held: Vec::new(),
            next_id: 1,
            reconnects: 0,
            connected_once: false,
            backoff: RECONNECT_MIN,
            next_attempt: Instant::now(),
        };
        let deadline = conn.options.wait.map(|w| Instant::now() + w);
        let mut pause = Duration::from_millis(20);
        loop {
            match conn.connect() {
                Ok(()) => return Ok(conn),
                Err(error) => {
                    conn.stream = None;
                    if let Some(deadline) = deadline
                        && Instant::now() + pause < deadline
                    {
                        std::thread::sleep(pause);
                        pause = (pause * 2).min(Duration::from_secs(1));
                        continue;
                    }
                    if conn.options.reconnect && deadline.is_none() {
                        conn.queue.push_back(ClientEvent::Disconnected { error });
                        conn.next_attempt = Instant::now() + conn.backoff;
                        return Ok(conn);
                    }
                    return Err(error);
                }
            }
        }
    }

    pub(super) fn next_id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        id
    }

    /// Connects, greets, fetches the device list and restores subscriptions
    /// and held intents.
    fn connect(&mut self) -> Result<(), ClientError> {
        let stream = UnixStream::connect(&self.options.path)?;
        stream.set_read_timeout(Some(self.options.timeout))?;
        stream.set_write_timeout(Some(self.options.timeout))?;
        self.stream = Some(stream);
        self.buf.clear();
        self.send(&Request::Hello {
            version: VERSION,
            client: self.options.name.clone(),
            priority: self.options.priority,
            keep: self.options.keep,
            events: self.options.events.unwrap_or(true),
        })?;
        let deadline = Instant::now() + self.options.timeout;
        loop {
            match self.receive(deadline)? {
                Message::Welcome {
                    board, client_id, ..
                } => {
                    self.board = board;
                    self.client_id = client_id;
                    break;
                }
                other => self.queue.push_back(ClientEvent::Data(other)),
            }
        }
        self.refresh_devices()?;
        for (device, channels, period) in self.subscriptions.clone() {
            // No id: the answer is not awaited (a refusal comes back as a
            // reply nobody reads).
            let request = if channels.is_empty() {
                Request::Subscribe {
                    id: 0,
                    device,
                    period_ms: period,
                }
            } else {
                Request::SubscribeChannels {
                    id: 0,
                    device,
                    channels,
                    period_ms: period,
                }
            };
            self.send(&request)?;
        }
        for led in self.held.clone() {
            self.send(&Request::Led(led))?;
        }
        if self.connected_once {
            self.reconnects += 1;
        }
        self.queue.push_back(ClientEvent::Connected {
            reconnects: self.reconnects,
        });
        self.connected_once = true;
        self.backoff = RECONNECT_MIN;
        Ok(())
    }

    fn send(&mut self, request: &Request) -> Result<(), ClientError> {
        let stream = self.stream.as_mut().ok_or(ClientError::Closed)?;
        match stream.write_all(&request.encode()) {
            Ok(()) => Ok(()),
            Err(e) => {
                self.lost(ClientError::Io(e));
                Err(ClientError::Closed)
            }
        }
    }

    fn lost(&mut self, error: ClientError) {
        if self.stream.take().is_some() {
            self.queue.push_back(ClientEvent::Disconnected { error });
            self.next_attempt = Instant::now() + self.backoff;
            self.backoff = (self.backoff * 2).min(RECONNECT_MAX);
        }
    }

    /// The next message, waiting until `deadline`.
    fn receive(&mut self, deadline: Instant) -> Result<Message, ClientError> {
        loop {
            if let Some((message, used)) = wire::decode_message(&self.buf)? {
                self.buf.drain(..used);
                return Ok(message);
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(ClientError::Timeout);
            }
            let stream = self.stream.as_mut().ok_or(ClientError::Closed)?;
            stream.set_read_timeout(Some((deadline - now).max(Duration::from_millis(1))))?;
            let mut chunk = [0u8; 4096];
            match stream.read(&mut chunk) {
                Ok(0) => {
                    self.lost(ClientError::Io(io::ErrorKind::ConnectionReset.into()));
                    return Err(ClientError::Closed);
                }
                Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    return Err(ClientError::Timeout);
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => {
                    self.lost(ClientError::Io(e));
                    return Err(ClientError::Closed);
                }
            }
        }
    }

    /// Sends `request` and waits for the message `pick` accepts; other
    /// messages are queued as events.
    pub(super) fn request<T>(
        &mut self,
        request: &Request,
        mut pick: impl FnMut(&Message) -> Option<T>,
    ) -> Result<T, ClientError> {
        if self.stream.is_none() && self.options.reconnect {
            self.connect()?;
        }
        self.send(request)?;
        let deadline = Instant::now() + self.options.timeout;
        loop {
            let message = self.receive(deadline)?;
            if let Some(answer) = pick(&message) {
                return Ok(answer);
            }
            self.queue.push_back(ClientEvent::Data(message));
        }
    }

    fn refresh_devices(&mut self) -> Result<(), ClientError> {
        self.send(&Request::List)?;
        let deadline = Instant::now() + self.options.timeout;
        loop {
            match self.receive(deadline)? {
                Message::Devices(devices) => {
                    self.devices = devices
                        .into_iter()
                        .map(|d| {
                            let channels: Arc<[ChannelDesc]> = d.channels.clone().into();
                            (d, channels)
                        })
                        .collect();
                    return Ok(());
                }
                other => self.queue.push_back(ClientEvent::Data(other)),
            }
        }
    }

    fn reading(&self, raw: RawReading) -> Reading {
        let channels = self
            .devices
            .iter()
            .find(|(d, _)| d.id == raw.device)
            .map(|(_, c)| Arc::clone(c))
            .unwrap_or_else(|| Arc::from(Vec::new()));
        Reading {
            device: raw.device,
            timestamp_us: raw.timestamp_us,
            status: raw.status,
            raw: raw.values,
            channels,
        }
    }

    /// The next event, waiting up to `wait` (`None`: forever, retrying
    /// connections when reconnecting).
    fn next(
        &mut self,
        wait: Option<Duration>,
    ) -> Result<Option<ClientEvent<Message>>, ClientError> {
        let deadline = wait.map(|w| Instant::now() + w);
        loop {
            if let Some(event) = self.queue.pop_front() {
                return Ok(Some(event));
            }
            if self.stream.is_none() {
                if !self.options.reconnect {
                    return Err(ClientError::Closed);
                }
                let now = Instant::now();
                if let Some(deadline) = deadline
                    && deadline <= now
                {
                    return Ok(None);
                }
                if now < self.next_attempt {
                    let until = deadline.map_or(self.next_attempt, |d| d.min(self.next_attempt));
                    std::thread::sleep(until.saturating_duration_since(now));
                    continue;
                }
                if let Err(error) = self.connect() {
                    self.stream = None;
                    self.next_attempt = Instant::now() + self.backoff;
                    self.backoff = (self.backoff * 2).min(RECONNECT_MAX);
                    let _ = error;
                }
                continue;
            }
            let until = deadline.unwrap_or_else(|| Instant::now() + Duration::from_secs(3600));
            match self.receive(until) {
                Ok(message) => return Ok(Some(ClientEvent::Data(message))),
                Err(ClientError::Timeout) if deadline.is_some() => return Ok(None),
                Err(ClientError::Timeout) | Err(ClientError::Closed) => continue,
                Err(other) => return Err(other),
            }
        }
    }
}

fn reply(id: u32) -> impl FnMut(&Message) -> Option<Result<f64, Refusal>> {
    move |m| match m {
        Message::Reply { id: got, result } if *got == id => Some(*result),
        _ => None,
    }
}

#[path = "client/calibration.rs"]
mod calibration;

/// Reads, subscribes to and controls `lemnosd`'s devices.
pub struct DeviceClient {
    conn: Connection,
}

impl DeviceClient {
    /// The service at `path`, as client `name`, with default options.
    pub fn connect(path: impl AsRef<Path>, name: impl Into<String>) -> Result<Self, ClientError> {
        ClientOptions::new(path, name).devices()
    }

    pub fn options(path: impl AsRef<Path>, name: impl Into<String>) -> ClientOptions {
        ClientOptions::new(path, name)
    }

    /// The board definition's id.
    pub fn board(&self) -> &str {
        &self.conn.board
    }

    /// The devices, as of the last connection or [`list`](Self::list).
    pub fn devices(&self) -> impl Iterator<Item = &DeviceDesc> {
        self.conn.devices.iter().map(|(d, _)| d)
    }

    /// Fetches the device list (with current statuses).
    pub fn list(&mut self) -> Result<Vec<DeviceDesc>, ClientError> {
        self.conn.refresh_devices()?;
        Ok(self.devices().cloned().collect())
    }

    /// The latest reading of `device`.
    pub fn read(&mut self, device: &str) -> Result<Reading, ClientError> {
        let name = device.to_string();
        let raw = self.conn.request(
            &Request::Read {
                device: name.clone(),
            },
            |m| match m {
                Message::Reading(r) if r.device == name => Some(Ok(r.clone())),
                Message::Reply {
                    id: 0,
                    result: Err(refusal),
                } => Some(Err(*refusal)),
                _ => None,
            },
        )?;
        raw.map(|r| self.conn.reading(r))
            .map_err(ClientError::Refused)
    }

    /// Streams `device`'s readings at most every `period_ms` (0
    /// unsubscribes), and kept across reconnections. Waits for the service's
    /// answer and returns the granted period: the requested one, or the
    /// device's read time when that is longer (readings cannot come faster
    /// than the device is read).
    ///
    /// A refusal ([`ClientError::Refused`]) says why nothing will arrive:
    /// [`Refusal::UnknownDevice`], or [`Refusal::Unsupported`] for a device
    /// that produces no readings. A refused subscription is not kept.
    pub fn subscribe(&mut self, device: &str, period_ms: u32) -> Result<u32, ClientError> {
        let id = self.conn.next_id();
        self.conn.subscriptions.retain(|(d, _, _)| d != device);
        if period_ms > 0 {
            self.conn
                .subscriptions
                .push((device.to_string(), Vec::new(), period_ms));
        }
        let request = Request::Subscribe {
            id,
            device: device.to_string(),
            period_ms,
        };
        match self.conn.request(&request, reply(id))? {
            Ok(granted) => Ok(u32::try_from(granted as u64).unwrap_or(u32::MAX)),
            Err(refusal) => {
                self.conn.subscriptions.retain(|(d, _, _)| d != device);
                Err(ClientError::Refused(refusal))
            }
        }
    }

    /// Subscribes to some of a device's channels only: each name is a channel
    /// (`angular_rate.z`), a prefix with a trailing `.*` (`angular_rate.*`), or
    /// `*` for all. The device's other channels come back as no value, and
    /// the device reads only what these channels need on its bus. Subscriptions
    /// on one device with different channels run at their own periods (say,
    /// gyro Z at 100 Hz and accelerometer at 10 Hz). `period_ms` 0 ends this
    /// selection's subscription. Returns the granted period, as
    /// [`subscribe`](Self::subscribe).
    pub fn subscribe_channels(
        &mut self,
        device: &str,
        channels: &[&str],
        period_ms: u32,
    ) -> Result<u32, ClientError> {
        let id = self.conn.next_id();
        let names: Vec<String> = channels.iter().map(|c| (*c).to_string()).collect();
        self.conn
            .subscriptions
            .retain(|(d, c, _)| !(d == device && *c == names));
        if period_ms > 0 {
            self.conn
                .subscriptions
                .push((device.to_string(), names.clone(), period_ms));
        }
        let request = Request::SubscribeChannels {
            id,
            device: device.to_string(),
            channels: names.clone(),
            period_ms,
        };
        match self.conn.request(&request, reply(id))? {
            Ok(granted) => Ok(u32::try_from(granted as u64).unwrap_or(u32::MAX)),
            Err(refusal) => {
                self.conn
                    .subscriptions
                    .retain(|(d, c, _)| !(d == device && *c == names));
                Err(ClientError::Refused(refusal))
            }
        }
    }

    /// Sets a control to `value` in its unit; returns the value applied.
    pub fn set(&mut self, device: &str, control: &str, value: f64) -> Result<f64, ClientError> {
        let id = self.conn.next_id;
        self.conn.next_id = self.conn.next_id.wrapping_add(1).max(1);
        let request = Request::Set {
            id,
            device: device.into(),
            control: control.into(),
            value,
        };
        self.conn
            .request(&request, reply(id))?
            .map_err(ClientError::Refused)
    }

    /// Hands fan `device` back to the kernel's thermal governor while the
    /// service keeps running; it stays there until a client writes one of
    /// its controls again.
    pub fn release(&mut self, device: &str) -> Result<(), ClientError> {
        let id = self.conn.next_id;
        self.conn.next_id = self.conn.next_id.wrapping_add(1).max(1);
        let request = Request::Release {
            id,
            device: device.into(),
        };
        self.conn
            .request(&request, reply(id))?
            .map(|_| ())
            .map_err(ClientError::Refused)
    }

    /// A control's current value in its unit.
    pub fn get(&mut self, device: &str, control: &str) -> Result<f64, ClientError> {
        let id = self.conn.next_id;
        self.conn.next_id = self.conn.next_id.wrapping_add(1).max(1);
        let request = Request::Get {
            id,
            device: device.into(),
            control: control.into(),
        };
        self.conn
            .request(&request, reply(id))?
            .map_err(ClientError::Refused)
    }

    fn convert(&self, event: ClientEvent<Message>) -> Option<ClientEvent<Update>> {
        Some(match event {
            ClientEvent::Connected { reconnects } => ClientEvent::Connected { reconnects },
            ClientEvent::Disconnected { error } => ClientEvent::Disconnected { error },
            ClientEvent::Data(Message::Reading(r)) => {
                ClientEvent::Data(Update::Reading(self.conn.reading(r)))
            }
            ClientEvent::Data(Message::Event(e)) => ClientEvent::Data(Update::Event(e)),
            ClientEvent::Data(_) => return None,
        })
    }

    /// The next event, blocking (reconnecting clients retry in here).
    pub fn next_event(&mut self) -> Result<ClientEvent<Update>, ClientError> {
        loop {
            if let Some(event) = self.conn.next(None)?
                && let Some(event) = self.convert(event)
            {
                return Ok(event);
            }
        }
    }

    /// The next event if one arrives within `wait`.
    pub fn next_event_timeout(
        &mut self,
        wait: Duration,
    ) -> Result<Option<ClientEvent<Update>>, ClientError> {
        let deadline = Instant::now() + wait;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.conn.next(Some(left))? {
                Some(event) => {
                    if let Some(event) = self.convert(event) {
                        return Ok(Some(event));
                    }
                }
                None => return Ok(None),
            }
        }
    }
}

impl AsFd for DeviceClient {
    /// The socket, for a `poll` loop; readable when an event may be ready.
    /// Panics while disconnected (check [`is_connected`](Self::is_connected)).
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.conn.stream.as_ref().expect("connected").as_fd()
    }
}

impl DeviceClient {
    pub fn is_connected(&self) -> bool {
        self.conn.stream.is_some()
    }

    /// Reconnections so far.
    pub fn reconnects(&self) -> u64 {
        self.conn.reconnects
    }
}
