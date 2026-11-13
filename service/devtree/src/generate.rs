//! Writes the overlay and the boot fragment for a board. Every node in them has its reason beside
//! it, and every refusal names the board entry, the map, and what the map has instead.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use lemnos_core::{BusKind, Manifest, NeedKind};
use lemnos_host::board::{BoardFile, LineSpec};

use crate::map::{Base, Controller, Form, Kind, PinSet, SocMap, Vbus};

/// The label of the stand-in regulator the overlay gives every device port's PHY as its `phy-supply`.
pub const NO_VBUS_LABEL: &str = "lemnos_no_vbus";
/// The stand-in's node, at the root of the tree. A fixed regulator with no enable pin, no GPIO and
/// no voltage: it is named by no hardware, so it supplies nothing.
pub const NO_VBUS_NODE: &str = "/regulator-lemnos-no-vbus";

/// What the board says about a device port's VBUS, once the generator has checked it against the
/// base tree's fact (see [`vbus_plan`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VbusPlan {
    /// The base has no supply on the port: nothing to hold.
    NoSupply,
    /// VBUS is switched by a line: the generator holds it at `off` (`"low"` or `"high"`) from power-on.
    Hold {
        line: String,
        off: String,
        reason: String,
        /// The base regulator the line switches, when the base says so: the overlay disables it.
        regulator: Option<String>,
    },
    /// The board author asserts VBUS is not driven on this port: nothing to hold, and nothing checked
    /// on the line.
    Undriven { reason: String },
}

/// What the generator makes: the overlay the OS layer compiles and loads, and the boot fragment
/// the firmware reads before the kernel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    /// Device-tree source: an overlay (`/plugin/;`), fragments by `target-path`.
    pub overlay: String,
    /// Firmware configuration lines, one `gpio=N=op,dhN` per line the board gives a boot state.
    pub boot: String,
    /// The U-Boot fragment: `gpio set`/`gpio clear` per held line, the same lines and levels as
    /// `boot` and the overlay's hogs. Refused by name (`Err`) when the map gives no U-Boot naming.
    pub uboot: Result<String, String>,
    /// The kernel module's configuration: `options lemnos` for the nodes the board gives to the
    /// module, and a `blacklist` for each stock module that binds one of them.
    pub modprobe: String,
}

/// One board entry the generator will enable: the entry, its controller and its pin set.
struct Chosen<'a> {
    name: &'a str,
    /// The controller's name in the map.
    key: &'a str,
    controller: &'a Controller,
    pins: Option<&'a PinSet>,
    /// The module's role for the node, when the module drives it.
    module: Option<&'a str>,
    unconfined: bool,
    confined: bool,
    /// The options the board entry names, each one the map gives the controller.
    options: Vec<&'a str>,
    /// For a device port: what its VBUS is, as the board says and the base tree allows.
    vbus: Option<VbusPlan>,
}

/// Makes the overlay and the boot fragment for `board`, using `map`. `drivers` are the manifests of
/// the drivers the tool knows (the built-in list, each driver's own declared needs): a device whose
/// driver declares no hardware need makes nothing by definition, and one whose driver does must name
/// it. Refuses what the map cannot provide, with the entry that asks for it.
pub fn generate(board: &BoardFile, map: &SocMap, drivers: &[Manifest]) -> Result<Output, String> {
    let soc = board
        .board
        .soc
        .as_deref()
        .ok_or("[board] soc is not set: name the SoC map this board is built for")?;
    if soc != map.soc {
        return Err(format!(
            "[board] soc is {soc:?}, but the map is for {:?}",
            map.soc
        ));
    }
    let base = base_of(board, map)?;
    let chosen = choose(board, map, base)?;
    check_lines(board, map)?;
    check_devices(board, map, &chosen, base.map(|(_, b)| b), drivers)?;
    let holds = holds(board, &chosen)?;
    let hogs = hogs(board, map, &chosen, &holds)?;
    let overlay = overlay(board, &chosen, base, &board.lines, &holds, &hogs, map.form)?;
    let boot = boot(board, &holds, map)?;
    let uboot = uboot(board, &holds, map);
    let modprobe = modprobe(board, &chosen);
    Ok(Output {
        overlay,
        boot,
        uboot,
        modprobe,
    })
}

/// The base tree the board names with `[board] base`, from the map.
fn base_of<'a>(board: &BoardFile, map: &'a SocMap) -> Result<Option<(&'a str, &'a Base)>, String> {
    match board.board.base.as_deref() {
        None => Ok(None),
        Some(name) => map
            .bases
            .get_key_value(name)
            .map(|(name, base)| Some((name.as_str(), base)))
            .ok_or_else(|| format!("[board] base {name:?} is not in the {} map", map.soc)),
    }
}

/// The lines the overlay hogs, for the start check: a hogged line is read from the kernel's listing,
/// a line the boot path alone holds is not. `Err` is the refusal `generate` would make.
pub fn hogged_lines(board: &BoardFile, map: &SocMap) -> Result<Vec<String>, String> {
    let base = base_of(board, map)?;
    let chosen = choose(board, map, base)?;
    let holds = holds(board, &chosen)?;
    Ok(hogs(board, map, &chosen, &holds)?
        .iter()
        .map(|hog| hog.hold.line.clone())
        .collect())
}

/// Each `[[hardware]]` entry, resolved against the map, and the GPIOs no two entries share.
fn choose<'a>(
    board: &'a BoardFile,
    map: &'a SocMap,
    base: Option<(&str, &Base)>,
) -> Result<Vec<Chosen<'a>>, String> {
    let mut chosen = Vec::new();
    let mut controllers = BTreeSet::new();
    let mut gpios: BTreeMap<u16, &str> = BTreeMap::new();
    let mut held_lines: BTreeSet<String> = BTreeSet::new();
    for hardware in &board.hardware {
        let name = hardware.controller.as_str();
        let controller = map.controllers.get(name).ok_or_else(|| {
            format!(
                "[[hardware]] controller {name:?} is not in the {} map; it has: {}",
                map.soc,
                map.controller_names()
            )
        })?;
        if !controllers.insert(name) {
            return Err(format!("[[hardware]] controller {name:?} is listed twice"));
        }
        let pins = match hardware.pins.as_deref() {
            None if controller.kind.needs_pins() => {
                return Err(format!(
                    "[[hardware]] controller {name:?} ({}) needs a `pins` set; the map has: {}",
                    controller.kind.name(),
                    pin_names(controller)
                ));
            }
            None => None,
            Some(set) => Some(controller.pins.get(set).ok_or_else(|| {
                format!(
                    "[[hardware]] controller {name:?} has no pin set {set:?}; it has: {}",
                    pin_names(controller)
                )
            })?),
        };
        if let Some(pins) = pins {
            for &gpio in &pins.gpios {
                if let Some(other) = gpios.insert(gpio, name) {
                    return Err(format!(
                        "GPIO {gpio} is in the pin sets of {other:?} and {name:?}"
                    ));
                }
            }
        }
        let runtime = hardware.name.as_deref().unwrap_or("");
        check_hardware_name(runtime, name, controller)?;
        let module = match hardware.module.as_deref() {
            None => None,
            Some(role) => Some(check_module_role(name, controller, role)?),
        };
        if module.is_none() && (hardware.unconfined || hardware.confined) {
            return Err(format!(
                "[[hardware]] controller {name:?} is `unconfined` or `confined`, which needs `module`"
            ));
        }
        if hardware.unconfined && hardware.confined {
            return Err(format!(
                "[[hardware]] controller {name:?} is both `unconfined` and `confined`"
            ));
        }
        if let Some(role) = hardware.role.as_deref() {
            if controller.kind != Kind::UsbPeripheral {
                return Err(format!(
                    "[[hardware]] controller {name:?} is {}, and `role` is for a usb-peripheral controller",
                    controller.kind.name()
                ));
            }
            if role != "peripheral" {
                return Err(format!(
                    "[[hardware]] controller {name:?} has role {role:?}; the generator makes only a \
                     \"peripheral\" port"
                ));
            }
        }
        let vbus = if controller.kind == Kind::UsbPeripheral {
            let fact = base.and_then(|(_, b)| b.vbus.get(name));
            let plan = vbus_plan(runtime, name, hardware, base.map(|(n, _)| n), fact)?;
            if let VbusPlan::Hold { line, .. } = &plan {
                if board
                    .lines
                    .iter()
                    .any(|l| &l.line == line && l.boot.is_some())
                {
                    return Err(format!(
                        "[[hardware]] controller {name:?}: VBUS line {line:?} is also a [[lines]] entry with a boot state; hold it in one place"
                    ));
                }
                if !held_lines.insert(line.clone()) {
                    return Err(format!(
                        "[[hardware]] controller {name:?}: VBUS line {line:?} is the VBUS line of another port"
                    ));
                }
            }
            Some(plan)
        } else {
            None
        };
        let mut options: Vec<&str> = Vec::new();
        for option in &hardware.options {
            if !controller
                .needs
                .iter()
                .any(|n| n.option.as_deref() == Some(option.as_str()))
            {
                let offered = controller
                    .needs
                    .iter()
                    .filter_map(|n| n.option.as_deref())
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect::<Vec<_>>();
                return Err(format!(
                    "[[hardware]] controller {name:?} has no option {option:?}; it has: {}",
                    if offered.is_empty() {
                        "none".to_string()
                    } else {
                        offered.join(", ")
                    }
                ));
            }
            if options.contains(&option.as_str()) {
                return Err(format!(
                    "[[hardware]] controller {name:?} names option {option:?} twice"
                ));
            }
            options.push(option.as_str());
        }
        chosen.push(Chosen {
            name: runtime,
            key: name,
            controller,
            pins,
            module,
            unconfined: hardware.unconfined,
            confined: hardware.confined,
            options,
            vbus,
        });
    }
    Ok(chosen)
}

/// What a device port's VBUS is, from the board's statement and the base tree's fact. The base tree
/// says what supply the port has (`fact`, from the base named by `[board] base`); the board says how
/// VBUS is handled, when it must. Refused, naming the base, the regulator and the keys to add, when
/// the base has a supply that can be on and the board says nothing about it, and when the base
/// describes nothing about the port at all.
pub fn vbus_plan(
    runtime: &str,
    key: &str,
    hardware: &lemnos_host::board::HardwareSpec,
    base: Option<&str>,
    fact: Option<&Vbus>,
) -> Result<VbusPlan, String> {
    let who = if runtime.is_empty() {
        format!("USB device port {key:?}")
    } else {
        format!("USB device port {key:?} (name {runtime:?})")
    };
    let keys = "`vbus = \"gpio:LABEL:LINE\"` with `vbus_off` and `vbus_reason`, or `vbus_undriven = true` with `vbus_reason`";
    let statement = match (&hardware.vbus, hardware.vbus_undriven) {
        (Some(_), true) => {
            return Err(format!(
                "[[hardware]] {who}: `vbus` and `vbus_undriven` are both set; a port's VBUS is one or the other"
            ));
        }
        (Some(line), false) => {
            let off = match hardware.vbus_off.as_deref() {
                Some(level @ ("low" | "high")) => level.to_string(),
                Some(other) => {
                    return Err(format!(
                        "[[hardware]] {who}: vbus_off {other:?} is not \"low\" or \"high\""
                    ));
                }
                None => {
                    return Err(format!(
                        "[[hardware]] {who}: `vbus` names a line, so `vbus_off` must say its level when VBUS is off"
                    ));
                }
            };
            Some(Statement::Line {
                line: line.clone(),
                off,
                reason: reason_of(hardware, &who)?,
            })
        }
        (None, true) => Some(Statement::Undriven {
            reason: reason_of(hardware, &who)?,
        }),
        (None, false) => {
            if hardware.vbus_off.is_some() || hardware.vbus_reason.is_some() {
                return Err(format!(
                    "[[hardware]] {who}: `vbus_off` and `vbus_reason` go with `vbus` or `vbus_undriven`"
                ));
            }
            None
        }
    };
    let Some(fact) = fact else {
        return match statement {
            Some(statement) => Ok(statement.plan()),
            None => Err(format!(
                "[[hardware]] {who}: the base {} does not describe its VBUS, so the generator cannot \
                 see whether it has an always-on or switched supply; state how VBUS is handled: {keys}",
                base.map_or_else(
                    || "(none is named by [board] base)".to_string(),
                    |b| format!("{b:?}")
                )
            )),
        };
    };
    let Some(regulator) = &fact.regulator else {
        return Ok(statement.map_or(VbusPlan::NoSupply, Statement::plan));
    };
    match (&fact.switch, statement) {
        (
            Some(line),
            Some(Statement::Line {
                line: stated,
                off,
                reason,
            }),
        ) if &stated == line => Ok(VbusPlan::Hold {
            line: stated,
            off,
            reason,
            regulator: Some(regulator.clone()),
        }),
        (Some(_), Some(statement @ Statement::Undriven { .. })) => Ok(statement.plan()),
        (Some(line), Some(Statement::Line { line: stated, .. })) => Err(format!(
            "[[hardware]] {who}: the base {base:?} switches its VBUS regulator {regulator} with {line}, \
             and the board names {stated}; the two must be the same line"
        )),
        (Some(line), None) => Err(format!(
            "[[hardware]] {who}: VBUS regulator {regulator} (base {base:?}: {}) is switched by {line}; \
             state it: `vbus = {line:?}` with `vbus_off` and `vbus_reason`, or {keys_undriven}",
            fact.reason,
            keys_undriven = "`vbus_undriven = true` if this port is not driven by it"
        )),
        (None, Some(statement)) => Ok(statement.plan()),
        (None, None) if fact.always_on => Err(format!(
            "[[hardware]] {who}: VBUS regulator {regulator} (base {base:?}: {}) is always on, so detaching \
             the PHY's phy-supply does not stop VBUS; state how VBUS is handled: {keys}",
            fact.reason
        )),
        (None, None) => Err(format!(
            "[[hardware]] {who}: VBUS regulator {regulator} (base {base:?}: {}) has no enable line the \
             generator can hold; state how VBUS is handled: {keys}",
            fact.reason
        )),
    }
}

/// A board's statement about a device port's VBUS, before the base tree's fact is applied.
enum Statement {
    Line {
        line: String,
        off: String,
        reason: String,
    },
    Undriven {
        reason: String,
    },
}

impl Statement {
    fn plan(self) -> VbusPlan {
        match self {
            Self::Line { line, off, reason } => VbusPlan::Hold {
                line,
                off,
                reason,
                regulator: None,
            },
            Self::Undriven { reason } => VbusPlan::Undriven { reason },
        }
    }
}

fn reason_of(hardware: &lemnos_host::board::HardwareSpec, who: &str) -> Result<String, String> {
    match hardware.vbus_reason.as_deref() {
        Some(reason) if !reason.trim().is_empty() => Ok(reason.to_string()),
        _ => Err(format!(
            "[[hardware]] {who}: a VBUS statement needs `vbus_reason`, what the schematic shows"
        )),
    }
}

/// A module role a board gives a node: `owned`, which needs the node's compatible, or a role the
/// map gives the controller.
fn check_module_role<'a>(
    name: &str,
    controller: &Controller,
    role: &'a str,
) -> Result<&'a str, String> {
    if controller.compatible.is_none() {
        return Err(format!(
            "[[hardware]] controller {name:?} is given to the module, but the map gives it no \
             `compatible`, so the module cannot own it"
        ));
    }
    if role == "owned" || controller.module_roles.iter().any(|r| r == role) {
        return Ok(role);
    }
    let roles = if controller.module_roles.is_empty() {
        "owned".to_string()
    } else {
        format!("owned, {}", controller.module_roles.join(", "))
    };
    Err(format!(
        "[[hardware]] controller {name:?} has module role {role:?}, which the map does not give it; \
         it has: {roles}"
    ))
}

fn pin_names(controller: &Controller) -> String {
    if controller.pins.is_empty() {
        "no pin sets".into()
    } else {
        controller
            .pins
            .keys()
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// A runtime name must be the one its controller's alias gives: the kernel numbers an I2C adapter
/// `i2c-N` by alias `i2cN`, and an SPI bus `spidevB.C` by alias `spiB`.
fn check_hardware_name(
    runtime: &str,
    controller_name: &str,
    controller: &Controller,
) -> Result<(), String> {
    if runtime.is_empty() {
        return Ok(());
    }
    if runtime.starts_with("gpiochip") || runtime.starts_with("pwmchip") {
        return Err(format!(
            "name {runtime:?}: the kernel numbers gpio and pwm chips as it probes them, and no \
             device-tree alias sets the number, so this generator cannot make it come out as written"
        ));
    }
    match controller.kind {
        Kind::UsbPeripheral => {
            let udc = controller.sysfs.as_deref().unwrap_or_default();
            if runtime != format!("udc:{udc}") {
                return Err(format!(
                    "name {runtime:?}: controller {controller_name:?} is the UDC {udc:?}, so a gadget \
                     names it as \"udc:{udc}\""
                ));
            }
        }
        Kind::I2c => {
            let number = runtime
                .strip_prefix("i2c-")
                .and_then(|n| n.parse::<u32>().ok());
            let number = number.ok_or_else(|| {
                format!("controller {controller_name:?} is an I2C bus, so its name is i2c-N, not {runtime:?}")
            })?;
            if controller.alias.as_deref() != Some(&format!("i2c{number}")) {
                return Err(format!(
                    "name {runtime:?}: controller {controller_name:?} has alias {:?}, and the name \
                     comes out of alias i2c{number}",
                    controller.alias.as_deref().unwrap_or("none")
                ));
            }
        }
        Kind::Spi => {
            let bus = runtime
                .strip_prefix("spidev")
                .and_then(|rest| rest.split_once('.'))
                .and_then(|(bus, _)| bus.parse::<u32>().ok());
            let bus = bus.ok_or_else(|| {
                format!("controller {controller_name:?} is an SPI bus, so its name is spidevB.C, not {runtime:?}")
            })?;
            if controller.alias.as_deref() != Some(&format!("spi{bus}")) {
                return Err(format!(
                    "name {runtime:?}: controller {controller_name:?} must have alias spi{bus}"
                ));
            }
        }
        _ => {}
    }
    Ok(())
}

/// Each `[[lines]]` entry names a GPIO line by its stable name, `gpio:LABEL:LINE`, and its boot
/// state, when it has one, has a reason.
fn check_lines(board: &BoardFile, map: &SocMap) -> Result<(), String> {
    for line in &board.lines {
        gpio_line(&line.line, map).map_err(|why| format!("[[lines]] {why}"))?;
        let Some(level) = line.boot.as_deref() else {
            continue;
        };
        if !matches!(level, "high" | "low") {
            return Err(format!(
                "line {:?}: boot {level:?} is not \"low\" or \"high\"",
                line.line
            ));
        }
        if line.reason.is_none() {
            return Err(format!(
                "line {:?}: boot {level:?} needs a `reason`",
                line.line
            ));
        }
    }
    Ok(())
}

/// A stable GPIO name's chip label and line, checked against the map's chips.
fn gpio_line<'a>(name: &'a str, map: &SocMap) -> Result<(&'a str, u16), String> {
    let (label, line) = name
        .strip_prefix("gpio:")
        .and_then(|rest| rest.rsplit_once(':'))
        .ok_or_else(|| {
            format!(
                "line {name:?} is not `gpio:LABEL:LINE`, the stable name of a GPIO line; the \
                 numbered `gpiochipN:LINE` cannot be made to match a generated tree"
            )
        })?;
    let number = line
        .parse::<u16>()
        .map_err(|_| format!("line {name:?}: {line:?} is not a line number"))?;
    if map.chip_labelled(label).is_none() {
        return Err(format!(
            "line {name:?}: the map has no GPIO chip labelled {label:?}; it has: {}",
            map.chip_labels()
        ));
    }
    Ok((label, number))
}

/// Every name a device uses is one the generator makes something of. A device names the hardware
/// its driver needs: a device that makes nothing is accepted only when its driver declares no hardware
/// need (its needs are other devices, or none), which is read from the driver's manifest.
fn check_devices(
    board: &BoardFile,
    map: &SocMap,
    chosen: &[Chosen<'_>],
    base: Option<&crate::map::Base>,
    drivers: &[Manifest],
) -> Result<(), String> {
    let ids: BTreeSet<&str> = board.devices.iter().map(|d| d.id.as_str()).collect();
    let named: BTreeSet<&str> = chosen.iter().map(|c| c.name).collect();
    let has_pio = chosen.iter().any(|c| c.controller.kind == Kind::Pio);
    for device in &board.devices {
        if device.disabled {
            continue;
        }
        let names = device
            .bus
            .iter()
            .map(String::as_str)
            .chain(device.uses.values().map(String::as_str));
        let mut makes = 0usize;
        for name in names {
            if ids.contains(name) {
                continue;
            }
            if name.starts_with("gpiochip")
                || name.starts_with("pwmchip")
                || is_numbered_watchdog(name)
            {
                return Err(format!(
                    "device {:?} uses {name:?}: the kernel numbers gpio, pwm and watchdog devices as it \
                     probes them, and no device-tree alias sets the number, so this generator cannot make \
                     it come out as written; name the device stably: gpio:LABEL:LINE, pwm:NODE:CHANNEL or \
                     watchdog:NODE",
                    device.id
                ));
            }
            if name.starts_with("gpio:") {
                let (label, _) = gpio_line(name, map)
                    .map_err(|why| format!("device {:?} uses {name:?}: {why}", device.id))?;
                let line = board.lines.iter().find(|l| l.line == name).ok_or_else(|| {
                    format!(
                        "device {:?} uses {name:?}, which no [[lines]] entry gives to Lemnos (chip {label:?})",
                        device.id
                    )
                })?;
                let held = base.is_some_and(|b| b.lines.contains_key(&line.line));
                if !held && line.boot.is_none() {
                    return Err(format!(
                        "device {:?} uses {name:?}: its [[lines]] entry makes nothing (no base node holds \
                         the line and it has no boot state)",
                        device.id
                    ));
                }
                makes += 1;
            } else if name.starts_with("pwm:") {
                let Some((node, _)) = name.strip_prefix("pwm:").and_then(|r| r.rsplit_once(':'))
                else {
                    return Err(format!(
                        "device {:?} uses {name:?}: not `pwm:NODE:CHANNEL`",
                        device.id
                    ));
                };
                let key = map
                    .controllers
                    .iter()
                    .find(|(_, c)| c.kind == Kind::Pwm && c.path == node)
                    .map(|(k, _)| k.as_str())
                    .ok_or_else(|| {
                        format!(
                            "device {:?} uses {name:?}: the map has no PWM block at {node}",
                            device.id
                        )
                    })?;
                if !chosen.iter().any(|c| c.key == key) {
                    return Err(format!(
                        "device {:?} uses {name:?}, which needs the PWM block: add a [[hardware]] entry \
                         for controller {key:?}",
                        device.id
                    ));
                }
                makes += 1;
            } else if let Some(node) = name.strip_prefix("watchdog:") {
                let key = map
                    .controllers
                    .iter()
                    .find(|(_, c)| c.kind == Kind::Watchdog && c.path == node)
                    .map(|(k, _)| k.as_str())
                    .ok_or_else(|| {
                        format!(
                            "device {:?} uses {name:?}: the map has no watchdog at {node}",
                            device.id
                        )
                    })?;
                if !chosen.iter().any(|c| c.key == key) {
                    return Err(format!(
                        "device {:?} uses {name:?}, which needs the watchdog: add a [[hardware]] entry \
                         for controller {key:?}",
                        device.id
                    ));
                }
                makes += 1;
            } else if name.starts_with("pio-i2c:") || name.starts_with("pio-out:") {
                if !has_pio {
                    return Err(format!(
                        "device {:?} uses {name:?}, which needs the PIO block: add a [[hardware]] entry for \
                         controller \"pio\"",
                        device.id
                    ));
                }
                makes += 1;
            } else if name.starts_with("i2c-")
                || name.starts_with("spidev")
                || name.starts_with("udc:")
            {
                if !named.contains(name) {
                    return Err(format!(
                        "device {:?} uses {name:?}, which no [[hardware]] entry names",
                        device.id
                    ));
                }
                makes += 1;
            } else {
                return Err(format!(
                    "device {:?} uses {name:?}: the generator makes nothing of that name; it knows \
                     i2c-N, spidevB.C, udc:NAME, pio-i2c:..., pio-out:..., gpio:LABEL:LINE and \
                     pwm:NODE:CHANNEL",
                    device.id
                ));
            }
        }
        if makes == 0 {
            check_hardware_needs(device, drivers)?;
        }
    }
    // A port in device mode is made only for the gadget that names it: the board file never puts
    // a port in peripheral mode for nothing.
    let used: BTreeSet<&str> = board
        .devices
        .iter()
        .filter(|d| !d.disabled)
        .flat_map(|d| d.bus.iter().chain(d.uses.values()))
        .map(String::as_str)
        .collect();
    for entry in chosen {
        if entry.controller.kind == Kind::UsbPeripheral
            && (entry.name.is_empty() || !used.contains(entry.name))
        {
            return Err(format!(
                "[[hardware]] controller {:?} is a USB device port, and no device names it: add a \
                 usb-gadget device with `uses = {{ function = \"udc:{}\" }}`, or remove the entry",
                entry.key,
                entry.controller.sysfs.as_deref().unwrap_or_default()
            ));
        }
    }
    Ok(())
}

/// Whether a board entry with `options` takes a map's need: always, or when it names the need's option.
fn asks(need: &crate::map::Need, options: &[&str]) -> bool {
    match need.option.as_deref() {
        None => true,
        Some(option) => options.contains(&option),
    }
}

/// A device that names no hardware is accepted only when its driver declares no hardware need: a
/// driver whose needs are other devices, or none, produces nothing in any output by definition.
/// A driver the tool does not know cannot be checked, so it is refused.
fn check_hardware_needs(
    device: &lemnos_host::board::DeviceSpec,
    drivers: &[Manifest],
) -> Result<(), String> {
    let Some(manifest) = drivers.iter().find(|m| m.name == device.driver) else {
        return Err(format!(
            "device {:?} (driver {:?}) names no hardware, and the tool does not know that driver's \
             needs, so it cannot tell that they produce nothing; name its hardware in `bus` or `uses`",
            device.id, device.driver
        ));
    };
    let hardware: Vec<&str> = manifest
        .needs
        .iter()
        .filter(|need| !need.optional && is_hardware(need.kind))
        .map(|need| need.name)
        .collect();
    if hardware.is_empty() {
        return Ok(());
    }
    Err(format!(
        "device {:?} (driver {:?}) needs {} and names no hardware for it; name the hardware in `bus` \
         or `uses`",
        device.id,
        device.driver,
        hardware.join(", ")
    ))
}

/// Whether a need is hardware a board file names: every bus kind but a stream, which the host
/// makes from the device's own `streams`.
fn is_hardware(kind: NeedKind) -> bool {
    !matches!(kind, NeedKind::Bus(BusKind::Stream) | NeedKind::Device(_))
}

/// A watchdog by its number, `watchdogN`: the number is the kernel's, as it probes.
fn is_numbered_watchdog(name: &str) -> bool {
    name.strip_prefix("watchdog")
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// The base-tree nodes the overlay disables, each with its reason, in the order it writes them: the
/// holders of each line the board gives to Lemnos or holds at boot, then the VBUS regulator of each
/// port held by its line. A disabled node owns nothing, so a line it held is free for a hog.
fn disabled_nodes(
    name: &str,
    base: &crate::map::Base,
    lines: &[LineSpec],
    holds: &[Hold],
) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut seen = BTreeSet::new();
    let board_lines = lines.iter().map(|l| l.line.as_str());
    let hold_lines = holds.iter().map(|h| h.line.as_str());
    for line in board_lines.chain(hold_lines) {
        if let Some(holders) = base.lines.get(line) {
            for holder in &holders.holders {
                if seen.insert(holder.clone()) {
                    out.push((
                        holder.clone(),
                        format!(
                            "disabled: base {name:?} {holder}: {}; the board gives {line} to Lemnos",
                            holders.reason
                        ),
                    ));
                }
            }
        }
    }
    for hold in holds {
        if let Some(regulator) = &hold.regulator
            && seen.insert(regulator.clone())
        {
            out.push((
                regulator.clone(),
                format!(
                    "disabled: base {name:?} {regulator}: the VBUS regulator of {} is switched by {}; \
                     its phy-supply is the stand-in, so nothing uses it and the hog holds {} at its off level",
                    hold.vbus_of.as_deref().unwrap_or_default(),
                    hold.line,
                    hold.line
                ),
            ));
        }
    }
    out
}

/// The overlay: one fragment per enabled node, per disabled base node and per hog, each with its reason.
fn overlay(
    board: &BoardFile,
    chosen: &[Chosen<'_>],
    base: Option<(&str, &crate::map::Base)>,
    lines: &[LineSpec],
    holds: &[Hold],
    hogs: &[Hog<'_>],
    form: Form,
) -> Result<String, String> {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "// Generated by `lemnos-ctl tree` from board {:?}. Do not edit: change the",
        board.board.id
    );
    let _ = writeln!(out, "// board file or the SoC map, and generate again.");
    let _ = writeln!(
        out,
        "// Every fragment says why it is here. Node paths are the base tree's."
    );
    match form {
        Form::Overlay => {
            let _ = writeln!(out, "/dts-v1/;");
            let _ = writeln!(out, "/plugin/;");
            let _ = writeln!(out, "/ {{");
        }
        // A fragment is merged into a board's own `.dts`, which names the base tree, so it has no
        // header of its own and no root node: each fragment is a `&{path}` (or `/`) block.
        Form::Fragment => {}
    }
    let mut fragment = 0usize;
    let mut fragments = String::new();
    let mut no_vbus_written = false;
    for entry in chosen {
        let controller = entry.controller;
        let mut body = vec![String::from("status = \"okay\";")];
        if let Some(pins) = entry.pins {
            body.push("pinctrl-names = \"default\";".into());
            body.push(format!("pinctrl-0 = <&{}>;", pins.label));
        }
        if controller.kind == Kind::UsbPeripheral {
            body.push("dr_mode = \"peripheral\";".into());
        }
        // The PHYs the entry takes, in the map's order: the base tree's `phys` is replaced by these, so
        // a port without an option has only its USB 2 PHY and no SuperSpeed link.
        let phys: Vec<&crate::map::Need> = controller
            .needs
            .iter()
            .filter(|n| n.phy.is_some() && asks(n, &entry.options))
            .collect();
        if !phys.is_empty() {
            let phandles = phys
                .iter()
                .map(|n| {
                    let mut cells = format!("&{}", n.label.as_deref().unwrap_or_default());
                    for arg in &n.args {
                        let _ = write!(cells, " {arg}");
                    }
                    format!("<{cells}>")
                })
                .collect::<Vec<_>>()
                .join(", ");
            let names = phys
                .iter()
                .map(|n| format!("\"{}\"", n.phy.as_deref().unwrap_or_default()))
                .collect::<Vec<_>>()
                .join(", ");
            body.push(format!("phys = {phandles};"));
            body.push(format!("phy-names = {names};"));
        }
        let mut reason = format!("{}: {}", controller.kind.name(), controller.reason);
        if let Some(alias) = &controller.alias {
            let _ = write!(reason, " (alias {alias}");
            if !entry.name.is_empty() {
                let _ = write!(reason, ", the name {:?}", entry.name);
            }
            reason.push(')');
        }
        if let Some(pins) = entry.pins {
            let _ = write!(
                reason,
                " on {}",
                pins.gpios
                    .iter()
                    .map(|g| format!("GPIO{g}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        push_fragment(
            form,
            &mut fragments,
            &mut fragment,
            &controller.path,
            &reason,
            &body,
        );
        if let Some(alias) = &controller.alias {
            let alias_body = format!("aliases {{ {alias} = \"{}\"; }};", controller.path);
            push_fragment(
                form,
                &mut fragments,
                &mut fragment,
                "/",
                &format!("the runtime name comes out of alias {alias}"),
                std::slice::from_ref(&alias_body),
            );
        }
        for need in &controller.needs {
            if asks(need, &entry.options) {
                // A device port's PHY never names a VBUS supply: its `phy-supply` (a host's 5 V
                // for VBUS) is replaced by the stand-in, which supplies nothing. dtc 1.8 drops
                // `/delete-property/` in an overlay, so replacing is the only way that every
                // loader applies (see docs/device-tree.md).
                let mut body = vec![String::from("status = \"okay\";")];
                let mut reason = format!(
                    "needed by {} ({}): {}",
                    entry.key,
                    controller.kind.name(),
                    need.reason
                );
                if controller.kind == Kind::UsbPeripheral && need.phy.as_deref() == Some("usb2-phy")
                {
                    if !no_vbus_written {
                        push_fragment(
                            form,
                            &mut fragments,
                            &mut fragment,
                            "/",
                            "the stand-in for a device port's VBUS supply: a fixed regulator with no enable pin, no GPIO and no voltage; it supplies nothing",
                            &[format!(
                                "{NO_VBUS_LABEL}: regulator-lemnos-no-vbus {{ compatible = \"regulator-fixed\"; regulator-name = \"lemnos-no-vbus\"; regulator-min-microvolt = <0>; regulator-max-microvolt = <0>; }};"
                            )],
                        );
                        no_vbus_written = true;
                    }
                    body.push(format!("phy-supply = <&{NO_VBUS_LABEL}>;"));
                    reason.push_str(
                        "; its phy-supply is replaced by the stand-in, so this PHY drives no VBUS",
                    );
                }
                push_fragment(
                    form,
                    &mut fragments,
                    &mut fragment,
                    &need.path,
                    &reason,
                    &body,
                );
            }
        }
    }
    if let Some((name, base)) = base {
        for (node, reason) in disabled_nodes(name, base, lines, holds) {
            push_fragment(
                form,
                &mut fragments,
                &mut fragment,
                &node,
                &reason,
                &["status = \"disabled\";".into()],
            );
        }
    } else if !lines.is_empty() {
        let _ = writeln!(
            fragments,
            "\t// No base named: lines the board gives to Lemnos are not checked against the base tree."
        );
    }
    // The hogs: each held line's `gpio-hog`, a child of its chip's node. The kernel asserts the
    // level when the chip registers (or, for an overlay added later, when the hog node is added).
    for hog in hogs {
        let level = if hog.hold.level == "high" {
            "output-high"
        } else {
            "output-low"
        };
        let node = format!(
            "lemnos-hog-{} {{ gpio-hog; gpios = <{} 0>; {level}; line-name = \"{}\"; }};",
            hog.number,
            hog.number,
            line_name(&hog.label)
        );
        push_fragment(
            form,
            &mut fragments,
            &mut fragment,
            hog.chip_path,
            &format!(
                "gpio-hog: {} is {} from power-on, until the kernel takes the line",
                hog.hold.line, hog.hold.level
            ),
            std::slice::from_ref(&node),
        );
    }
    out.push_str(&fragments);
    if form == Form::Overlay {
        let _ = writeln!(out, "}};");
    }
    Ok(out)
}

fn push_fragment(
    form: Form,
    out: &mut String,
    n: &mut usize,
    target: &str,
    reason: &str,
    body: &[String],
) {
    match form {
        Form::Overlay => {
            let _ = writeln!(out, "\t// {reason}");
            let _ = writeln!(out, "\tfragment@{n} {{");
            let _ = writeln!(out, "\t\ttarget-path = \"{target}\";");
            let _ = writeln!(out, "\t\t__overlay__ {{");
            for line in body {
                let _ = writeln!(out, "\t\t\t{line}");
            }
            let _ = writeln!(out, "\t\t}};");
            let _ = writeln!(out, "\t}};");
        }
        // The base tree's path, by a `dtc` path reference: no label is needed, and the root's
        // `aliases` merge in as they are written.
        Form::Fragment => {
            let _ = writeln!(out, "// {reason}");
            let head = if target == "/" {
                "/".to_string()
            } else {
                format!("&{{{target}}}")
            };
            let _ = writeln!(out, "{head} {{");
            for line in body {
                let _ = writeln!(out, "\t{line}");
            }
            let _ = writeln!(out, "}};");
        }
    }
    *n += 1;
}

/// A line the boot path holds from power-on: a `[[lines]]` entry with a `boot` state, or a device
/// port's VBUS held at its off level. Each is one `gpio=` line of the boot fragment, one `gpio set`
/// or `gpio clear` of the U-Boot fragment, and, where a hog is made, one `gpio-hog` of the overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hold {
    /// The stable GPIO name, `gpio:LABEL:LINE`.
    pub line: String,
    /// `"low"` or `"high"`: the pin's level from power-on.
    pub level: String,
    /// What the board says about the line.
    pub reason: String,
    /// For a device port's VBUS: the port's key in the map. `None` for a board's `[[lines]]` entry.
    pub vbus_of: Option<String>,
    /// For a VBUS hold: the base regulator the line switches. The overlay disables it, since the
    /// port's `phy-supply` is the stand-in and nothing else uses it; left enabled, the regulator
    /// would own the line and the hog could not take it.
    pub regulator: Option<String>,
}

/// Every line the boot path holds, in the order the boot fragment writes them: each `[[lines]]`
/// boot state, then each device port's VBUS hold. A line held twice is refused by name.
fn holds(board: &BoardFile, chosen: &[Chosen<'_>]) -> Result<Vec<Hold>, String> {
    let mut out: Vec<Hold> = Vec::new();
    for line in &board.lines {
        let Some(level) = line.boot.as_deref() else {
            continue;
        };
        if out.iter().any(|h| h.line == line.line) {
            return Err(format!("[[lines]] {:?} has a boot state twice", line.line));
        }
        out.push(Hold {
            line: line.line.clone(),
            level: level.to_string(),
            reason: line.reason.clone().unwrap_or_default(),
            vbus_of: None,
            regulator: None,
        });
    }
    for entry in chosen {
        let Some(VbusPlan::Hold {
            line,
            off,
            reason,
            regulator,
        }) = &entry.vbus
        else {
            continue;
        };
        if out.iter().any(|h| &h.line == line) {
            return Err(format!(
                "[[hardware]] controller {:?}: VBUS line {line:?} is also held by another entry",
                entry.key
            ));
        }
        out.push(Hold {
            line: line.clone(),
            level: off.clone(),
            reason: reason.clone(),
            vbus_of: Some(entry.key.to_string()),
            regulator: regulator.clone(),
        });
    }
    Ok(out)
}

/// A `gpio-hog` the overlay makes for a hold: the chip node it is a child of, and the line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hog<'a> {
    pub hold: &'a Hold,
    /// The chip's controller node in the base tree (the map's `path`).
    pub chip_path: &'a str,
    /// The line within the chip.
    pub number: u16,
    /// The hog's `line-name` before it is cleaned for the tree.
    pub label: String,
}

/// The holds the kernel can own, as `gpio-hog`s. The rule, decided from the board file and the base:
///
/// - A line a device of the board names (in `bus` or `uses`) is used at run time: its driver requests
///   it when it binds, and a hog would make that request fail. Such a board `[[lines]]` hold is not
///   hogged: the boot path holds it until the driver takes it. A device port's VBUS line used by a
///   device is refused by name, since no hog or boot path can then hold VBUS alone.
/// - A line a chosen controller's pin set takes is the pad of that function (a PWM output, say) from
///   the driver's probe on: no hog is made, and the boot path holds it until then. A device port's
///   VBUS on such a pad is refused by name.
/// - Otherwise the hold is hogged, when its chip has a node in the map (`path`); a chip without one is
///   refused by name, since the overlay cannot hold the line.
/// - A base node that holds a line the overlay holds is disabled by the overlay (see
///   [`disabled_nodes`]), so it owns nothing and the hog takes the line. The line's owner is the
///   driver that binds it, or the hog; a base node is never left to share it.
fn hogs<'a>(
    board: &BoardFile,
    map: &'a SocMap,
    chosen: &'a [Chosen<'_>],
    holds: &'a [Hold],
) -> Result<Vec<Hog<'a>>, String> {
    let mut users: BTreeMap<&str, &str> = BTreeMap::new();
    for device in board.devices.iter().filter(|d| !d.disabled) {
        for name in device
            .bus
            .iter()
            .map(String::as_str)
            .chain(device.uses.values().map(String::as_str))
        {
            users.entry(name).or_insert(device.id.as_str());
        }
    }
    let mut out = Vec::new();
    for hold in holds {
        let (label, number) = gpio_line(&hold.line, map)?;
        let who = match &hold.vbus_of {
            Some(key) => format!("[[hardware]] controller {key:?}: VBUS line {}", hold.line),
            None => format!("[[lines]] {:?}", hold.line),
        };
        if let Some(device) = users.get(hold.line.as_str()) {
            if hold.vbus_of.is_some() {
                return Err(format!(
                    "{who} is also used by device {device:?}: the VBUS hold is a gpio-hog, which a \
                     device cannot share with its driver; remove the device's use of the line, or the \
                     VBUS statement"
                ));
            }
            continue;
        }
        let chip = map
            .chip_labelled(label)
            .ok_or_else(|| format!("{who}: the map has no GPIO chip labelled {label:?}"))?;
        let gpio = chip.number_base + u32::from(number);
        let taken = chosen.iter().find_map(|entry| {
            let pins = entry.pins?;
            u16::try_from(gpio)
                .is_ok_and(|g| pins.gpios.contains(&g))
                .then_some((entry.key, pins))
        });
        if let Some((key, pins)) = taken {
            if hold.vbus_of.is_some() {
                return Err(format!(
                    "{who} is GPIO {gpio}, in the pin set {:?} of controller {key:?}; a hog would take \
                     the pad from that pin set, and VBUS must be held by the overlay",
                    pins.label
                ));
            }
            // The pad is the pin set's function (a PWM output, say) from the driver's probe on: the
            // boot path holds it before that, and no hog is made.
            continue;
        }
        let chip_path = chip.path.as_deref().ok_or_else(|| {
            format!(
                "{who}: chip {label:?} has no `path` in the {} map, so the overlay cannot hog the \
                 line; add the chip's node path to the map",
                map.soc
            )
        })?;
        out.push(Hog {
            hold,
            chip_path,
            number,
            label: match &hold.vbus_of {
                Some(key) => format!("VBUS of {key}, off {}", hold.level),
                None => hold.reason.clone(),
            },
        });
    }
    Ok(out)
}

/// A hog's `line-name`: the text with the characters a device-tree string cannot carry dropped, its
/// spaces collapsed, cut to 60 characters. The kernel keeps the name as the hog's consumer label.
fn line_name(text: &str) -> String {
    let clean: String = text
        .chars()
        .filter(|c| !c.is_control() && *c != '"' && *c != '\\')
        .collect();
    let joined = clean.split_whitespace().collect::<Vec<_>>().join(" ");
    joined.chars().take(60).collect()
}

/// The firmware configuration: the level each held line has from power-on until the kernel takes it.
/// Every boot hold is written, with its reason. The Raspberry Pi firmware reads this file.
fn boot(board: &BoardFile, holds: &[Hold], map: &SocMap) -> Result<String, String> {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "# Generated by `lemnos-ctl tree` from board {:?}. The firmware reads these",
        board.board.id
    );
    let _ = writeln!(
        out,
        "# before the kernel runs, so each line holds its level from power-on until the"
    );
    let _ = writeln!(out, "# kernel drives it. Syntax not checked on board.");
    for hold in holds {
        let state = if hold.level == "high" { "dh" } else { "dl" };
        let gpio = boot_number(&hold.line, map)?;
        match &hold.vbus_of {
            None => {
                let _ = writeln!(out, "# {}: boot {}: {}", hold.line, hold.level, hold.reason);
            }
            Some(key) => {
                let _ = writeln!(
                    out,
                    "# {}: VBUS of {key} held {} (off) from power-on: {}",
                    hold.line, hold.level, hold.reason
                );
            }
        }
        let _ = writeln!(out, "gpio={gpio}=op,{state}");
    }
    Ok(out)
}

/// The U-Boot fragment: `gpio set` and `gpio clear` for each hold, for a boot script to run before
/// `booti` or `sysboot`. The map's `uboot` names the lines (`rockchip-bank`: the bank's letter, `A`
/// for bank 0, and the line within it, as `gpio set E11`). A map without a naming is refused by name
/// when there is a hold to write.
fn uboot(board: &BoardFile, holds: &[Hold], map: &SocMap) -> Result<String, String> {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "# Generated by `lemnos-ctl tree` from board {:?}. A boot script runs these before",
        board.board.id
    );
    let _ = writeln!(
        out,
        "# booti or sysboot, so each line holds its level until the kernel takes it."
    );
    if holds.is_empty() {
        let _ = writeln!(out, "# No line is held from power-on: nothing to set.");
        return Ok(out);
    }
    let Some(scheme) = map.uboot.as_deref() else {
        return Err(format!(
            "the {} map gives no U-Boot GPIO naming, so the {} held line(s) ({}) cannot be written \
             for U-Boot; the boot fragment (`--boot`) is the firmware's only form for this SoC",
            map.soc,
            holds.len(),
            holds
                .iter()
                .map(|h| h.line.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    };
    for hold in holds {
        let (label, line) = gpio_line(&hold.line, map)?;
        let chip = map
            .chip_labelled(label)
            .ok_or_else(|| format!("line {:?}: no GPIO chip labelled {label:?}", hold.line))?;
        let name = match scheme {
            "rockchip-bank" => {
                if chip.number_base % 32 != 0 || line >= 32 {
                    return Err(format!(
                        "line {:?}: U-Boot names it by bank letter and line, and the chip {label:?} is not a 32-line bank",
                        hold.line
                    ));
                }
                let letter = char::from(b'A' + (chip.number_base / 32) as u8);
                format!("{letter}{line}")
            }
            other => {
                return Err(format!(
                    "uboot {other:?} is not a naming the generator knows"
                ));
            }
        };
        let command = if hold.level == "high" { "set" } else { "clear" };
        let _ = writeln!(
            out,
            "# {}{}: {}",
            hold.line,
            if hold.vbus_of.is_some() {
                " (VBUS)"
            } else {
                ""
            },
            hold.reason
        );
        let _ = writeln!(out, "gpio {command} {name}");
    }
    Ok(out)
}

/// The SoC's GPIO number of a stable GPIO name, which the boot fragment writes: the chip's first
/// number plus the line.
fn boot_number(name: &str, map: &SocMap) -> Result<u32, String> {
    let (label, line) = gpio_line(name, map)?;
    let chip = map
        .chip_labelled(label)
        .ok_or_else(|| format!("line {name:?}: no GPIO chip labelled {label:?}"))?;
    Ok(chip.number_base + u32::from(line))
}

/// The kernel module's configuration for the board: the nodes given to it, as `owned=` and the
/// role and confinement options, and a `blacklist` for each stock module that binds one of them.
/// Written as modprobe configuration, with each node's reason beside it.
fn modprobe(board: &BoardFile, chosen: &[Chosen<'_>]) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "# Generated by `lemnos-ctl tree` from board {:?}. Do not edit: change the board file or",
        board.board.id
    );
    let _ = writeln!(out, "# the SoC map, and generate again.");
    let _ = writeln!(
        out,
        "# The kernel module owns the nodes below, and each stock driver named by a blacklist is"
    );
    let _ = writeln!(
        out,
        "# kept off them when it loads. Install in /etc/modprobe.d/. A driver built into the kernel"
    );
    let _ = writeln!(
        out,
        "# is not stopped by a blacklist: check it at start (lemnosd reports it)."
    );
    // Each option as an ordered set of compatibles, and the stock modules to blacklist.
    let mut options: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    let mut blacklist: BTreeSet<&str> = BTreeSet::new();
    let mut nodes = 0usize;
    for entry in chosen.iter().filter(|c| c.module.is_some()) {
        let Some(compatible) = entry.controller.compatible.as_deref() else {
            continue;
        };
        nodes += 1;
        let role = entry.module.unwrap_or("owned");
        let label = if entry.name.is_empty() {
            entry.key.to_string()
        } else {
            entry.name.to_string()
        };
        let _ = writeln!(
            out,
            "# {label}: {}. Compatible {compatible}, given to the module as {role}{}",
            entry.controller.reason,
            if entry.unconfined {
                " (registers mapped unconfined)"
            } else if entry.confined {
                " (DMA confined by an IOMMU)"
            } else {
                ""
            }
        );
        options
            .entry("owned")
            .or_default()
            .insert(compatible.into());
        if role != "owned" {
            options.entry(role).or_default().insert(compatible.into());
        }
        if entry.unconfined {
            options
                .entry("unconfined")
                .or_default()
                .insert(compatible.into());
        }
        if entry.confined {
            options
                .entry("confined")
                .or_default()
                .insert(compatible.into());
        }
        blacklist.extend(entry.controller.stock_modules.iter().map(String::as_str));
    }
    if nodes == 0 {
        let _ = writeln!(
            out,
            "# No node is given to the kernel module, so there are no options."
        );
        return out;
    }
    let line = options
        .iter()
        .map(|(option, pairs)| {
            format!(
                "{option}={}",
                pairs.iter().cloned().collect::<Vec<_>>().join(",")
            )
        })
        .collect::<Vec<_>>()
        .join(" ");
    let _ = writeln!(out, "options lemnos {line}");
    for module in blacklist {
        let _ = writeln!(
            out,
            "# the stock driver for a node the module owns: its node must not bind it"
        );
        let _ = writeln!(out, "blacklist {module}");
    }
    out
}
