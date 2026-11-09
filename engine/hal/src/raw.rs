//! Raw lines and PWM channels: how a client configures, drives and reads a
//! GPIO line or a PWM channel it claimed, defined once for every layer.
//!
//! [`RawLine`], [`RawPwm`] and [`RawSpi`] are implemented by the Linux
//! backends (the GPIO character device, sysfs PWM, spidev), by the mocks in
//! `mock`, and can be by microcontroller HALs. `lemnosd` hands them out to
//! clients through its socket, and its wire protocol encodes exactly these
//! types. I2C needs nothing new: it is the embedded-hal bus.

use crate::ErrorKind;

/// Line direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Direction {
    #[default]
    Input,
    Output,
}

/// Internal bias.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Bias {
    /// As the device tree or a previous user left it.
    #[default]
    AsIs,
    PullUp,
    PullDown,
    /// No bias (high impedance for an input).
    Disabled,
}

/// Output drive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Drive {
    #[default]
    PushPull,
    OpenDrain,
    OpenSource,
}

/// Which edges of an input produce events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum EdgeDetect {
    #[default]
    None,
    Rising,
    Falling,
    Both,
}

/// How a claimed line is configured. Values are logical: with `active_low`,
/// logical high drives the pin low.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct LineConfig {
    pub direction: Direction,
    /// The logical value an output starts at.
    pub initial: bool,
    pub active_low: bool,
    pub bias: Bias,
    pub drive: Drive,
    pub edge: EdgeDetect,
    /// Debounce period of an input, microseconds (0: none).
    pub debounce_us: u32,
}

impl LineConfig {
    /// An input.
    pub const fn input() -> Self {
        Self {
            direction: Direction::Input,
            initial: false,
            active_low: false,
            bias: Bias::AsIs,
            drive: Drive::PushPull,
            edge: EdgeDetect::None,
            debounce_us: 0,
        }
    }

    /// A push-pull output starting at logical `value`.
    pub const fn output(value: bool) -> Self {
        let mut config = Self::input();
        config.direction = Direction::Output;
        config.initial = value;
        config
    }

    /// An input with no bias: a released line's safe state.
    pub const fn high_impedance() -> Self {
        let mut config = Self::input();
        config.bias = Bias::Disabled;
        config
    }

    pub const fn with_bias(mut self, bias: Bias) -> Self {
        self.bias = bias;
        self
    }

    pub const fn with_drive(mut self, drive: Drive) -> Self {
        self.drive = drive;
        self
    }

    pub const fn with_edge(mut self, edge: EdgeDetect) -> Self {
        self.edge = edge;
        self
    }

    pub const fn with_debounce_us(mut self, debounce_us: u32) -> Self {
        self.debounce_us = debounce_us;
        self
    }

    pub const fn active_low(mut self) -> Self {
        self.active_low = true;
        self
    }
}

/// What a line is left as when its claim ends (the client releases it or
/// disconnects).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SafeState {
    /// An input with no bias (high impedance): the default.
    #[default]
    Input,
    /// An output driven low (physical level).
    Low,
    /// An output driven high (physical level).
    High,
    /// Left exactly as the client last set it.
    Keep,
}

impl SafeState {
    /// The configuration that puts a line in this state (`None` for
    /// [`SafeState::Keep`]).
    pub const fn config(self) -> Option<LineConfig> {
        match self {
            Self::Input => Some(LineConfig::high_impedance()),
            Self::Low => Some(LineConfig::output(false)),
            Self::High => Some(LineConfig::output(true)),
            Self::Keep => None,
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "input" => Some(Self::Input),
            "low" => Some(Self::Low),
            "high" => Some(Self::High),
            "keep" => Some(Self::Keep),
            _ => None,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Input => "input",
            Self::Low => "low",
            Self::High => "high",
            Self::Keep => "keep",
        }
    }
}

/// An edge an input saw.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Edge {
    /// Logical low to high (`false`: high to low).
    pub rising: bool,
    /// Monotonic nanoseconds (the kernel's `CLOCK_MONOTONIC` on Linux).
    pub timestamp_ns: u64,
    /// Sequence number on this line, to spot missed events.
    pub seq: u32,
}

/// A claimed GPIO line.
pub trait RawLine {
    /// Changes the configuration without releasing the line.
    fn configure(&mut self, config: &LineConfig) -> Result<(), ErrorKind>;
    /// The logical value (inputs and outputs).
    fn get(&mut self) -> Result<bool, ErrorKind>;
    /// Sets the logical value of an output.
    fn set(&mut self, value: bool) -> Result<(), ErrorKind>;
    /// The next pending edge, without waiting.
    fn read_edge(&mut self) -> Result<Option<Edge>, ErrorKind>;
}

/// PWM output polarity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Polarity {
    #[default]
    Normal,
    Inversed,
}

/// A PWM channel's settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct PwmConfig {
    pub period_ns: u64,
    pub duty_ns: u64,
    pub polarity: Polarity,
    pub enabled: bool,
}

/// A claimed PWM channel.
pub trait RawPwm {
    /// Applies `config` (period, duty, polarity, then enable).
    fn configure(&mut self, config: &PwmConfig) -> Result<(), ErrorKind>;
    /// The channel's current settings.
    fn config(&mut self) -> Result<PwmConfig, ErrorKind>;
}

/// SPI clock polarity and phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SpiMode {
    #[default]
    Mode0,
    Mode1,
    Mode2,
    Mode3,
}

impl SpiMode {
    pub const fn from_bits(bits: u8) -> Self {
        match bits & 3 {
            0 => Self::Mode0,
            1 => Self::Mode1,
            2 => Self::Mode2,
            _ => Self::Mode3,
        }
    }

    pub const fn bits(self) -> u8 {
        self as u8
    }
}

/// Settings of one SPI transfer (a transaction may mix them).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct SpiConfig {
    pub mode: SpiMode,
    /// Clock, Hz (0: the device's default).
    pub speed_hz: u32,
    /// Bits per word (0: 8).
    pub bits_per_word: u8,
}

/// One transfer of a raw SPI transaction: `tx` is clocked out while `rx`
/// fills (full duplex; either may be empty, and a longer side is padded
/// with zeros on the wire).
#[derive(Debug)]
pub struct SpiSegment<'a> {
    pub config: SpiConfig,
    pub tx: &'a [u8],
    pub rx: &'a mut [u8],
    /// Deselect the chip after this transfer.
    pub cs_change: bool,
    /// Wait after this transfer, microseconds.
    pub delay_us: u16,
}

/// A raw SPI device (one bus and chip select): a transaction keeps the chip
/// selected across its segments unless one asks otherwise.
pub trait RawSpi {
    fn transfer(&mut self, segments: &mut [SpiSegment<'_>]) -> Result<(), ErrorKind>;
}
