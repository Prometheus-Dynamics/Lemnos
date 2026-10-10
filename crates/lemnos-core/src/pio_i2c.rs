//! Bus numbers for I2C buses bit-banged by the RP1 PIO block (`pio-i2c:` board
//! selectors). A PIO bus has no kernel adapter number, so it gets a virtual
//! one that carries its pins and clock: bit 31 set, SDA in bits 30:26, SCL in
//! bits 25:21, and the frequency in hertz in bits 20:0.

/// The flag bit that marks a virtual PIO bus number.
pub const PIO_I2C_BUS_FLAG: u32 = 1 << 31;
/// Frequency used when a selector gives none.
pub const PIO_I2C_DEFAULT_HZ: u32 = 400_000;
/// Highest frequency the 21-bit field carries.
pub const PIO_I2C_MAX_HZ: u32 = (1 << 21) - 1;

/// The pins and clock a PIO bus number stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PioI2cPins {
    /// The SDA GPIO.
    pub sda: u8,
    /// The SCL GPIO.
    pub scl: u8,
    /// The SCL frequency in hertz.
    pub hz: u32,
}

impl PioI2cPins {
    /// The virtual bus number for these pins (see the module docs).
    pub const fn bus_number(self) -> u32 {
        PIO_I2C_BUS_FLAG
            | ((self.sda as u32 & 0x1f) << 26)
            | ((self.scl as u32 & 0x1f) << 21)
            | (self.hz & PIO_I2C_MAX_HZ)
    }

    /// The pins of a virtual PIO bus number; `None` for a kernel adapter number.
    pub const fn from_bus_number(bus: u32) -> Option<Self> {
        if bus & PIO_I2C_BUS_FLAG == 0 {
            return None;
        }
        Some(Self {
            sda: ((bus >> 26) & 0x1f) as u8,
            scl: ((bus >> 21) & 0x1f) as u8,
            hz: bus & PIO_I2C_MAX_HZ,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bus_numbers_round_trip() {
        let pins = PioI2cPins {
            sda: 8,
            scl: 7,
            hz: 400_000,
        };
        let bus = pins.bus_number();
        assert!(bus & PIO_I2C_BUS_FLAG != 0);
        assert_eq!(PioI2cPins::from_bus_number(bus), Some(pins));
        assert_eq!(PioI2cPins::from_bus_number(1), None);
    }
}
