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
    /// The form the output takes. `overlay` (the default) is a `/plugin/;` overlay for a loader
    /// that applies overlays; `fragment` is blocks a board's own `.dts` takes, for a loader that
    /// boots a plain device tree.
    #[serde(default)]
    pub form: Form,
    /// The GPIO chips a `gpio:LABEL:LINE` name may name, by a short key. The label is the chip's
    /// own, as the kernel reports it.
    #[serde(default)]
    pub chips: BTreeMap<String, Chip>,
}

/// What the overlay output is: the form a loader applies, which the map says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Form {
    /// A `/plugin/;` overlay, with `target-path` fragments.
    #[default]
    Overlay,
    /// Fragments for a board's `.dts`: `&{/path} { ... };` blocks.
    Fragment,
}

/// A GPIO chip: the label `gpio:LABEL:LINE` names it by.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Chip {
    /// The label the kernel gives the chip (`GPIO_GET_CHIPINFO`'s label).
    pub label: String,
    /// What the chip is, for a message that names it.
    #[serde(default)]
    pub reason: String,
    /// The SoC's GPIO number of the chip's line 0, which the boot fragment's `gpio=N` takes. Zero
    /// for a chip numbered from 0 (the Raspberry Pi's RP1); a bank of a Rockchip SoC is 32 times its
    /// bank number.
    #[serde(default)]
    pub number_base: u32,
}

/// The kernel module's options a node may be given, besides `owned`, which every module-owned
/// node has. Each is a comma-separated list of `vendor,name` pairs in the module's own parameter.
pub const MODULE_ROLES: &[&str] = &["pisp_be", "gsdma", "a733_ve", "k230_isp", "vip9000"];

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
    /// A block the kernel module drives, enabled with no pin set and no bus of its own: its
    /// board entry makes the module's option for it, and nothing else.
    Block,
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
            Self::Block => "block",
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
    /// The name the kernel gives the platform device. For the module-ownership check; and for a
    /// `usb-peripheral` controller, the UDC name a gadget's `udc:` names: the controller is listed
    /// under `/sys/class/udc` by it. Required for that kind.
    #[serde(default)]
    pub sysfs: Option<String>,
    /// The stock driver the kernel would bind to the node. Bound, it means the module does not own
    /// the node.
    #[serde(default)]
    pub stock_driver: Option<String>,
    /// The pin sets the controller can be wired through, by name.
    #[serde(default)]
    pub pins: BTreeMap<String, PinSet>,
    /// The node's compatible string, `vendor,name`: what the kernel module's `owned=` parameter
    /// takes to claim it. Needed for a controller a board gives to the module.
    #[serde(default)]
    pub compatible: Option<String>,
    /// The module roles this node may be given besides `owned`, from [`MODULE_ROLES`].
    #[serde(default)]
    pub module_roles: Vec<String>,
    /// Other nodes the controller needs enabled to work: its PHY and PHY ports, say. Each is
    /// enabled with the controller when the board entry takes it: an entry with no `option` is
    /// always enabled, and one with an `option` only when the board names that option.
    #[serde(default)]
    pub needs: Vec<Need>,
    /// Stock kernel modules that bind this node when they load. The generator writes a
    /// `blacklist` for each one a board gives to the module, so it is not loaded for the node.
    #[serde(default)]
    pub stock_modules: Vec<String>,
    /// Why the generated overlay enables this node. Written beside it.
    pub reason: String,
}

/// A node a controller needs enabled, and the board option that asks for it, when it needs one. A
/// PHY the controller is linked to is a need with `phy` (its `phy-names` entry) and `label` (its label
/// in the base tree, which the overlay refers to); the controller's `phys` lists the PHYs its entry
/// takes, in the order the map gives them.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Need {
    /// The node's path in the base tree.
    pub path: String,
    /// The board's option that enables this node, from the entry's `options`. Left out, the node
    /// is always enabled with the controller.
    #[serde(default)]
    pub option: Option<String>,
    /// The PHY's name in the controller's `phy-names` (`usb2-phy`, `usb3-phy`), when the controller
    /// is linked to it. Needs `label`.
    #[serde(default)]
    pub phy: Option<String>,
    /// The PHY node's label in the base tree, which the controller's `phys` refers to.
    #[serde(default)]
    pub label: Option<String>,
    /// The PHY specifier's cells after the phandle (`PHY_TYPE_USB3` is 4), when the PHY has any.
    #[serde(default)]
    pub args: Vec<u32>,
    /// Why the node is needed. Written beside the overlay's fragment.
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

/// A base tree's own nodes that matter to a board: the nodes that hold a line, and the VBUS supply
/// each USB device port has in the base tree.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Base {
    /// Each line (`rp1:34`) and the base-tree nodes that drive it.
    #[serde(default)]
    pub lines: BTreeMap<String, LineHolders>,
    /// The VBUS supply of each USB device port (a `usb-peripheral` controller of the map), by the
    /// controller's name. A port the base does not describe has no fact here, and the generator
    /// then refuses it unless the board file states how its VBUS is handled.
    #[serde(default)]
    pub vbus: BTreeMap<String, Vbus>,
}

/// What the base tree says about the VBUS supply of one device port: the regulator its USB 2 PHY's
/// OTG port names in `phy-supply` (a host VBUS supply on a device port is a hazard, so the generator
/// replaces it), and how that regulator is switched. Every fact is the base tree's, not the board's.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Vbus {
    /// The base-tree regulator the port's `phy-supply` names, by its path. Left out: the port has
    /// no VBUS supply in the base tree.
    #[serde(default)]
    pub regulator: Option<String>,
    /// The regulator's `regulator-name`, which the kernel lists it under in `/sys/class/regulator`.
    /// Needed with `regulator`.
    #[serde(default)]
    pub regulator_name: Option<String>,
    /// The regulator is `regulator-always-on`: the kernel keeps it on whatever the PHY does, so
    /// detaching `phy-supply` does not stop VBUS. Exclusive with `switch`.
    #[serde(default)]
    pub always_on: bool,
    /// The GPIO line that switches the regulator (its enable pin), as `gpio:LABEL:LINE`. Exclusive
    /// with `always_on`. Needs `regulator`.
    #[serde(default)]
    pub switch: Option<String>,
    /// Why the base tree has this supply, for a message that names it.
    pub reason: String,
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
        for (key, chip) in &map.chips {
            if chip.label.is_empty() || chip.label.contains([':', '/', ',', ' ']) {
                return Err(format!(
                    "chip {key:?}: label {:?} is not a chip label",
                    chip.label
                ));
            }
        }
        for (name, controller) in &map.controllers {
            if !controller.path.starts_with('/') {
                return Err(format!("controller {name:?}: its path must be absolute"));
            }
            if controller.kind == Kind::UsbPeripheral && controller.sysfs.is_none() {
                return Err(format!(
                    "controller {name:?}: a usb-peripheral controller needs `sysfs`, the UDC name \
                     its gadget's `udc:` names"
                ));
            }
            for need in &controller.needs {
                if !need.path.starts_with('/') {
                    return Err(format!(
                        "controller {name:?}: needed node {:?} must have an absolute path",
                        need.path
                    ));
                }
                if need.option.as_deref() == Some("") {
                    return Err(format!(
                        "controller {name:?}: a needed node has an empty option"
                    ));
                }
                match (&need.phy, &need.label) {
                    (None, None) if need.args.is_empty() => {}
                    (Some(_), Some(label))
                        if !label.is_empty()
                            && label
                                .bytes()
                                .all(|b| b.is_ascii_alphanumeric() || b == b'_') => {}
                    _ => {
                        return Err(format!(
                            "controller {name:?}: needed node {:?} has `phy` and `label` together, \
                             a label of letters, digits and _, or else neither (and no args)",
                            need.path
                        ));
                    }
                }
            }
            if let Some(compatible) = &controller.compatible {
                let pair = compatible.split(',').collect::<Vec<_>>();
                let valid = pair.len() == 2
                    && pair.iter().all(|part| {
                        !part.is_empty()
                            && part.bytes().all(|b| {
                                b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'+')
                            })
                    });
                if !valid {
                    return Err(format!(
                        "controller {name:?}: compatible {compatible:?} is not vendor,name"
                    ));
                }
            }
            for role in &controller.module_roles {
                if !MODULE_ROLES.contains(&role.as_str()) {
                    return Err(format!(
                        "controller {name:?}: module role {role:?} is not one of {}",
                        MODULE_ROLES.join(", ")
                    ));
                }
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
        for (base_name, base) in &map.bases {
            for (port, vbus) in &base.vbus {
                let who = format!("base {base_name:?} port {port:?}");
                match map.controllers.get(port) {
                    Some(c) if c.kind == Kind::UsbPeripheral => {}
                    Some(c) => {
                        return Err(format!(
                            "{who}: the controller is {}, and only a usb-peripheral port has a VBUS fact",
                            c.kind.name()
                        ));
                    }
                    None => return Err(format!("{who}: the map has no such controller")),
                }
                match &vbus.regulator {
                    None => {
                        if vbus.always_on || vbus.switch.is_some() || vbus.regulator_name.is_some()
                        {
                            return Err(format!(
                                "{who}: no regulator, so no always_on, switch or regulator_name"
                            ));
                        }
                    }
                    Some(path) => {
                        if !path.starts_with('/') {
                            return Err(format!("{who}: the regulator's path must be absolute"));
                        }
                        if vbus
                            .regulator_name
                            .as_deref()
                            .unwrap_or_default()
                            .is_empty()
                        {
                            return Err(format!("{who}: a regulator needs its regulator_name"));
                        }
                        if vbus.always_on && vbus.switch.is_some() {
                            return Err(format!(
                                "{who}: a regulator is always_on or switched by a line, not both"
                            ));
                        }
                    }
                }
                if let Some(line) = &vbus.switch {
                    let label = line
                        .strip_prefix("gpio:")
                        .and_then(|rest| rest.rsplit_once(':'))
                        .map(|(label, _)| label)
                        .unwrap_or_default();
                    if map.chip_labelled(label).is_none() {
                        return Err(format!(
                            "{who}: switch {line:?} names no GPIO chip of the map; it has: {}",
                            map.chip_labels()
                        ));
                    }
                }
                if vbus.reason.is_empty() {
                    return Err(format!("{who}: a VBUS fact needs its reason"));
                }
            }
        }
        Ok(map)
    }

    /// The GPIO chip labelled `label`, if the map has one.
    pub fn chip_labelled(&self, label: &str) -> Option<&Chip> {
        self.chips.values().find(|chip| chip.label == label)
    }

    /// The labels of the map's GPIO chips, for a message that lists what the map has.
    pub fn chip_labels(&self) -> String {
        if self.chips.is_empty() {
            return "no GPIO chips".into();
        }
        self.chips
            .values()
            .map(|chip| chip.label.clone())
            .collect::<Vec<_>>()
            .join(", ")
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
