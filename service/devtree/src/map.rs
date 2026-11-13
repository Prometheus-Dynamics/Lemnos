//! The SoC map: what each controller of a SoC is, the node that enables it and its pin sets.

use std::collections::BTreeMap;

use serde::Deserialize;

/// A SoC's map, read from `socs/<soc>.toml`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SocMap {
    /// The SoC's name; a board file's `[board] soc` must match it.
    pub soc: String,
    /// The controllers a board file may name, by their names in the map.
    #[serde(default)]
    pub controllers: BTreeMap<String, Controller>,
    /// The base trees a board may be built on, by name.
    #[serde(default)]
    pub bases: BTreeMap<String, Base>,
}

/// What a controller is. Each kind is enabled the same way (its node's status) with what its
/// board entry needs on top.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    /// An I2C adapter. Its `alias` is `i2cN`, and that number is the board's `i2c-N`.
    I2c,
    /// An SPI controller. Its `alias` is `spiN`, and that number is the board's `spidevN.C`.
    Spi,
    /// A serial port with its pin set.
    Uart,
    /// A PWM block, with the pin set of each channel's pin.
    Pwm,
    /// The PIO block: no pin set, the driver claims its pins at run time.
    Pio,
    /// A CSI-2 receiver, enabled with no sensor node below it.
    Csi,
    /// A USB controller in peripheral mode.
    UsbPeripheral,
    /// The hardware watchdog.
    Watchdog,
}

impl Kind {
    /// The name a board file and the map's errors use for the kind.
    pub const fn name(self) -> &'static str {
        match self {
            Self::I2c => "i2c",
            Self::Spi => "spi",
            Self::Uart => "uart",
            Self::Pwm => "pwm",
            Self::Pio => "pio",
            Self::Csi => "csi",
            Self::UsbPeripheral => "usb-peripheral",
            Self::Watchdog => "watchdog",
        }
    }

    /// Whether a board entry of this kind must name a pin set.
    pub const fn needs_pins(self) -> bool {
        matches!(self, Self::Pwm | Self::Uart)
    }
}

/// One controller of the SoC.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Controller {
    pub kind: Kind,
    /// The node's path in the base tree.
    pub path: String,
    /// The alias the kernel numbers the controller by (`i2c1`, `spi0`), when it has one.
    #[serde(default)]
    pub alias: Option<String>,
    /// The name the kernel gives the platform device, for the module-ownership check.
    #[serde(default)]
    pub sysfs: Option<String>,
    /// The stock driver the kernel would bind to the node. Bound, it means the module does not own
    /// the node.
    #[serde(default)]
    pub stock_driver: Option<String>,
    /// The pin sets the controller can be wired through, by name.
    #[serde(default)]
    pub pins: BTreeMap<String, PinSet>,
    /// Why the generated overlay enables this node. Written beside it.
    pub reason: String,
}

/// A pin set: the pin configuration node a controller's `pinctrl-0` names, and the GPIOs it uses.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PinSet {
    pub path: String,
    /// The node's label in the base tree, which an overlay refers to.
    pub label: String,
    pub gpios: Vec<u16>,
}

/// A base tree's own nodes that matter to a board: the nodes that hold a line.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Base {
    /// Each line (`rp1:34`) and the base-tree nodes that drive it.
    #[serde(default)]
    pub lines: BTreeMap<String, LineHolders>,
}

/// The base-tree nodes that hold one line.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LineHolders {
    pub holders: Vec<String>,
    pub reason: String,
}

impl SocMap {
    /// Parses a map and checks what the generator relies on: paths are absolute, a controller's
    /// alias matches its kind's form, and no two pin sets of one controller share a GPIO.
    pub fn parse(text: &str) -> Result<Self, String> {
        let map: Self = toml::from_str(text).map_err(|e| format!("soc map: {e}"))?;
        for (name, controller) in &map.controllers {
            if !controller.path.starts_with('/') {
                return Err(format!("controller {name:?}: its path must be absolute"));
            }
            if let (Some(alias), Kind::I2c | Kind::Spi) = (&controller.alias, controller.kind) {
                let prefix = if controller.kind == Kind::I2c {
                    "i2c"
                } else {
                    "spi"
                };
                let number = alias
                    .strip_prefix(prefix)
                    .is_some_and(|n| n.parse::<u32>().is_ok());
                if !number {
                    return Err(format!(
                        "controller {name:?}: alias {alias:?} is not {prefix}N for a {} controller",
                        controller.kind.name()
                    ));
                }
            }
            let mut seen = Vec::new();
            for (pins, set) in &controller.pins {
                if !set.path.starts_with('/') {
                    return Err(format!(
                        "controller {name:?} pin set {pins:?}: absolute path"
                    ));
                }
                for gpio in &set.gpios {
                    if seen.contains(gpio) {
                        return Err(format!(
                            "controller {name:?}: GPIO {gpio} is in two pin sets"
                        ));
                    }
                    seen.push(*gpio);
                }
            }
        }
        Ok(map)
    }

    /// The controllers' names, for a message that lists what the map has.
    pub fn controller_names(&self) -> String {
        self.controllers
            .keys()
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    }
}
