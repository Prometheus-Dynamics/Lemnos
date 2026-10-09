//! Clients of `lemnosd`: [`DeviceClient`] (devices, readings, controls) and
//! [`LedClient`] (LED intents). Both connect with a timeout, can reconnect in
//! the background, and report connection changes in order with what they
//! receive ([`ClientEvent`]), like Styx's `FrameClient` and `ControlClient`.

use crate::wire::{
    self, ChannelDesc, DeviceDesc, Event, LedRequest, LedShow, Message, RawReading, Refusal,
    Request, VERSION, WireError,
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
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "lemnosd connection: {e}"),
            Self::Protocol(e) => e.fmt(f),
            Self::Refused(r) => write!(f, "refused: {r}"),
            Self::Timeout => f.write_str("lemnosd did not answer in time"),
            Self::Closed => f.write_str("not connected to lemnosd"),
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
    /// Microseconds on the service's monotonic clock.
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
        }
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

    /// Ask the service to keep this client's LED intents after it
    /// disconnects (for one-shot command-line clients).
    pub fn keep_intents(mut self) -> Self {
        self.keep = true;
        self
    }

    pub fn devices(self) -> Result<DeviceClient, ClientError> {
        Ok(DeviceClient {
            conn: Connection::open(self)?,
        })
    }

    pub fn leds(self) -> Result<LedClient, ClientError> {
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
    subscriptions: Vec<(String, u32)>,
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
        conn.connect()?;
        Ok(conn)
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
        for (device, period) in self.subscriptions.clone() {
            self.send(&Request::Subscribe {
                device,
                period_ms: period,
            })?;
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
    fn request<T>(
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
    /// unsubscribes). Kept across reconnections.
    pub fn subscribe(&mut self, device: &str, period_ms: u32) -> Result<(), ClientError> {
        self.conn.subscriptions.retain(|(d, _)| d != device);
        if period_ms > 0 {
            self.conn
                .subscriptions
                .push((device.to_string(), period_ms));
        }
        self.conn.send(&Request::Subscribe {
            device: device.to_string(),
            period_ms,
        })
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

/// Holds LED intents on `lemnosd`'s lights.
pub struct LedClient {
    conn: Connection,
}

impl LedClient {
    /// The service at `path`, as client `name`.
    pub fn connect(path: impl AsRef<Path>, name: impl Into<String>) -> Result<Self, ClientError> {
        ClientOptions::new(path, name).leds()
    }

    /// Sends `request` and remembers it (to send again after a reconnection).
    pub fn send(&mut self, request: LedRequest) -> Result<(), ClientError> {
        if request.show == LedShow::Clear {
            self.conn
                .held
                .retain(|r| r.device != request.device || (request.test && !r.test));
        } else {
            let layer = layer_of(&request);
            self.conn
                .held
                .retain(|r| r.device != request.device || layer_of(r) != layer);
            // Test intents are leases the caller renews: not re-sent.
            if request.duration_ms.is_none() && !request.test {
                self.conn.held.push(request.clone());
            }
        }
        if self.conn.stream.is_none() && self.conn.options.reconnect {
            self.conn.connect()?;
        }
        self.conn.send(&Request::Led(request))
    }

    /// Shows `status` with the light's default effect.
    pub fn status(&mut self, status: lemnos_light::Status) -> Result<(), ClientError> {
        self.send(LedRequest::new(LedShow::Status(status)))
    }

    /// Every LED `0xRRGGBB`.
    pub fn color(&mut self, rgb: u32) -> Result<(), ClientError> {
        self.send(LedRequest::new(LedShow::Color(rgb)))
    }

    /// One `0xWWRRGGBB` colour per LED.
    pub fn frame(&mut self, pixels: &[u32]) -> Result<(), ClientError> {
        self.send(LedRequest::new(LedShow::Frame(pixels.to_vec())))
    }

    /// Sets single LEDs `(logical index, 0xWWRRGGBB)`, keeping this client's
    /// other LEDs; index 0 is the ring's top.
    pub fn set_leds(&mut self, pixels: &[(u16, u32)]) -> Result<(), ClientError> {
        self.send(LedRequest::new(LedShow::Pixels(pixels.to_vec())))
    }

    /// A gauge filled to `fraction` (0.0-1.0) in `0xRRGGBB` (`None`: the
    /// board's progress colour); the fill advances eased.
    pub fn progress(&mut self, fraction: f32, color: Option<u32>) -> Result<(), ClientError> {
        let fraction = (fraction.clamp(0.0, 1.0) * 1000.0).round() as u16;
        self.send(LedRequest::new(LedShow::Progress {
            fraction,
            color,
            background: None,
        }))
    }

    /// A spinner for progress of an unknown amount.
    pub fn indeterminate(&mut self, color: Option<u32>) -> Result<(), ClientError> {
        self.send(LedRequest::new(LedShow::Indeterminate { color }))
    }

    /// A built-in system animation (updating, booting, rebooting, update
    /// failed, rolled back), above application status.
    pub fn system(&mut self, state: lemnos_light::SystemState) -> Result<(), ClientError> {
        self.send(LedRequest::new(LedShow::System(state)))
    }

    /// Shows the board's locate look over everything for `duration`.
    pub fn locate(&mut self, duration: Duration) -> Result<(), ClientError> {
        let mut request = LedRequest::new(LedShow::Locate);
        request.duration_ms = Some(duration.as_millis().min(u128::from(u32::MAX - 1)) as u32);
        self.send(request)
    }

    /// Shows `show` in the test layer, over every client's status, for
    /// `lease` (`None`: the service's default, 10 s in `lemnosd`). Send it
    /// again to renew it; it falls back to the layers below when the lease
    /// runs out or this client disconnects.
    pub fn test(&mut self, show: LedShow, lease: Option<Duration>) -> Result<(), ClientError> {
        let mut request = LedRequest::new(show).test();
        request.duration_ms = lease.map(|d| d.as_millis().min(u128::from(u32::MAX - 1)) as u32);
        self.send(request)
    }

    /// Drops this client's test intent.
    pub fn clear_test(&mut self) -> Result<(), ClientError> {
        self.send(LedRequest::new(LedShow::Clear).test())
    }

    /// Drops this client's intents.
    pub fn clear(&mut self) -> Result<(), ClientError> {
        self.send(LedRequest::new(LedShow::Clear))
    }

    /// Waits for the service to process what was sent (a round trip).
    pub fn sync(&mut self) -> Result<(), ClientError> {
        self.conn.request(&Request::List, |m| {
            matches!(m, Message::Devices(_)).then_some(())
        })
    }
}

fn layer_of(request: &LedRequest) -> u8 {
    if request.test {
        return 5;
    }
    match &request.show {
        LedShow::Clear => 0,
        LedShow::Color(_)
        | LedShow::Frame(_)
        | LedShow::Pixels(_)
        | LedShow::Progress { .. }
        | LedShow::Indeterminate { .. } => 1,
        LedShow::Status(_) => 2,
        LedShow::System(_) => 3,
        LedShow::Locate => 4,
    }
}
