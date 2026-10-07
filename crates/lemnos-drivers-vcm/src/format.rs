use core::fmt;

/// Most power-up or power-down writes an `OwnedVcmFormat` (feature `alloc`)
/// lends to a [`VcmFormat`].
pub const MAX_VCM_WRITES: usize = 4;

/// VCM chips whose command formats are built in, plus [`VcmChip::Custom`]
/// for a chip described by its own [`VcmFormat`].
///
/// With the `serde` feature it (de)serializes as its description name:
/// `"dw9714"`, `"dw9807"`, `"dw9817"`, `"ak7375"` or `"custom"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum VcmChip {
    /// Dongwoon DW9714: two bytes, no register, `position << 4 | slew`
    /// (10 bits); bit 15 powers down.
    Dw9714,
    /// Dongwoon DW9807: register 0x03-0x04 (10 bits), control register 0x02
    /// (0 on, 1 off).
    Dw9807,
    /// Dongwoon DW9817 (Raspberry Pi Camera Module 3): the DW9807's register
    /// layout.
    Dw9817,
    /// Asahi Kasei AK7375: register 0x00-0x01 `position << 4` (12 bits),
    /// control register 0x02 (0 active, 0x40 standby).
    Ak7375,
    /// Another chip: a [`VcmFormat`] (or, with `alloc`, an
    /// `OwnedVcmFormat`) describes it. It has no built-in format.
    Custom,
}

impl VcmChip {
    /// The built-in chips.
    pub const BUILTIN: [Self; 4] = [Self::Dw9714, Self::Dw9807, Self::Dw9817, Self::Ak7375];

    /// The chip's built-in command format; `None` for [`VcmChip::Custom`].
    pub const fn builtin_format(self) -> Option<VcmFormat<'static>> {
        match self {
            Self::Custom => None,
            chip => Some(chip.format()),
        }
    }

    /// The chip's command format. [`VcmChip::Custom`] has none: it returns
    /// [`VcmFormat::UNSET`], which [`VcmFormat::check`] rejects with
    /// [`FormatError::Unset`]. Prefer [`Self::builtin_format`].
    pub const fn format(self) -> VcmFormat<'static> {
        match self {
            Self::Custom => VcmFormat::UNSET,
            Self::Dw9714 => VcmFormat {
                register: None,
                bytes: 2,
                shift: 4,
                bits: 10,
                or: 0,
                power_up: &[],
                power_up_us: 12_000,
                power_down: &[&[0x80, 0x00]],
            },
            Self::Dw9807 | Self::Dw9817 => VcmFormat {
                register: Some(0x03),
                bytes: 2,
                shift: 0,
                bits: 10,
                or: 0,
                power_up: &[&[0x02, 0x00]],
                power_up_us: 1_000,
                power_down: &[&[0x02, 0x01]],
            },
            Self::Ak7375 => VcmFormat {
                register: Some(0x00),
                bytes: 2,
                shift: 4,
                bits: 12,
                or: 0,
                power_up: &[&[0x02, 0x00]],
                power_up_us: 10_000,
                power_down: &[&[0x02, 0x40]],
            },
        }
    }

    /// The chip for a kernel driver or description name (`dw9714`,
    /// `dw9807-vcm`, `dw9817`, `ak7375`, `custom`).
    pub fn from_name(name: &str) -> Option<Self> {
        let base = name.split(['-', ' ']).next().unwrap_or(name);
        match base {
            "dw9714" => Some(Self::Dw9714),
            "dw9807" => Some(Self::Dw9807),
            "dw9817" => Some(Self::Dw9817),
            "ak7375" => Some(Self::Ak7375),
            "custom" => Some(Self::Custom),
            _ => None,
        }
    }

    /// The description name ([`Self::from_name`]'s input, the `serde` form).
    pub const fn name(self) -> &'static str {
        match self {
            Self::Dw9714 => "dw9714",
            Self::Dw9807 => "dw9807",
            Self::Dw9817 => "dw9817",
            Self::Ak7375 => "ak7375",
            Self::Custom => "custom",
        }
    }
}

/// How a position is written: `[register?] ((position << shift) | or)`,
/// big-endian in `bytes`, plus the power sequences.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VcmFormat<'a> {
    /// The register the value goes to (none: the value is the whole message).
    pub register: Option<u8>,
    /// Bytes of the value (1 or 2).
    pub bytes: u8,
    /// Bits the position is shifted left.
    pub shift: u8,
    /// Bits of the position.
    pub bits: u8,
    /// Constant bits or-ed in (mode, slew).
    pub or: u16,
    /// Raw messages that power the chip up.
    pub power_up: &'a [&'a [u8]],
    /// Microseconds to wait after power-up before the first move.
    pub power_up_us: u32,
    /// Raw messages that put the chip in standby.
    pub power_down: &'a [&'a [u8]],
}

/// Why a [`VcmFormat`] is unusable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FormatError {
    /// `bytes` is not 1 or 2, or `bits` is 0.
    Width,
    /// The shifted position does not fit `bytes`.
    Overflow,
    /// No format: a [`VcmChip::Custom`] chip without one.
    Unset,
    /// More power-up or power-down writes than an `OwnedVcmFormat` can lend
    /// ([`MAX_VCM_WRITES`]).
    TooManyWrites,
}

impl fmt::Display for FormatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Width => "bytes must be 1 or 2, bits at least 1",
            Self::Overflow => "position does not fit its bytes",
            Self::Unset => "a custom VCM chip needs a format",
            Self::TooManyWrites => "too many power-up or power-down writes",
        })
    }
}

impl core::error::Error for FormatError {}

impl VcmFormat<'_> {
    /// No format (all zero): what [`VcmChip::Custom`] has built in.
    pub const UNSET: VcmFormat<'static> = VcmFormat {
        register: None,
        bytes: 0,
        shift: 0,
        bits: 0,
        or: 0,
        power_up: &[],
        power_up_us: 0,
        power_down: &[],
    };

    /// The highest position.
    pub const fn max_position(&self) -> i32 {
        let bits = if self.bits > 16 { 16 } else { self.bits };
        (1i32 << bits) - 1
    }

    /// The message moving to `position` (clamped to `0..=max_position`):
    /// the bytes, their count, and the clamped position.
    pub fn encode(&self, position: i32) -> ([u8; 3], usize, i32) {
        let clamped = position.clamp(0, self.max_position());
        let v = ((clamped as u32) << self.shift) | u32::from(self.or);
        let mut out = [0u8; 3];
        let mut n = 0;
        if let Some(register) = self.register {
            out[n] = register;
            n += 1;
        }
        if self.bytes >= 2 {
            out[n] = (v >> 8) as u8;
            n += 1;
        }
        out[n] = v as u8;
        (out, n + 1, clamped)
    }

    /// Checks the format.
    pub fn check(&self) -> Result<(), FormatError> {
        if self.bytes == 0 && self.bits == 0 {
            return Err(FormatError::Unset);
        }
        if !(1..=2).contains(&self.bytes) || self.bits == 0 {
            return Err(FormatError::Width);
        }
        if u32::from(self.bits) + u32::from(self.shift) > 8 * u32::from(self.bytes) {
            return Err(FormatError::Overflow);
        }
        Ok(())
    }
}
