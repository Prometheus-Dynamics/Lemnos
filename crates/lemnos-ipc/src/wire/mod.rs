//! The `lemnosd` socket protocol: length-prefixed frames over a Unix stream
//! socket, little-endian.
//!
//! Each frame is `u32 length` (of what follows), `u16 kind`, then the
//! payload. Strings are `u16 length` + UTF-8; lists are `u16 count` + items.
//! Readings carry fixed-point integers; the device list carries each
//! channel's exponent, so clients convert to `f64` themselves. A client opens
//! with [`Request::Hello`]; the service answers [`Message::Welcome`].

use lemnos_device::{Axis, DeviceClass, DeviceStatus, Quantity};
use lemnos_hal::ErrorKind;
use std::fmt;

/// The protocol version both sides send in their greeting.
pub const VERSION: u16 = 1;
/// Frames longer than this are refused.
pub const MAX_FRAME: usize = 1 << 20;

const HELLO: u16 = 1;
const LIST: u16 = 2;
const READ: u16 = 3;
const SUBSCRIBE: u16 = 4;
const SET: u16 = 5;
const GET: u16 = 6;
const LED: u16 = 7;
const WELCOME: u16 = 101;
const DEVICES: u16 = 102;
const READING: u16 = 103;
const REPLY: u16 = 104;
const EVENT: u16 = 105;

/// A malformed frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireError(pub String);

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "lemnosd protocol: {}", self.0)
    }
}

impl std::error::Error for WireError {}

fn bad(what: impl Into<String>) -> WireError {
    WireError(what.into())
}

mod codec;
mod message;
mod request;

use codec::{Code, frame};

// --- messages ---------------------------------------------------------------

/// A channel, as a client sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelDesc {
    pub name: String,
    pub quantity: Quantity,
    pub axis: Axis,
    pub exponent: i8,
}

/// A control, as a client sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlDesc {
    pub name: String,
    pub quantity: Quantity,
    pub exponent: i8,
    pub min: i32,
    pub max: i32,
}

/// A device, as a client sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceDesc {
    /// The board definition's device id.
    pub id: String,
    pub label: String,
    pub class: DeviceClass,
    pub model: String,
    pub status: DeviceStatus,
    pub channels: Vec<ChannelDesc>,
    pub controls: Vec<ControlDesc>,
    /// LEDs, for lights; 0 otherwise.
    pub pixels: u16,
}

/// One device reading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawReading {
    pub device: String,
    /// Microseconds on the service's monotonic clock.
    pub timestamp_us: u64,
    pub status: DeviceStatus,
    /// One fixed-point value per channel (`NO_VALUE`: none).
    pub values: Vec<i32>,
}

/// Why the service refused a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Refusal {
    /// The device's write policy does not list this client.
    NotAllowed,
    OutOfRange,
    UnknownDevice,
    UnknownControl,
    /// The device failed or is not available; its error kind.
    Device(ErrorKind),
}

impl Refusal {
    fn code(self) -> (u8, u8) {
        match self {
            Self::NotAllowed => (1, 0),
            Self::OutOfRange => (2, 0),
            Self::UnknownDevice => (3, 0),
            Self::UnknownControl => (4, 0),
            Self::Device(kind) => (5, kind.code()),
        }
    }

    fn from_code(code: u8, kind: u8) -> Self {
        match code {
            1 => Self::NotAllowed,
            2 => Self::OutOfRange,
            3 => Self::UnknownDevice,
            4 => Self::UnknownControl,
            _ => Self::Device(ErrorKind::from_code(kind)),
        }
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAllowed => f.write_str("not allowed by the device's write policy"),
            Self::OutOfRange => f.write_str("value out of range"),
            Self::UnknownDevice => f.write_str("no such device"),
            Self::UnknownControl => f.write_str("no such control"),
            Self::Device(kind) => write!(f, "device error: {kind}"),
        }
    }
}

/// Something that happened, sent to every client.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// A device's status changed.
    Status {
        device: String,
        status: DeviceStatus,
        error: Option<ErrorKind>,
    },
    /// A control changed; `by` is the writer's client name.
    Control {
        device: String,
        control: String,
        value: f64,
        by: String,
    },
    /// Whose intent a light shows now (`owner` empty: nobody's).
    LedOwner {
        device: String,
        owner: String,
        layer: String,
    },
}

/// The LED request kinds. Colours are `0xWWRRGGBB` (white in the top byte);
/// LED indices are logical, with the board's geometry applied (index 0 is the
/// ring's "top").
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LedShow {
    /// Drop this client's intents (all layers).
    Clear,
    Status(lemnos_light::Status),
    Color(u32),
    /// One colour per LED, from index 0.
    Frame(Vec<u32>),
    /// Single LEDs `(index, colour)`, merged into this client's frame.
    Pixels(Vec<(u16, u32)>),
    /// A gauge at `fraction` thousandths.
    Progress {
        fraction: u16,
        color: Option<u32>,
        background: Option<u32>,
    },
    /// A spinner: progress of an unknown amount.
    Indeterminate {
        color: Option<u32>,
    },
    /// A built-in system animation.
    System(lemnos_light::SystemState),
    Locate,
}

/// An LED intent on the wire. `None` fields take the light's defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedRequest {
    /// The light (empty: the board's status light).
    pub device: String,
    pub show: LedShow,
    pub effect: Option<lemnos_light::EffectKind>,
    pub period_ms: Option<u32>,
    /// Breathe depth or blink duty, thousandths.
    pub depth: Option<u16>,
    /// Thousandths.
    pub brightness: Option<u16>,
    pub fade_ms: Option<u32>,
    pub easing: Option<lemnos_light::Easing>,
    /// How long the intent lasts (`None`: until cleared or the client
    /// leaves).
    pub duration_ms: Option<u32>,
}

impl LedRequest {
    pub fn new(show: LedShow) -> Self {
        Self {
            device: String::new(),
            show,
            effect: None,
            period_ms: None,
            depth: None,
            brightness: None,
            fade_ms: None,
            easing: None,
            duration_ms: None,
        }
    }
}

/// Client to service.
#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    /// `keep`: the service keeps this client's LED intents after it
    /// disconnects (for one-shot command-line clients).
    Hello {
        version: u16,
        client: String,
        priority: u8,
        keep: bool,
    },
    List,
    Read {
        device: String,
    },
    /// `period_ms` 0 unsubscribes.
    Subscribe {
        device: String,
        period_ms: u32,
    },
    Set {
        id: u32,
        device: String,
        control: String,
        value: f64,
    },
    Get {
        id: u32,
        device: String,
        control: String,
    },
    Led(LedRequest),
}

/// Service to client.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    Welcome {
        version: u16,
        board: String,
        client_id: u32,
    },
    Devices(Vec<DeviceDesc>),
    Reading(RawReading),
    Reply {
        id: u32,
        result: Result<f64, Refusal>,
    },
    Event(Event),
}

/// Splits and decodes requests from `buf`; returns the request and the bytes
/// it used, `None` until a whole frame arrived.
pub fn decode_request(buf: &[u8]) -> Result<Option<(Request, usize)>, WireError> {
    match frame(buf)? {
        Some((kind, payload, used)) => Ok(Some((Request::decode(kind, payload)?, used))),
        None => Ok(None),
    }
}

/// Splits and decodes messages from `buf`.
pub fn decode_message(buf: &[u8]) -> Result<Option<(Message, usize)>, WireError> {
    match frame(buf)? {
        Some((kind, payload, used)) => Ok(Some((Message::decode(kind, payload)?, used))),
        None => Ok(None),
    }
}
