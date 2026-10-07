use core::fmt;

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "lowercase"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum InterfaceKind {
    Gpio,
    Pwm,
    I2c,
    Spi,
    Uart,
    Usb,
    /// No bus session: an on-chip or kernel-managed device reached through a
    /// class interface (a thermal zone, an IIO or hwmon sensor).
    Platform,
}

impl InterfaceKind {
    pub const ALL: [Self; 7] = [
        Self::Gpio,
        Self::Pwm,
        Self::I2c,
        Self::Spi,
        Self::Uart,
        Self::Usb,
        Self::Platform,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Gpio => "gpio",
            Self::Pwm => "pwm",
            Self::I2c => "i2c",
            Self::Spi => "spi",
            Self::Uart => "uart",
            Self::Usb => "usb",
            Self::Platform => "platform",
        }
    }
}

impl fmt::Display for InterfaceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
