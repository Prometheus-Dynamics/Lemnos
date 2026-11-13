//! Writes the overlay and the boot fragment for a board. Every node in them has its reason beside
//! it, and every refusal names the board entry, the map, and what the map has instead.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use lemnos_host::board::{BoardFile, LineSpec};

use crate::map::{Controller, Form, Kind, PinSet, SocMap};

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
}

/// Makes the overlay and the boot fragment for `board`, using `map`. Refuses what the map cannot
/// provide, with the entry that asks for it.
pub fn generate(board: &BoardFile, map: &SocMap) -> Result<Output, String> {
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
    let chosen = choose(board, map)?;
    check_lines(board, map)?;
    check_devices(board, map, &chosen, base.map(|(_, b)| b))?;
    let overlay = overlay(board, &chosen, base, &board.lines, map.form)?;
    let boot = boot(board, map)?;
    let modprobe = modprobe(board, &chosen);
    Ok(Output {
        overlay,
        boot,
        modprobe,
    })
}

/// Each `[[hardware]]` entry, resolved against the map, and the GPIOs no two entries share.
fn choose<'a>(board: &'a BoardFile, map: &'a SocMap) -> Result<Vec<Chosen<'a>>, String> {
    let mut chosen = Vec::new();
    let mut controllers = BTreeSet::new();
    let mut gpios: BTreeMap<u16, &str> = BTreeMap::new();
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
        chosen.push(Chosen {
            name: runtime,
            key: name,
            controller,
            pins,
            module,
            unconfined: hardware.unconfined,
            confined: hardware.confined,
        });
    }
    Ok(chosen)
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

/// Every name a device uses is one the generator makes something of, and every device makes
/// something: a node enabled, a line freed, or a module option. A device that makes nothing is
/// refused, unless its driver needs nothing from the SoC (the map's `needs_nothing`).
fn check_devices(
    board: &BoardFile,
    map: &SocMap,
    chosen: &[Chosen<'_>],
    base: Option<&crate::map::Base>,
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
            if name.starts_with("gpiochip") || name.starts_with("pwmchip") {
                return Err(format!(
                    "device {:?} uses {name:?}: the kernel numbers gpio and pwm chips as it probes \
                     them, and no device-tree alias sets the number, so this generator cannot make \
                     it come out as written; name the chip stably: gpio:LABEL:LINE or pwm:NODE:CHANNEL",
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
        if makes == 0 && !map.needs_nothing.iter().any(|d| d == &device.driver) {
            return Err(format!(
                "device {:?} (driver {:?}) has no hardware name, so its needs produce nothing in any \
                 output; name its hardware, or list the driver in the map's `needs_nothing`",
                device.id, device.driver
            ));
        }
    }
    Ok(())
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

/// The firmware configuration: the state each board line has from power-on until the kernel
/// takes it. Every `[[lines]]` entry with a `boot` state is written, with its reason.
fn boot(board: &BoardFile, map: &SocMap) -> Result<String, String> {
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
        let (_, gpio) = gpio_line(&line.line, map)?;
        let reason = line.reason.as_deref().unwrap_or_default();
        let _ = writeln!(out, "# {}: boot {level}: {reason}", line.line);
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
