//! A chip's facts, as text. The text is parsed once, at bind, into fixed arrays, so a read never
//! parses or allocates.
//!
//! Lines are `key words...`. `#` starts a comment, blank lines are skipped, and a number is decimal
//! or `0x` hex. The lines are:
//!
//! - `identity <register> <value>`: the identity register and the value it must read. Optional.
//! - `init <register> <value>`: a register write. Writes run in file order when the chip starts.
//! - `wait <milliseconds>`: the start-up stops here until that much time has passed.
//! - `axis <x|y|z> <register> <bits> <little|big> <signed|unsigned> <scale>`: where one axis of the
//!   vector sits, and its value in the contract's unit per count. Each of x, y and z is needed once.
//!
//! Registers are 8-bit. An axis is 8, 16, 24 or 32 bits, starting at its register. At most
//! [`MAX_STEPS`] identity, init and wait lines are allowed.

use lemnos_hal::register::Endian;

/// The most `init` and `wait` lines a description may hold.
pub const MAX_STEPS: usize = 32;

/// One start-up step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Write one byte.
    Write { register: u8, value: u8 },
    /// Stop here until this many milliseconds have passed.
    Wait { millis: u32 },
}

/// Where one axis of the vector is, and how to turn its counts into the contract's unit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Axis {
    pub register: u8,
    /// Width in bytes: 1 to 4.
    pub bytes: u8,
    pub endian: Endian,
    pub signed: bool,
    /// Contract units per count.
    pub scale: f32,
}

impl Axis {
    const ZERO: Self = Self {
        register: 0,
        bytes: 1,
        endian: Endian::Little,
        signed: false,
        scale: 0.0,
    };

    /// The value of raw register bytes read as one number, in the contract's unit.
    pub fn units(&self, raw: u32) -> f32 {
        let count = if self.signed {
            let shift = 32 - 8 * u32::from(self.bytes);
            (((raw << shift) as i32) >> shift) as f32
        } else {
            raw as f32
        };
        count * self.scale
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
    identity: Option<(u8, u8)>,
    steps: [Step; MAX_STEPS],
    len: usize,
    axes: [Axis; 3],
}

impl Chip {
    /// Parses a description. Every line is checked, so a description that parses is complete.
    pub fn parse(text: &str) -> Result<Self, ParseError> {
        let mut chip = Self {
            identity: None,
            steps: [Step::Wait { millis: 0 }; MAX_STEPS],
            len: 0,
            axes: [Axis::ZERO; 3],
        };
        let mut seen = [false; 3];
        for (index, raw) in text.lines().enumerate() {
            let line = raw.split('#').next().unwrap_or("");
            let mut words = line.split_whitespace();
            let Some(key) = words.next() else {
                continue;
            };
            line_into(&mut chip, &mut seen, key, &mut words).map_err(|reason| ParseError {
                line: index + 1,
                reason,
            })?;
        }
        if seen.iter().any(|&axis| !axis) {
            return Err(ParseError {
                line: 0,
                reason: "axis x, y and z are each needed",
            });
        }
        Ok(chip)
    }

    /// The identity register and the value it must read, if the description checks one.
    pub fn identity(&self) -> Option<(u8, u8)> {
        self.identity
    }

    /// The start-up steps, in order.
    pub fn steps(&self) -> &[Step] {
        &self.steps[..self.len]
    }

    /// The x, y and z axes.
    pub fn axes(&self) -> &[Axis; 3] {
        &self.axes
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

fn line_into<'a>(
    chip: &mut Chip,
    seen: &mut [bool; 3],
    key: &str,
    words: &mut impl Iterator<Item = &'a str>,
) -> Result<(), &'static str> {
    match key {
        "identity" => {
            if chip.identity.is_some() {
                return Err("identity given twice");
            }
            let register = byte(words.next())?;
            chip.identity = Some((register, byte(words.next())?));
        }
        "init" => {
            let step = Step::Write {
                register: byte(words.next())?,
                value: byte(words.next())?,
            };
            chip.push(step)?;
        }
        "wait" => {
            let millis = number(words.next())?;
            chip.push(Step::Wait { millis })?;
        }
        "axis" => {
            let slot = match words.next() {
                Some("x") => 0,
                Some("y") => 1,
                Some("z") => 2,
                _ => return Err("axis is x, y or z"),
            };
            if seen[slot] {
                return Err("axis given twice");
            }
            let register = byte(words.next())?;
            let bytes = match number(words.next())? {
                8 => 1,
                16 => 2,
                24 => 3,
                32 => 4,
                _ => return Err("bits is 8, 16, 24 or 32"),
            };
            let endian = match words.next() {
                Some("little") => Endian::Little,
                Some("big") => Endian::Big,
                _ => return Err("endian is little or big"),
            };
            let signed = match words.next() {
                Some("signed") => true,
                Some("unsigned") => false,
                _ => return Err("sign is signed or unsigned"),
            };
            let scale = words
                .next()
                .ok_or("missing scale")?
                .parse::<f32>()
                .ok()
                .filter(|scale| scale.is_finite())
                .ok_or("scale is not a finite number")?;
            seen[slot] = true;
            chip.axes[slot] = Axis {
                register,
                bytes,
                endian,
                signed,
                scale,
            };
        }
        _ => return Err("unknown line"),
    }
    if words.next().is_some() {
        return Err("extra text at the end of the line");
    }
    Ok(())
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
