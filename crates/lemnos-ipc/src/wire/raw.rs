//! Raw bus and line access on the wire: claims of GPIO lines and PWM
//! channels, I2C and SPI transactions, and their answers.

use super::codec::{Decoder, Encoder};
use super::{WireError, bad};
use lemnos_hal::raw::{
    Bias, Direction, Drive, EdgeDetect, LineConfig, Polarity, PwmConfig, SafeState, SpiConfig,
    SpiMode,
};

/// A GPIO line to claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineTarget {
    /// `chip` is `gpiochipN` or a chip label (`pinctrl-rp1`).
    Chip { chip: String, offset: u32 },
    /// A board line name (the board definition's `[[lines]]`) or a kernel
    /// line name.
    Name(String),
}

/// A PWM channel to claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PwmTarget {
    Chip {
        chip: u32,
        channel: u32,
    },
    /// A board PWM name (`[[pwms]]`).
    Name(String),
}

/// One I2C message of a transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum I2cOp {
    Write(Vec<u8>),
    /// Read this many bytes.
    Read(u16),
}

/// One SPI transfer of a transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpiXfer {
    pub config: SpiConfig,
    pub tx: Vec<u8>,
    /// Bytes to read (full duplex with `tx`; the longer side wins).
    pub rx_len: u16,
    pub cs_change: bool,
    pub delay_us: u16,
}

impl SpiXfer {
    pub fn new(tx: impl Into<Vec<u8>>, rx_len: u16) -> Self {
        Self {
            config: SpiConfig::default(),
            tx: tx.into(),
            rx_len,
            cs_change: false,
            delay_us: 0,
        }
    }
}

/// A raw-access request. `bus` names an I2C bus as a number (`1`), `i2c-1`
/// or a board selector (`i2c:compatible=i2c-gpio`).
#[derive(Debug, Clone, PartialEq)]
pub enum RawRequest {
    /// Answered with [`super::Message::Claimed`].
    LineClaim {
        id: u32,
        line: LineTarget,
        config: LineConfig,
        /// Overrides the board's safe state for this claim.
        on_release: Option<SafeState>,
    },
    LineConfigure {
        id: u32,
        handle: u32,
        config: LineConfig,
    },
    /// Answered with the value (0 or 1).
    LineGet {
        id: u32,
        handle: u32,
    },
    LineSet {
        id: u32,
        handle: u32,
        value: bool,
    },
    /// Answered with [`super::Message::Claimed`].
    PwmClaim {
        id: u32,
        pwm: PwmTarget,
    },
    PwmConfigure {
        id: u32,
        handle: u32,
        config: PwmConfig,
    },
    /// Ends a line or PWM claim (the line goes to its safe state, the
    /// channel is disabled).
    Unclaim {
        id: u32,
        handle: u32,
    },
    /// Answered with [`super::Message::Data`]: the bytes read, in order.
    I2cTransfer {
        id: u32,
        bus: String,
        address: u16,
        ops: Vec<I2cOp>,
    },
    /// Takes (or with `lock = false` gives back) an address for this
    /// connection alone, across several transactions.
    I2cLock {
        id: u32,
        bus: String,
        address: u16,
        lock: bool,
    },
    /// Answered with [`super::Message::Data`]: the bytes read, in order.
    SpiTransfer {
        id: u32,
        bus: u32,
        chip_select: u16,
        transfers: Vec<SpiXfer>,
    },
    SpiLock {
        id: u32,
        bus: u32,
        chip_select: u16,
        lock: bool,
    },
}

pub(super) const LINE_CLAIM: u16 = 20;
pub(super) const LINE_CONFIGURE: u16 = 21;
pub(super) const LINE_GET: u16 = 22;
pub(super) const LINE_SET: u16 = 23;
pub(super) const PWM_CLAIM: u16 = 24;
pub(super) const PWM_CONFIGURE: u16 = 25;
pub(super) const UNCLAIM: u16 = 26;
pub(super) const I2C_TRANSFER: u16 = 27;
pub(super) const I2C_LOCK: u16 = 28;
pub(super) const SPI_TRANSFER: u16 = 29;
pub(super) const SPI_LOCK: u16 = 30;

/// Whether `kind` is a raw request.
pub(super) fn is_raw(kind: u16) -> bool {
    (LINE_CLAIM..=SPI_LOCK).contains(&kind)
}

fn line_config(e: &mut Encoder, c: &LineConfig) {
    e.u8(match c.direction {
        Direction::Input => 0,
        Direction::Output => 1,
    })
    .u8(u8::from(c.initial))
    .u8(u8::from(c.active_low))
    .u8(match c.bias {
        Bias::AsIs => 0,
        Bias::PullUp => 1,
        Bias::PullDown => 2,
        Bias::Disabled => 3,
    })
    .u8(match c.drive {
        Drive::PushPull => 0,
        Drive::OpenDrain => 1,
        Drive::OpenSource => 2,
    })
    .u8(match c.edge {
        EdgeDetect::None => 0,
        EdgeDetect::Rising => 1,
        EdgeDetect::Falling => 2,
        EdgeDetect::Both => 3,
    })
    .u32(c.debounce_us);
}

fn read_line_config(d: &mut Decoder<'_>) -> Result<LineConfig, WireError> {
    Ok(LineConfig {
        direction: if d.u8()? == 1 {
            Direction::Output
        } else {
            Direction::Input
        },
        initial: d.u8()? != 0,
        active_low: d.u8()? != 0,
        bias: match d.u8()? {
            1 => Bias::PullUp,
            2 => Bias::PullDown,
            3 => Bias::Disabled,
            _ => Bias::AsIs,
        },
        drive: match d.u8()? {
            1 => Drive::OpenDrain,
            2 => Drive::OpenSource,
            _ => Drive::PushPull,
        },
        edge: match d.u8()? {
            1 => EdgeDetect::Rising,
            2 => EdgeDetect::Falling,
            3 => EdgeDetect::Both,
            _ => EdgeDetect::None,
        },
        debounce_us: d.u32()?,
    })
}

fn safe_code(state: Option<SafeState>) -> u8 {
    match state {
        None => 0,
        Some(SafeState::Input) => 1,
        Some(SafeState::Low) => 2,
        Some(SafeState::High) => 3,
        Some(SafeState::Keep) => 4,
    }
}

fn safe_from(code: u8) -> Option<SafeState> {
    match code {
        1 => Some(SafeState::Input),
        2 => Some(SafeState::Low),
        3 => Some(SafeState::High),
        4 => Some(SafeState::Keep),
        _ => None,
    }
}

impl RawRequest {
    pub(super) fn encode(&self) -> Vec<u8> {
        match self {
            Self::LineClaim {
                id,
                line,
                config,
                on_release,
            } => {
                let mut e = Encoder::new(LINE_CLAIM);
                e.u32(*id);
                match line {
                    LineTarget::Chip { chip, offset } => e.u8(0).str(chip).u32(*offset),
                    LineTarget::Name(name) => e.u8(1).str(name).u32(0),
                };
                line_config(&mut e, config);
                e.u8(safe_code(*on_release));
                e.finish()
            }
            Self::LineConfigure { id, handle, config } => {
                let mut e = Encoder::new(LINE_CONFIGURE);
                e.u32(*id).u32(*handle);
                line_config(&mut e, config);
                e.finish()
            }
            Self::LineGet { id, handle } => {
                let mut e = Encoder::new(LINE_GET);
                e.u32(*id).u32(*handle);
                e.finish()
            }
            Self::LineSet { id, handle, value } => {
                let mut e = Encoder::new(LINE_SET);
                e.u32(*id).u32(*handle).u8(u8::from(*value));
                e.finish()
            }
            Self::PwmClaim { id, pwm } => {
                let mut e = Encoder::new(PWM_CLAIM);
                e.u32(*id);
                match pwm {
                    PwmTarget::Chip { chip, channel } => e.u8(0).u32(*chip).u32(*channel).str(""),
                    PwmTarget::Name(name) => e.u8(1).u32(0).u32(0).str(name),
                };
                e.finish()
            }
            Self::PwmConfigure { id, handle, config } => {
                let mut e = Encoder::new(PWM_CONFIGURE);
                e.u32(*id)
                    .u32(*handle)
                    .u64(config.period_ns)
                    .u64(config.duty_ns)
                    .u8(u8::from(config.polarity == Polarity::Inversed))
                    .u8(u8::from(config.enabled));
                e.finish()
            }
            Self::Unclaim { id, handle } => {
                let mut e = Encoder::new(UNCLAIM);
                e.u32(*id).u32(*handle);
                e.finish()
            }
            Self::I2cTransfer {
                id,
                bus,
                address,
                ops,
            } => {
                let mut e = Encoder::new(I2C_TRANSFER);
                e.u32(*id).str(bus).u16(*address).u16(ops.len() as u16);
                for op in ops {
                    match op {
                        I2cOp::Write(bytes) => e.u8(0).bytes(bytes),
                        I2cOp::Read(n) => e.u8(1).u32(u32::from(*n)),
                    };
                }
                e.finish()
            }
            Self::I2cLock {
                id,
                bus,
                address,
                lock,
            } => {
                let mut e = Encoder::new(I2C_LOCK);
                e.u32(*id).str(bus).u16(*address).u8(u8::from(*lock));
                e.finish()
            }
            Self::SpiTransfer {
                id,
                bus,
                chip_select,
                transfers,
            } => {
                let mut e = Encoder::new(SPI_TRANSFER);
                e.u32(*id)
                    .u32(*bus)
                    .u16(*chip_select)
                    .u16(transfers.len() as u16);
                for t in transfers {
                    e.u8(t.config.mode.bits())
                        .u32(t.config.speed_hz)
                        .u8(t.config.bits_per_word)
                        .bytes(&t.tx)
                        .u16(t.rx_len)
                        .u8(u8::from(t.cs_change))
                        .u16(t.delay_us);
                }
                e.finish()
            }
            Self::SpiLock {
                id,
                bus,
                chip_select,
                lock,
            } => {
                let mut e = Encoder::new(SPI_LOCK);
                e.u32(*id).u32(*bus).u16(*chip_select).u8(u8::from(*lock));
                e.finish()
            }
        }
    }

    pub(super) fn decode(kind: u16, d: &mut Decoder<'_>) -> Result<Self, WireError> {
        Ok(match kind {
            LINE_CLAIM => {
                let id = d.u32()?;
                let line = match d.u8()? {
                    0 => LineTarget::Chip {
                        chip: d.str()?,
                        offset: d.u32()?,
                    },
                    _ => {
                        let name = d.str()?;
                        d.u32()?;
                        LineTarget::Name(name)
                    }
                };
                Self::LineClaim {
                    id,
                    line,
                    config: read_line_config(d)?,
                    on_release: safe_from(d.u8()?),
                }
            }
            LINE_CONFIGURE => Self::LineConfigure {
                id: d.u32()?,
                handle: d.u32()?,
                config: read_line_config(d)?,
            },
            LINE_GET => Self::LineGet {
                id: d.u32()?,
                handle: d.u32()?,
            },
            LINE_SET => Self::LineSet {
                id: d.u32()?,
                handle: d.u32()?,
                value: d.u8()? != 0,
            },
            PWM_CLAIM => {
                let id = d.u32()?;
                let by_name = d.u8()? == 1;
                let chip = d.u32()?;
                let channel = d.u32()?;
                let name = d.str()?;
                Self::PwmClaim {
                    id,
                    pwm: if by_name {
                        PwmTarget::Name(name)
                    } else {
                        PwmTarget::Chip { chip, channel }
                    },
                }
            }
            PWM_CONFIGURE => Self::PwmConfigure {
                id: d.u32()?,
                handle: d.u32()?,
                config: PwmConfig {
                    period_ns: d.u64()?,
                    duty_ns: d.u64()?,
                    polarity: if d.u8()? == 1 {
                        Polarity::Inversed
                    } else {
                        Polarity::Normal
                    },
                    enabled: d.u8()? != 0,
                },
            },
            UNCLAIM => Self::Unclaim {
                id: d.u32()?,
                handle: d.u32()?,
            },
            I2C_TRANSFER => {
                let id = d.u32()?;
                let bus = d.str()?;
                let address = d.u16()?;
                let n = d.u16()?;
                let mut ops = Vec::with_capacity(usize::from(n));
                for _ in 0..n {
                    ops.push(match d.u8()? {
                        0 => I2cOp::Write(d.bytes()?),
                        _ => I2cOp::Read(u16::try_from(d.u32()?).map_err(|_| bad("I2C read"))?),
                    });
                }
                Self::I2cTransfer {
                    id,
                    bus,
                    address,
                    ops,
                }
            }
            I2C_LOCK => Self::I2cLock {
                id: d.u32()?,
                bus: d.str()?,
                address: d.u16()?,
                lock: d.u8()? != 0,
            },
            SPI_TRANSFER => {
                let id = d.u32()?;
                let bus = d.u32()?;
                let chip_select = d.u16()?;
                let n = d.u16()?;
                let mut transfers = Vec::with_capacity(usize::from(n));
                for _ in 0..n {
                    let config = SpiConfig {
                        mode: SpiMode::from_bits(d.u8()?),
                        speed_hz: d.u32()?,
                        bits_per_word: d.u8()?,
                    };
                    transfers.push(SpiXfer {
                        config,
                        tx: d.bytes()?,
                        rx_len: d.u16()?,
                        cs_change: d.u8()? != 0,
                        delay_us: d.u16()?,
                    });
                }
                Self::SpiTransfer {
                    id,
                    bus,
                    chip_select,
                    transfers,
                }
            }
            SPI_LOCK => Self::SpiLock {
                id: d.u32()?,
                bus: d.u32()?,
                chip_select: d.u16()?,
                lock: d.u8()? != 0,
            },
            other => return Err(bad(format!("unknown raw request {other}"))),
        })
    }

    /// The request id.
    pub fn id(&self) -> u32 {
        match self {
            Self::LineClaim { id, .. }
            | Self::LineConfigure { id, .. }
            | Self::LineGet { id, .. }
            | Self::LineSet { id, .. }
            | Self::PwmClaim { id, .. }
            | Self::PwmConfigure { id, .. }
            | Self::Unclaim { id, .. }
            | Self::I2cTransfer { id, .. }
            | Self::I2cLock { id, .. }
            | Self::SpiTransfer { id, .. }
            | Self::SpiLock { id, .. } => *id,
        }
    }
}
