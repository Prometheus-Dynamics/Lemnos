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
    let base = match board.board.base.as_deref() {
        None => None,
        Some(name) => Some((
            name,
            map.bases
                .get(name)
                .ok_or_else(|| format!("[board] base {name:?} is not in the {soc} map"))?,
        )),
    };
    let chosen = choose(board, map, base)?;
    check_lines(board, map)?;
    check_devices(board, map, &chosen, base.map(|(_, b)| b), drivers)?;
    let overlay = overlay(board, &chosen, base, &board.lines, map.form)?;
    let boot = boot(board, map, &chosen)?;
    let modprobe = modprobe(board, &chosen);
    Ok(Output {
        overlay,
        boot,
        modprobe,
    })
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
            Self::Line { line, off, reason } => VbusPlan::Hold { line, off, reason },
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

/// The overlay: one fragment per enabled node and per disabled holder, each with its reason.
fn overlay(
    board: &BoardFile,
    chosen: &[Chosen<'_>],
    base: Option<(&str, &crate::map::Base)>,
    lines: &[LineSpec],
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
        let mut disabled = BTreeSet::new();
        for line in lines {
            if let Some(holders) = base.lines.get(&line.line) {
                for holder in &holders.holders {
                    if disabled.insert(holder.clone()) {
                        push_fragment(
                            form,
                            &mut fragments,
                            &mut fragment,
                            holder,
                            &format!(
                                "disabled: base {name:?} {}: {}; the board gives {} to Lemnos",
                                holder, holders.reason, line.line
                            ),
                            &["status = \"disabled\";".into()],
                        );
                    }
                }
            }
        }
    } else if !lines.is_empty() {
        let _ = writeln!(
            fragments,
            "\t// No base named: lines the board gives to Lemnos are not checked against the base tree."
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

/// The SoC's GPIO number of a stable GPIO name, which the boot fragment writes: the chip's first
/// number plus the line.
fn boot_number(name: &str, map: &SocMap) -> Result<u32, String> {
    let (label, line) = gpio_line(name, map)?;
    let chip = map
        .chip_labelled(label)
        .ok_or_else(|| format!("line {name:?}: no GPIO chip labelled {label:?}"))?;
    Ok(chip.number_base + u32::from(line))
}

/// The firmware configuration: the state each board line has from power-on until the kernel
/// takes it. Every `[[lines]]` entry with a `boot` state is written, with its reason.
fn boot(board: &BoardFile, map: &SocMap, chosen: &[Chosen<'_>]) -> Result<String, String> {
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
    for line in &board.lines {
        let Some(level) = line.boot.as_deref() else {
            continue;
        };
        let state = if level == "high" { "dh" } else { "dl" };
        let gpio = boot_number(&line.line, map)?;
        let reason = line.reason.as_deref().unwrap_or_default();
        let _ = writeln!(out, "# {}: boot {level}: {reason}", line.line);
        let _ = writeln!(out, "gpio={gpio}=op,{state}");
    }
    // A device port's VBUS line, held at its off level from power-on: the board's statement.
    for entry in chosen {
        let Some(VbusPlan::Hold { line, off, reason }) = &entry.vbus else {
            continue;
        };
        let gpio = boot_number(line, map)?;
        let state = if off == "high" { "dh" } else { "dl" };
        let _ = writeln!(
            out,
            "# {line}: VBUS of {} held {off} (off) from power-on: {reason}",
            entry.key
        );
        let _ = writeln!(out, "gpio={gpio}=op,{state}");
    }
    Ok(out)
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
