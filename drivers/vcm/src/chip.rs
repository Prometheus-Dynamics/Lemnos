//! The voice-coil motor chip layer: the built-in command formats, position
//! encoding and the power sequences. It holds no bus; each operation is given
//! one.
//!
//! Positions are written as one 16-bit value, `position << shift`, big-endian,
//! behind the chip's register when it has one. The formats are those of the
//! DW9714, DW9807, DW9817 and AK7375 datasheets.

use lemnos_hal::i2c::I2c;

/// The usual 7-bit address of these chips.
pub const DEFAULT_ADDRESS: u8 = 0x0c;

/// A supported chip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Chip {
    /// DW9714: two bytes, no register, `position << 4` (10 bits). Bit 15 of
    /// the value powers down.
    Dw9714,
    /// DW9807: register 0x03 (10 bits), control register 0x02 (0 on, 1 off).
    Dw9807,
    /// DW9817: the DW9807's register layout.
    Dw9817,
    /// AK7375: register 0x00, `position << 4` (12 bits), control register 0x02
    /// (0 active, 0x40 standby).
    Ak7375,
}

impl Chip {
    /// The chip for a description name (`dw9714`, `dw9807`, `dw9817`,
    /// `ak7375`, any case).
    pub fn from_name(name: &str) -> Option<Self> {
        if name.eq_ignore_ascii_case("dw9714") {
            Some(Self::Dw9714)
        } else if name.eq_ignore_ascii_case("dw9807") {
            Some(Self::Dw9807)
        } else if name.eq_ignore_ascii_case("dw9817") {
            Some(Self::Dw9817)
        } else if name.eq_ignore_ascii_case("ak7375") {
            Some(Self::Ak7375)
        } else {
            None
        }
    }

    /// The command format of the chip.
    pub const fn format(self) -> Format {
        match self {
            Self::Dw9714 => Format {
                register: None,
                shift: 4,
                bits: 10,
                power_up: &[],
                power_up_us: 12_000,
                power_down: &[&[0x80, 0x00]],
            },
            Self::Dw9807 | Self::Dw9817 => Format {
                register: Some(0x03),
                shift: 0,
                bits: 10,
                power_up: &[&[0x02, 0x00]],
                power_up_us: 1_000,
                power_down: &[&[0x02, 0x01]],
            },
            Self::Ak7375 => Format {
                register: Some(0x00),
                shift: 4,
                bits: 12,
                power_up: &[&[0x02, 0x00]],
                power_up_us: 10_000,
                power_down: &[&[0x02, 0x40]],
            },
        }
    }
}

/// How a position is written and how the chip is powered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Format {
    /// The register the value goes to; none means the value is the message.
    pub register: Option<u8>,
    /// Bits the position is shifted left within the 16-bit value.
    pub shift: u8,
    /// Bits of the position; the highest position is `2^bits - 1`.
    pub bits: u8,
    /// Raw messages that power the chip up.
    pub power_up: &'static [&'static [u8]],
    /// Microseconds the chip needs after power-up before its first move.
    pub power_up_us: u32,
    /// Raw messages that put the chip in standby.
    pub power_down: &'static [&'static [u8]],
}

/// One encoded move: up to three bytes, the first `len` of them sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Message {
    bytes: [u8; 3],
    len: usize,
}

impl Message {
    pub fn as_slice(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

impl Format {
    /// The highest position the chip takes.
    pub const fn max_position(&self) -> u16 {
        ((1u32 << self.bits) - 1) as u16
    }

    /// The message that moves to `position`, clamped to the chip's range,
    /// and the position it moves to.
    pub fn encode(&self, position: u16) -> (Message, u16) {
        let clamped = position.min(self.max_position());
        let value = clamped << self.shift;
        let mut bytes = [0u8; 3];
        let mut len = 0;
        if let Some(register) = self.register {
            bytes[len] = register;
            len += 1;
        }
        bytes[len] = (value >> 8) as u8;
        bytes[len + 1] = value as u8;
        len += 2;
        (Message { bytes, len }, clamped)
    }
}

/// Writes the power-up messages. The caller waits `power_up_us` before the
/// first move.
pub fn power_up<B: I2c>(bus: &mut B, address: u8, format: &Format) -> Result<(), B::Error> {
    for message in format.power_up {
        bus.write(address, message)?;
    }
    Ok(())
}

/// Writes the standby messages.
pub fn power_down<B: I2c>(bus: &mut B, address: u8, format: &Format) -> Result<(), B::Error> {
    for message in format.power_down {
        bus.write(address, message)?;
    }
    Ok(())
}

/// Writes the move to `position` (clamped to the chip's range); returns the
/// position written.
pub fn move_to<B: I2c>(
    bus: &mut B,
    address: u8,
    format: &Format,
    position: u16,
) -> Result<u16, B::Error> {
    let (message, written) = format.encode(position);
    bus.write(address, message.as_slice())?;
    Ok(written)
}
