//! Writes the overlay and the boot fragment for a board. Every node in them has its reason beside
//! it, and every refusal names the board entry, the map, and what the map has instead.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use lemnos_host::board::{BoardFile, LineSpec};

use crate::map::{Controller, Kind, PinSet, SocMap};

/// What the generator makes: the overlay the OS layer compiles and loads, and the boot fragment
/// the firmware reads before the kernel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    /// Device-tree source: an overlay (`/plugin/;`), fragments by `target-path`.
    pub overlay: String,
    /// Firmware configuration lines, one `gpio=N=op,dhN` per line the board gives a boot state.
    pub boot: String,
}

/// One board entry the generator will enable: the entry, its controller and its pin set.
struct Chosen<'a> {
    name: &'a str,
    controller: &'a Controller,
    pins: Option<&'a PinSet>,
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
    check_device_names(board, &chosen)?;
    let overlay = overlay(board, &chosen, base, &board.lines)?;
    let boot = boot(board)?;
    Ok(Output { overlay, boot })
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
        chosen.push(Chosen {
            name: runtime,
            controller,
            pins,
        });
    }
    Ok(chosen)
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

/// Every runtime name a device uses has a hardware entry that gives it, or needs none (an I2C
/// bus made by PIO carries its pins in its name, and needs only the PIO block).
fn check_device_names(board: &BoardFile, chosen: &[Chosen<'_>]) -> Result<(), String> {
    let ids: BTreeSet<&str> = board.devices.iter().map(|d| d.id.as_str()).collect();
    let named: BTreeSet<&str> = chosen.iter().map(|c| c.name).collect();
    let has_pio = chosen.iter().any(|c| c.controller.kind == Kind::Pio);
    for device in &board.devices {
        let names = device
            .bus
            .iter()
            .map(String::as_str)
            .chain(device.uses.values().map(String::as_str));
        for name in names {
            if ids.contains(name) {
                continue;
            }
            if name.starts_with("gpiochip") || name.starts_with("pwmchip") {
                return Err(format!(
                    "device {:?} uses {name:?}: the kernel numbers gpio and pwm chips as it probes \
                     them, and no device-tree alias sets the number, so this generator cannot make \
                     it come out as written",
                    device.id
                ));
            }
            if name.starts_with("pio-i2c:") || name.starts_with("pio-out:") {
                if !has_pio {
                    return Err(format!(
                        "device {:?} uses {name:?}, which needs the PIO block: add a [[hardware]] \
                         entry for controller \"pio\"",
                        device.id
                    ));
                }
            } else if (name.starts_with("i2c-")
                || name.starts_with("spidev")
                || name.starts_with("udc:"))
                && !named.contains(name)
            {
                return Err(format!(
                    "device {:?} uses {name:?}, which no [[hardware]] entry names",
                    device.id
                ));
            }
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
    let _ = writeln!(out, "/dts-v1/;");
    let _ = writeln!(out, "/plugin/;");
    let _ = writeln!(out, "/ {{");
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
            &mut fragments,
            &mut fragment,
            &controller.path,
            &reason,
            &body,
        );
        if let Some(alias) = &controller.alias {
            let alias_body = format!("aliases {{ {alias} = \"{}\"; }};", controller.path);
            push_fragment(
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
    let _ = writeln!(out, "}};");
    Ok(out)
}

fn push_fragment(out: &mut String, n: &mut usize, target: &str, reason: &str, body: &[String]) {
    let _ = writeln!(out, "\t// {reason}");
    let _ = writeln!(out, "\tfragment@{n} {{");
    let _ = writeln!(out, "\t\ttarget-path = \"{target}\";");
    let _ = writeln!(out, "\t\t__overlay__ {{");
    for line in body {
        let _ = writeln!(out, "\t\t\t{line}");
    }
    let _ = writeln!(out, "\t\t}};");
    let _ = writeln!(out, "\t}};");
    *n += 1;
}

/// The firmware configuration: the state each board line has from power-on until the kernel
/// takes it.
fn boot(board: &BoardFile) -> Result<String, String> {
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
        let state = match level {
            "high" => "dh",
            "low" => "dl",
            other => {
                return Err(format!(
                    "line {:?}: boot {other:?} is not \"low\" or \"high\"",
                    line.line
                ));
            }
        };
        let gpio = line
            .line
            .split_once(':')
            .and_then(|(_, n)| n.parse::<u16>().ok())
            .ok_or_else(|| format!("line {:?} is not `chip:N`", line.line))?;
        let reason = line
            .reason
            .as_deref()
            .ok_or_else(|| format!("line {:?}: boot {level:?} needs a `reason`", line.line))?;
        let _ = writeln!(out, "# {}: boot {level}: {reason}", line.line);
        let _ = writeln!(out, "gpio={gpio}=op,{state}");
    }
    Ok(out)
}
