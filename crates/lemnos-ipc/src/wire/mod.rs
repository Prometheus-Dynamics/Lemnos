//! The `lemnosd` socket protocol: length-prefixed frames over a Unix stream
//! socket, little-endian.
//!
//! Each frame is `u32 length` (of what follows), `u16 kind`, then the
//! payload. Strings are `u16 length` + UTF-8; lists are `u16 count` + items.
//! Readings carry fixed-point integers; the device list carries each
//! channel's exponent, so clients convert to `f64` themselves. A client opens
//! with [`Request::Hello`]; the service answers [`Message::Welcome`].
//!
//! Reading timestamps are microseconds on `CLOCK_BOOTTIME`, taken when the
//! read started. The clock keeps counting across a `lemnosd` restart, so a
//! timestamp from before a restart is still comparable with one after it. It
//! restarts from zero only at a reboot, so a stream that goes backwards means
//! the board rebooted. Intervals between readings of one device follow the
//! schedule; arrival times at a client also include socket delays.
//!
//! Units: a channel's `value()` is in its quantity's canonical unit (m/s²,
//! rad/s, V, A, ...). The raw count is that value scaled by `10^-exponent`:
//! the BMI088's counts are mm/s² (exponent -3) and µrad/s (exponent -6).

use lemnos_device::{
    Axis, CalibrationCommand, CalibrationStatus, DeviceClass, DeviceStatus, Quantity,
};
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
const RELEASE: u16 = 8;
const RESTORE: u16 = 9;
const LOOKS: u16 = 10;
const SUBSCRIBE_CHANNELS: u16 = 11;
const WATCH_FRAMES: u16 = 12;
const LIGHT_SETTING: u16 = 15;
const WELCOME: u16 = 101;
const DEVICES: u16 = 102;
const READING: u16 = 103;
const REPLY: u16 = 104;
const EVENT: u16 = 105;
const CLAIMED: u16 = 106;
const DATA: u16 = 107;
const TEXT: u16 = 108;
const FRAME: u16 = 109;
const LIGHT_INFO: u16 = 110;
const CALIBRATION_STATUS_REPLY: u16 = 111;
const CALIBRATION: u16 = 13;
const CALIBRATION_STATUS: u16 = 14;

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

mod calibration;
mod codec;
mod look;
mod message;
mod raw;
mod request;

pub use raw::{I2cOp, LineTarget, PwmTarget, RawRequest, SpiXfer};

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
    /// Why the device is not available, in the driver's or the board's own
    /// words (empty while it works, and from services that predate it).
    pub reason: String,
    pub channels: Vec<ChannelDesc>,
    pub controls: Vec<ControlDesc>,
    /// LEDs, for lights; 0 otherwise.
    pub pixels: u16,
}

/// One device reading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawReading {
    pub device: String,
    /// Microseconds on `CLOCK_BOOTTIME`, taken when the read started (see the
    /// module docs for the contract).
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
    /// A board device owns this bus address, line or channel.
    Owned,
    /// Another client holds a claim or lock on it.
    Claimed,
    /// No such claim handle on this connection.
    UnknownHandle,
    /// The device cannot do this: a subscription to a device that produces
    /// no readings (a light, a fan).
    Unsupported,
    /// A channel name a subscription asked for is not one of the device's.
    UnknownChannel,
}

impl Refusal {
    fn code(self) -> (u8, u8) {
        match self {
            Self::NotAllowed => (1, 0),
            Self::OutOfRange => (2, 0),
            Self::UnknownDevice => (3, 0),
            Self::UnknownControl => (4, 0),
            Self::Device(kind) => (5, kind.code()),
            Self::Owned => (6, 0),
            Self::Claimed => (7, 0),
            Self::UnknownHandle => (8, 0),
            Self::Unsupported => (9, 0),
            Self::UnknownChannel => (10, 0),
        }
    }

    fn from_code(code: u8, kind: u8) -> Self {
        match code {
            1 => Self::NotAllowed,
            2 => Self::OutOfRange,
            3 => Self::UnknownDevice,
            4 => Self::UnknownControl,
            6 => Self::Owned,
            7 => Self::Claimed,
            8 => Self::UnknownHandle,
            9 => Self::Unsupported,
            10 => Self::UnknownChannel,
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
            Self::Owned => f.write_str("owned by a board device"),
            Self::Claimed => f.write_str("claimed by another client"),
            Self::UnknownHandle => f.write_str("no such claim"),
            Self::Unsupported => f.write_str("not supported by this device"),
            Self::UnknownChannel => f.write_str("no such channel on this device"),
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
        /// Why, as [`DeviceDesc::reason`].
        reason: String,
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
    /// An edge on a line this connection claimed with edge detection.
    Edge {
        handle: u32,
        rising: bool,
        timestamp_ns: u64,
        seq: u32,
    },
    /// The service dropped this many events because the client did not read
    /// them (it keeps a bounded queue per client).
    Dropped { count: u32 },
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
    /// One or two comets going round: a colour (`None`: the progress colour),
    /// a tail length in thousandths of an LED (`None`: the spinner's), 1 or
    /// 2 heads, and a floor brightness in thousandths (`None`: 0). The period
    /// is the request's `period_ms`.
    Orbit {
        color: Option<u32>,
        tail: Option<u16>,
        heads: u8,
        base: Option<u16>,
    },
    /// A built-in system animation.
    System(lemnos_light::SystemState),
    Locate,
    /// A named look: a built-in, or one from a look file (`lemnosd`'s look
    /// table). An unknown name is refused, and the light keeps its look.
    /// `progress` (thousandths) fills the look's arcs that take an input.
    Look {
        name: String,
        progress: Option<u16>,
    },
    /// A look given in full, shown until replaced or cleared (or for the
    /// request's `duration_ms`). `progress` as for [`LedShow::Look`].
    /// (Boxed: a look is about a kilobyte, and requests are mostly small.)
    Inline {
        spec: Box<lemnos_light::LookSpec>,
        progress: Option<u16>,
    },
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
    /// leaves; for a `test` intent, the service's lease, 10 s in `lemnosd`).
    pub duration_ms: Option<u32>,
    /// A selftest or diagnostic look, in the test layer above every client's
    /// status ([`lemnos_light::Layer::Test`]). The service holds it on a
    /// lease (`duration_ms`, else its default), so a client renews it by
    /// sending it again; it ends at once when a client that does not keep
    /// its intents disconnects. With [`LedShow::Clear`], clears only this
    /// client's test intent.
    pub test: bool,
    /// A nonzero `id` gets a [`Message::Text`] reply: the empty text when
    /// the look is shown, else why it was refused (an unknown name, a bad
    /// look). `0` (what older clients send) gets no reply.
    pub id: u32,
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
            test: false,
            id: 0,
        }
    }

    /// The same request in the test layer.
    pub fn test(mut self) -> Self {
        self.test = true;
        self
    }
}

/// Client to service.
#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    /// `keep`: the service keeps this client's LED intents after it
    /// disconnects (for one-shot command-line clients).
    /// `events`: the client reads events (status, control, LED owner,
    /// edges); without it the service sends none.
    Hello {
        version: u16,
        client: String,
        priority: u8,
        keep: bool,
        events: bool,
    },
    List,
    Read {
        device: String,
    },
    /// `period_ms` 0 unsubscribes. A nonzero `id` gets a [`Message::Reply`]
    /// with the granted period, or a refusal. `id` 0 (what older clients
    /// send) gets one only for a refusal, as before.
    Subscribe {
        id: u32,
        device: String,
        period_ms: u32,
    },
    /// Subscribes to some of a device's channels, each name one channel
    /// (`angular_rate.z`), or a prefix with a trailing `.*` (`angular_rate.*`),
    /// or `*` for all. `period_ms` 0 ends this selection's subscription.
    /// Answered as [`Subscribe`](Self::Subscribe) is.
    SubscribeChannels {
        id: u32,
        device: String,
        channels: Vec<String>,
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
    /// Hands a fan back to the kernel's thermal governor while the service
    /// keeps running (the same hand-back as on stop). The fan stays with the
    /// governor until a client writes one of its controls again, which takes
    /// it back. Answered with a [`Message::Reply`] (value 0) under the
    /// device's write policy.
    Release {
        id: u32,
        device: String,
    },
    /// Undoes this client's control writes on `device` (`control` empty:
    /// all of them): fans go back to the kernel, other controls to their
    /// value from before the client's first write. Clients that do not keep
    /// their intents get this automatically when they disconnect.
    Restore {
        id: u32,
        device: String,
        control: String,
    },
    /// Raw bus and line access.
    Raw(RawRequest),
    /// Looks: list them, show one as TOML, reload the look files, or save a
    /// look into the writable directory. Answered with a [`Message::Text`].
    Looks {
        id: u32,
        op: LooksOp,
    },
    /// Watches a light's rendered frames: a [`Message::Frame`] whenever the
    /// frame changes, at most `fps` a second (0 ends the watch), after a
    /// [`Message::LightInfo`]. Answered with a [`Message::Reply`] (the granted
    /// rate, or a refusal). The frames are sent only while they change, so a
    /// watch costs nothing when the ring is still.
    WatchFrames {
        id: u32,
        device: String,
        fps: u16,
    },
    /// Starts, stops, applies, discards or resets a device's calibration (see
    /// [`CalibrationCommand`]). Answered with a [`Message::Reply`] (value 0),
    /// or a refusal: `UnknownDevice`, `Unsupported` (no calibration), or
    /// `NotAllowed` under the device's write policy.
    Calibration {
        id: u32,
        device: String,
        command: CalibrationCommand,
    },
    /// A device's calibration status. Answered with a
    /// [`Message::CalibrationStatus`], or a [`Message::Reply`] refusal.
    CalibrationStatus {
        id: u32,
        device: String,
    },
    /// Sets a light's ring-wide look brightness at runtime (`look_brightness`
    /// of the board, in thousandths: 0 to 1000). `persist` saves it in the
    /// state directory, where it is read back at start over the board's value.
    /// Answered with a [`Message::Reply`] (the value applied) or a refusal.
    LightSetting {
        id: u32,
        device: String,
        look_brightness: u16,
        persist: bool,
    },
}

/// A looks request (see [`Request::Looks`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LooksOp {
    /// The look names, with where each comes from.
    List,
    /// One look's resolved definition, as TOML.
    Show(String),
    /// Re-reads the look files now.
    Reload,
    /// Writes `text` (a look file body, `[looks.<name>]` table included) to
    /// `<name>.toml` in the writable directory, then reloads.
    Save { name: String, text: String },
    /// The presets: each built-in or saved one, with the active one marked.
    PresetList,
    /// One preset's look file text.
    PresetShow(String),
    /// Makes a preset the active one (its looks sit above the board's and
    /// below the look files), and remembers the choice.
    PresetApply(String),
    /// Saves `text` (a look file body with any looks in it) as a preset.
    PresetSave { name: String, text: String },
    /// Deletes a saved preset (the active one falls back to the default).
    PresetDelete(String),
    /// Deletes the look `name` from the writable directory.
    Delete(String),
}

/// Service to client.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// A rendered frame of a watched light (see [`Request::WatchFrames`]).
    Frame(LightFrame),
    /// A watched light's geometry and what it shows.
    LightInfo(LightInfo),
    /// The answer to a request that asked for text: a look listing or
    /// definition, or a success (empty) or refusal message.
    Text {
        id: u32,
        result: Result<String, String>,
    },
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
    /// The handle of a new line or PWM claim.
    Claimed {
        id: u32,
        result: Result<u32, Refusal>,
    },
    /// Bytes read by an I2C or SPI transaction.
    Data {
        id: u32,
        result: Result<Vec<u8>, Refusal>,
    },
    /// A device's calibration status (the answer to
    /// [`Request::CalibrationStatus`]).
    CalibrationStatus {
        id: u32,
        device: String,
        status: CalibrationStatus,
    },
}

/// One rendered frame of a light: exactly what `lemnosd` wrote to the ring
/// (after brightness and arbitration), one colour per logical LED, in the
/// `0xWWRRGGBB` form of [`LedShow::Color`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LightFrame {
    pub device: String,
    /// Counts the frames sent on this watch (from 1).
    pub seq: u32,
    pub pixels: Vec<u32>,
}

/// A light's geometry and what it shows, sent when a watch starts and when
/// any of it changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LightInfo {
    pub device: String,
    /// LEDs on the ring.
    pub count: u16,
    /// The physical LED that is logical LED 0 (the ring's "top").
    pub offset: u16,
    /// Whether logical indices run clockwise (`cw`) from the top.
    pub clockwise: bool,
    /// The named look shown, or empty for an inline look or none.
    pub look: String,
    /// The layer that holds the light (`status`, `application`, `test`,
    /// `alert`, `system`, `locate`), empty when nothing holds it.
    pub layer: String,
    /// The client that holds it (`lemnosd` for the service's own looks).
    pub owner: String,
    /// The ring-wide look brightness, in thousandths (`look_brightness`).
    pub look_brightness: u16,
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
