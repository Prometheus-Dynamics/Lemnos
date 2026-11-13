//! A chip's facts, as text. The text is parsed once, at bind, into fixed arrays, so a read never
//! parses or allocates.
//!
//! Lines are `key words...`. `#` starts a comment, blank lines are skipped, and a number is decimal
//! or `0x` hex. The lines are:
//!
//! - `modbus <holding|input>`: the chip is a Modbus RTU device on a UART, and its registers are
//!   that table's 16-bit registers. Optional, and it comes before every other line. Without it the
//!   chip's registers are 8-bit addresses on I2C.
//! - `identity <register> <value>`: the identity register and the value it must read. Optional.
//! - `init <register> <value> [<bits> <little|big>]`: a register write, one byte unless `bits` (8,
//!   16, 24 or 32) and a byte order say wider. Writes run in file order when the chip starts.
//! - `wait <milliseconds>`: the start-up stops here until that much time has passed.
//! - `axis <x|y|z> <register> <bits> <little|big> <signed|unsigned> <scale> [shift <n>] [offset <o>]`:
//!   one axis of a vector, for the vector contracts (magnetometer, accelerometer, gyroscope).
//! - `value <name> <register> <bits> <little|big> <signed|unsigned> <scale> [shift <n>] [offset <o>]`:
//!   one named value, for the other contracts. [`Kind::channels`] lists the names each contract
//!   takes, and every one of them is needed once.
//!
//! A channel's number is `bits` wide (1 to 32) and sits `shift` bits up from the bottom of the
//! bytes read at its register (default 0). As many bytes are read as `bits + shift` fills, at most
//! four, in the byte order given. A signed number is two's complement in its `bits`. Its value in
//! the contract's unit is `count * scale + offset` (offset default 0).
//!
//! On Modbus a register holds 16 bits, high byte first, so a channel reads one register, or two
//! when `bits + shift` passes 16. Then `big` means the register at the lower address holds the
//! high half, and `little` the low half. An `init` writes one register (function 6): its value is
//! up to 16 bits, and the width, if given, is `16 big`. `identity` reads one register.
//!
//! Registers are 8-bit on I2C and 16-bit on Modbus. At most [`MAX_STEPS`] init and wait lines are
//! allowed.

use lemnos_hal::register::Endian;

/// The most `init` and `wait` lines a description may hold.
pub const MAX_STEPS: usize = 32;

/// The most channels a contract reads.
pub const MAX_CHANNELS: usize = 4;

/// How the chip's registers are reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// 8-bit register addresses on I2C.
    I2c,
    /// A Modbus RTU device's holding registers (function 3), or its input registers (function 4)
    /// with `input`.
    Modbus { input: bool },
}

/// Which contract a description is read for. Each names the channels the file must give.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `magnetometer/1`: axes x, y, z in tesla.
    Magnetometer,
    /// `accelerometer/1`: axes x, y, z in m/s².
    Accelerometer,
    /// `gyroscope/1`: axes x, y, z in rad/s.
    Gyroscope,
    /// `temperature/1`: the value `temperature` in °C.
    Temperature,
    /// `power-monitor/1`: the values `bus_v`, `shunt_v` (volts), `current_a` (amps) and `power_w`
    /// (watts).
    PowerMonitor,
}

impl Kind {
    /// The line key its channels are given with: `axis` for a vector, `value` otherwise.
    pub const fn key(self) -> &'static str {
        match self {
            Self::Magnetometer | Self::Accelerometer | Self::Gyroscope => "axis",
            Self::Temperature | Self::PowerMonitor => "value",
        }
    }

    /// The channels, in the order a read answers them.
    pub const fn channels(self) -> &'static [&'static str] {
        match self {
            Self::Magnetometer | Self::Accelerometer | Self::Gyroscope => &["x", "y", "z"],
            Self::Temperature => &["temperature"],
            Self::PowerMonitor => &["bus_v", "shunt_v", "current_a", "power_w"],
        }
    }
}

/// One start-up step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Write `bytes` bytes of `value` in `endian` order, starting at `register`.
    Write {
        register: u16,
        value: u32,
        bytes: u8,
        endian: Endian,
    },
    /// Stop here until this many milliseconds have passed.
    Wait { millis: u32 },
}

/// Where one channel is, and how to turn its counts into the contract's unit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Channel {
    pub register: u16,
    /// Bytes read: 1 to 4. On Modbus, 2 or 4: one register or two.
    pub bytes: u8,
    /// Width of the number: 1 to 32.
    pub bits: u8,
    /// Bits below the number.
    pub shift: u8,
    pub endian: Endian,
    pub signed: bool,
    /// Contract units per count.
    pub scale: f32,
    /// Added after scaling, in contract units.
    pub offset: f32,
}

impl Channel {
    const ZERO: Self = Self {
        register: 0,
        bytes: 1,
        bits: 8,
        shift: 0,
        endian: Endian::Little,
        signed: false,
        scale: 0.0,
        offset: 0.0,
    };

    /// The value of raw register bytes read as one number, in the contract's unit.
    pub fn units(&self, raw: u32) -> f32 {
        let bits = u32::from(self.bits);
        let field = raw >> self.shift;
        let field = if bits == 32 {
            field
        } else {
            field & ((1u32 << bits) - 1)
        };
        let count = if self.signed {
            let up = 32 - bits;
            (((field << up) as i32) >> up) as f32
        } else {
            field as f32
        };
        count * self.scale + self.offset
    }
}

/// Why a description was refused. `line` is 1-based; 0 means the whole file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseError {
    pub line: usize,
    pub reason: &'static str,
}

/// A parsed description.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Chip {
    kind: Kind,
    transport: Transport,
    identity: Option<(u16, u16)>,
    steps: [Step; MAX_STEPS],
    len: usize,
    channels: [Channel; MAX_CHANNELS],
}

impl Chip {
    /// Parses a description for `kind`. Every line is checked, so a description that parses is
    /// complete.
    pub fn parse(text: &str, kind: Kind) -> Result<Self, ParseError> {
        let mut chip = Self {
            kind,
            transport: Transport::I2c,
            identity: None,
            steps: [Step::Wait { millis: 0 }; MAX_STEPS],
            len: 0,
            channels: [Channel::ZERO; MAX_CHANNELS],
        };
        let mut seen = [false; MAX_CHANNELS];
        let mut first = true;
        for (index, raw) in text.lines().enumerate() {
            let line = raw.split('#').next().unwrap_or("");
            let mut words = line.split_whitespace();
            let Some(key) = words.next() else {
                continue;
            };
            let result = if key == "modbus" {
                transport_into(&mut chip, first, &mut words)
            } else {
                line_into(&mut chip, &mut seen, key, &mut words)
            };
            result.map_err(|reason| ParseError {
                line: index + 1,
                reason,
            })?;
            first = false;
        }
        if seen[..kind.channels().len()].iter().any(|&given| !given) {
            return Err(ParseError {
                line: 0,
                reason: match kind.key() {
                    "axis" => "axis x, y and z are each needed",
                    _ => "a value the contract reads is missing",
                },
            });
        }
        Ok(chip)
    }

    /// The contract the description was read for.
    pub fn kind(&self) -> Kind {
        self.kind
    }

    /// How the chip's registers are reached.
    pub fn transport(&self) -> Transport {
        self.transport
    }

    /// The identity register and the value it must read, if the description checks one.
    pub fn identity(&self) -> Option<(u16, u16)> {
        self.identity
    }

    /// The start-up steps, in order.
    pub fn steps(&self) -> &[Step] {
        &self.steps[..self.len]
    }

    /// The channels, in the order of [`Kind::channels`].
    pub fn channels(&self) -> &[Channel] {
        &self.channels[..self.kind.channels().len()]
    }

    fn push(&mut self, step: Step) -> Result<(), &'static str> {
        if self.len == MAX_STEPS {
            return Err("too many init and wait lines");
        }
        self.steps[self.len] = step;
        self.len += 1;
        Ok(())
    }
}

/// The `modbus` line.
fn transport_into<'a>(
    chip: &mut Chip,
    first: bool,
    words: &mut impl Iterator<Item = &'a str>,
) -> Result<(), &'static str> {
    if !first {
        return Err("modbus comes before every other line");
    }
    let input = match words.next() {
        Some("holding") => false,
        Some("input") => true,
        _ => return Err("modbus is holding or input"),
    };
    if words.next().is_some() {
        return Err("extra text at the end of the line");
    }
    chip.transport = Transport::Modbus { input };
    Ok(())
}

fn line_into<'a>(
    chip: &mut Chip,
    seen: &mut [bool; MAX_CHANNELS],
    key: &str,
    words: &mut impl Iterator<Item = &'a str>,
) -> Result<(), &'static str> {
    match key {
        "identity" => {
            if chip.identity.is_some() {
                return Err("identity given twice");
            }
            let register = register(words.next(), chip.transport)?;
            chip.identity = Some((register, value(words.next(), chip.transport)?));
        }
        "init" => {
            let register = register(words.next(), chip.transport)?;
            let value = number(words.next())?;
            let width = match words.next() {
                None => None,
                Some(bits) => {
                    let bytes = match number(Some(bits))? {
                        8 => 1,
                        16 => 2,
                        24 => 3,
                        32 => 4,
                        _ => return Err("an init's bits is 8, 16, 24 or 32"),
                    };
                    Some((bytes, endian_of(words.next())?))
                }
            };
            let (bytes, endian) = match chip.transport {
                Transport::I2c => width.unwrap_or((1, Endian::Big)),
                Transport::Modbus { .. } => {
                    if value > 0xffff {
                        return Err("a modbus register or value is above 65535");
                    }
                    match width {
                        None | Some((2, Endian::Big)) => (2, Endian::Big),
                        Some(_) => return Err("a modbus init writes one 16-bit register"),
                    }
                }
            };
            if bytes < 4 && value >> (8 * u32::from(bytes)) != 0 {
                return Err("a register or value is wider than its bits");
            }
            chip.push(Step::Write {
                register,
                value,
                bytes,
                endian,
            })?;
        }
        "wait" => {
            let millis = number(words.next())?;
            chip.push(Step::Wait { millis })?;
        }
        "axis" | "value" => {
            if key != chip.kind.key() {
                return Err(match key {
                    "axis" => "this contract takes value lines, not axis lines",
                    _ => "this contract takes axis lines, not value lines",
                });
            }
            let name = words.next().ok_or("missing name")?;
            let slot = chip
                .kind
                .channels()
                .iter()
                .position(|known| *known == name)
                .ok_or(match key {
                    "axis" => "axis is x, y or z",
                    _ => "not a value this contract reads",
                })?;
            if seen[slot] {
                return Err(match key {
                    "axis" => "axis given twice",
                    _ => "value given twice",
                });
            }
            chip.channels[slot] = channel(words, chip.transport)?;
            seen[slot] = true;
            // `channel` takes the options to the end of the line.
            return Ok(());
        }
        _ => return Err("unknown line"),
    }
    finish(words)
}

/// Nothing may follow a line's last word.
fn finish<'a>(words: &mut impl Iterator<Item = &'a str>) -> Result<(), &'static str> {
    if words.next().is_some() {
        return Err("extra text at the end of the line");
    }
    Ok(())
}

/// The rest of an `axis` or `value` line: where the number is, and its scale.
fn channel<'a>(
    words: &mut impl Iterator<Item = &'a str>,
    transport: Transport,
) -> Result<Channel, &'static str> {
    let register = register(words.next(), transport)?;
    let bits = number(words.next())?;
    if !(1..=32).contains(&bits) {
        return Err("bits is 1 to 32");
    }
    let endian = endian_of(words.next())?;
    let signed = match words.next() {
        Some("signed") => true,
        Some("unsigned") => false,
        _ => return Err("sign is signed or unsigned"),
    };
    let scale = finite(words.next(), "missing scale")?;
    let mut shift = None;
    let mut offset = None;
    while let Some(option) = words.next() {
        match option {
            "shift" if shift.is_none() => shift = Some(number(words.next())?),
            "offset" if offset.is_none() => offset = Some(finite(words.next(), "missing offset")?),
            "shift" | "offset" => return Err("an option is given twice"),
            _ => return Err("extra text at the end of the line"),
        }
    }
    let shift = shift.unwrap_or(0);
    let span = bits.checked_add(shift).filter(|span| *span <= 32);
    let Some(span) = span else {
        return Err("bits and shift together are more than 32");
    };
    let bytes = match transport {
        Transport::I2c => span.div_ceil(8),
        Transport::Modbus { .. } => 2 * span.div_ceil(16),
    };
    Ok(Channel {
        register,
        bytes: bytes as u8,
        bits: bits as u8,
        shift: shift as u8,
        endian,
        signed,
        scale,
        offset: offset.unwrap_or(0.0),
    })
}

fn endian_of(word: Option<&str>) -> Result<Endian, &'static str> {
    match word {
        Some("little") => Ok(Endian::Little),
        Some("big") => Ok(Endian::Big),
        _ => Err("endian is little or big"),
    }
}

fn finite(word: Option<&str>, missing: &'static str) -> Result<f32, &'static str> {
    word.ok_or(missing)?
        .parse::<f32>()
        .ok()
        .filter(|value| value.is_finite())
        .ok_or("scale or offset is not a finite number")
}

fn number(word: Option<&str>) -> Result<u32, &'static str> {
    let word = word.ok_or("missing number")?;
    match word.strip_prefix("0x") {
        Some(hex) => u32::from_str_radix(hex, 16),
        None => word.parse(),
    }
    .map_err(|_| "not a number")
}

fn byte(word: Option<&str>) -> Result<u8, &'static str> {
    u8::try_from(number(word)?).map_err(|_| "a register or value is above 255")
}

/// A register: a byte on I2C, 16 bits on Modbus.
fn register(word: Option<&str>, transport: Transport) -> Result<u16, &'static str> {
    match transport {
        Transport::I2c => byte(word).map(u16::from),
        Transport::Modbus { .. } => {
            u16::try_from(number(word)?).map_err(|_| "a modbus register or value is above 65535")
        }
    }
}

/// A register's value: as wide as one register.
fn value(word: Option<&str>, transport: Transport) -> Result<u16, &'static str> {
    register(word, transport)
}
