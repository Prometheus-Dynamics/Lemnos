//! Compares a live device tree (`/proc/device-tree`) and the kernel's platform devices with what a
//! board file needs. A mismatch is reported, never worked around: the message names the node, and
//! what is in its way.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use lemnos_host::board::BoardFile;

use crate::generate::{NO_VBUS_NODE, VbusPlan, vbus_plan};
use crate::map::{Base, Controller, Kind, PinSet, SocMap};

/// What the check reads: the live device tree and the platform device directory.
pub struct Live<'a> {
    /// The device tree, as `/proc/device-tree` shows it (a directory per node).
    pub tree: &'a Path,
    /// The platform devices, as `/sys/bus/platform/devices` shows them.
    pub sys: &'a Path,
    /// The GPIO chips on the machine, as `(label, line count)`: the kernel's own chip information.
    pub gpio_chips: &'a [(String, u32)],
    /// The USB device controllers the kernel lists, as `/sys/class/udc` shows them.
    pub udc: &'a Path,
    /// The kernel's GPIO state, as debugfs shows it (`/sys/kernel/debug/gpio`): the level of each
    /// line a driver has requested. Read for a board's VBUS line.
    pub gpio_debug: &'a Path,
    /// The kernel's regulators, as `/sys/class/regulator` shows them (`name`, `state`, `num_users`).
    pub regulators: &'a Path,
}

/// What the check found: each problem names what is wrong and where, and each note is a fact the
/// operator must confirm on the board (a supply that is enabled, a line that cannot be read here).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Findings {
    /// Each mismatch with the board. Empty when the live system matches.
    pub problems: Vec<String>,
    /// Facts the check cannot settle from the live system.
    pub notes: Vec<String>,
}

/// Every problem with the live system against the board, and the notes. Empty when it matches.
pub fn check(board: &BoardFile, map: &SocMap, live: &Live<'_>) -> Findings {
    let mut problems = Vec::new();
    let mut notes = Vec::new();
    let phandles = phandles(live.tree);
    let base = board
        .board
        .base
        .as_deref()
        .and_then(|name| map.bases.get(name).map(|b| (name, b)));
    for hardware in &board.hardware {
        let Some(controller) = map.controllers.get(&hardware.controller) else {
            problems.push(format!(
                "controller {:?} is not in the map",
                hardware.controller
            ));
            continue;
        };
        let label = hardware.name.as_deref().unwrap_or(&hardware.controller);
        let node = live.tree.join(controller.path.trim_start_matches('/'));
        if !node.is_dir() {
            problems.push(format!(
                "{label}: node absent: {} (the overlay should enable it)",
                controller.path
            ));
            continue;
        }
        if status_of(&node) == "disabled" {
            problems.push(format!("{label}: node disabled: {}", controller.path));
        }
        if controller.kind == Kind::UsbPeripheral {
            let findings = usb_port(
                hardware,
                &hardware.controller,
                controller,
                &node,
                live,
                &phandles,
                base,
            );
            problems.extend(findings.problems);
            notes.extend(findings.notes);
        }
        // A node the board gives to the module must not be bound by the stock driver the map names.
        if let Some(role) = &hardware.module
            && let Some(name) = &controller.sysfs
            && let Some(stock) = &controller.stock_driver
            && let Some(bound) = bound_driver(live.sys, name)
            && bound == *stock
        {
            problems.push(format!(
                "{label}: stock driver {stock} is bound to {name}, and the board gives that node to \
                 the module ({role}); the module must own it (set driver_override, or unbind {stock})"
            ));
        }
        if let Some(pins) = &hardware.pins
            && let Some(set) = controller.pins.get(pins)
        {
            if !live.tree.join(set.path.trim_start_matches('/')).is_dir() {
                problems.push(format!("{label}: pin set {pins:?} is absent: {}", set.path));
            }
            problems.extend(pin_conflicts(
                label,
                &controller.path,
                set,
                map,
                &phandles,
                live.tree,
            ));
        }
    }
    // A stable GPIO name: a chip with its label must be on the machine, with the line in range.
    let mut lines: Vec<(&str, &str)> = board
        .lines
        .iter()
        .map(|l| (l.line.as_str(), "[[lines]]"))
        .collect();
    for device in &board.devices {
        for name in device.bus.iter().chain(device.uses.values()) {
            lines.push((name.as_str(), device.id.as_str()));
        }
    }
    for (name, owner) in lines {
        let Some(rest) = name.strip_prefix("gpio:") else {
            continue;
        };
        let Some((label, line)) = rest.rsplit_once(':') else {
            continue;
        };
        let Ok(line) = line.parse::<u32>() else {
            continue;
        };
        match live.gpio_chips.iter().find(|(found, _)| found == label) {
            None => problems.push(format!(
                "{owner}: {name}: no GPIO chip labelled {label:?} on this machine (the chips are: {})",
                if live.gpio_chips.is_empty() {
                    "none".to_string()
                } else {
                    live.gpio_chips
                        .iter()
                        .map(|(l, _)| l.clone())
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            )),
            Some((_, count)) if line >= *count => problems.push(format!(
                "{owner}: {name}: line {line} is past the {count} lines of the chip labelled {label:?}"
            )),
            Some(_) => {}
        }
    }
    // A PWM or watchdog name is a node of the live tree: it must be there and enabled, as the
    // overlay makes it.
    for device in &board.devices {
        for name in device.bus.iter().chain(device.uses.values()) {
            let node = match name.strip_prefix("watchdog:") {
                Some(node) => node,
                None => match name
                    .strip_prefix("pwm:")
                    .and_then(|rest| rest.rsplit_once(':'))
                {
                    Some((node, _)) => node,
                    None => continue,
                },
            };
            let dir = live.tree.join(node.trim_start_matches('/'));
            if !dir.is_dir() {
                problems.push(format!(
                    "device {:?}: {name}: node absent: {node} (the overlay should enable it)",
                    device.id
                ));
            } else if status_of(&dir) == "disabled" {
                problems.push(format!(
                    "device {:?}: {name}: node disabled: {node}",
                    device.id
                ));
            }
        }
    }
    Findings { problems, notes }
}

/// What a USB device port needs live: its node in peripheral mode, each node it needs enabled, its
/// VBUS as the board states it, and, for a named port, the UDC the kernel lists under
/// `/sys/class/udc` for the gadget.
fn usb_port(
    hardware: &lemnos_host::board::HardwareSpec,
    key: &str,
    controller: &Controller,
    node: &Path,
    live: &Live<'_>,
    phandles: &BTreeMap<u32, String>,
    base: Option<(&str, &Base)>,
) -> Findings {
    let mut problems = Vec::new();
    let mut notes = Vec::new();
    let linked: Vec<u32> = fs::read(node.join("phys"))
        .map(|bytes| {
            bytes
                .as_chunks::<4>()
                .0
                .iter()
                .map(|four| u32::from_be_bytes(*four))
                .collect()
        })
        .unwrap_or_default();
    let who = format!("USB port {key}");
    let mode = fs::read_to_string(node.join("dr_mode"))
        .map(|m| m.trim_end_matches('\0').trim().to_string())
        .ok();
    match mode.as_deref() {
        Some("peripheral") => {}
        Some(other) => problems.push(format!(
            "{who}: host mode: {} has dr_mode {other:?}, so the port is not a device; the overlay sets \"peripheral\"",
            controller.path
        )),
        None => problems.push(format!(
            "{who}: {} has no dr_mode, so the port is not in peripheral mode; the overlay sets it",
            controller.path
        )),
    }
    for need in &controller.needs {
        let asked = match need.option.as_deref() {
            None => true,
            Some(option) => hardware.options.iter().any(|o| o.as_str() == option),
        };
        if !asked {
            continue;
        }
        let dir = live.tree.join(need.path.trim_start_matches('/'));
        if !dir.is_dir() {
            problems.push(format!(
                "{who}: {} is absent, and the port needs it ({})",
                need.path, need.reason
            ));
        } else if status_of(&dir) == "disabled" {
            problems.push(format!(
                "{who}: PHY or node disabled: {} is disabled, and the port needs it ({})",
                need.path, need.reason
            ));
        } else if need.phy.is_some() {
            let reached = phandles
                .iter()
                .find(|(_, path)| path.as_str() == need.path)
                .is_some_and(|(phandle, _)| linked.contains(phandle));
            if !reached {
                problems.push(format!(
                    "{who}: PHY link missing: {} has no phys entry for {} (phy-names {:?}), so the \
                     port cannot reach that PHY",
                    controller.path,
                    need.path,
                    need.phy.as_deref().unwrap_or_default()
                ));
            }
        }
    }
    if let (Some(name), Some(udc)) = (&hardware.name, &controller.sysfs)
        && name.starts_with("udc:")
        && !live.udc.join(udc).exists()
    {
        problems.push(format!(
            "{who}: no UDC {udc:?} under {}: the gadget's controller is not in device mode, so the \
             gadget cannot bind it",
            live.udc.display()
        ));
    }
    vbus(
        hardware,
        key,
        controller,
        live,
        phandles,
        base,
        &who,
        &mut problems,
        &mut notes,
    );
    Findings { problems, notes }
}

/// The VBUS of a device port, live. The board's statement must be one the generator accepts for the
/// base tree's supply (the same rule, [`vbus_plan`]). Then the PHY's `phy-supply` must be the stand-in
/// or absent, or else the board's line must read its off level. A board that holds VBUS by a line is
/// checked on the line; an always-on supply that is enabled is reported as a note, for a meter.
#[allow(clippy::too_many_arguments)]
fn vbus(
    hardware: &lemnos_host::board::HardwareSpec,
    key: &str,
    controller: &Controller,
    live: &Live<'_>,
    phandles: &BTreeMap<u32, String>,
    base: Option<(&str, &Base)>,
    who: &str,
    problems: &mut Vec<String>,
    notes: &mut Vec<String>,
) {
    let fact = base.and_then(|(_, b)| b.vbus.get(key));
    let base_name = base.map(|(name, _)| name);
    let runtime = hardware.name.as_deref().unwrap_or("");
    let plan = match vbus_plan(runtime, key, hardware, base_name, fact) {
        Ok(plan) => plan,
        Err(why) => {
            problems.push(why);
            return;
        }
    };
    let Some(phy) = controller
        .needs
        .iter()
        .find(|n| n.phy.as_deref() == Some("usb2-phy"))
    else {
        return;
    };
    let phy_dir = live.tree.join(phy.path.trim_start_matches('/'));
    if phy_dir.is_dir() {
        let supply = fs::read(phy_dir.join("phy-supply")).ok().and_then(|bytes| {
            let four = <[u8; 4]>::try_from(bytes.as_slice()).ok()?;
            phandles.get(&u32::from_be_bytes(four)).cloned()
        });
        match (supply.as_deref(), &plan) {
            (None, _) => {}
            (Some(path), _) if path == NO_VBUS_NODE => {}
            // A board that holds VBUS by a line: the supply is acceptable while the line reads
            // its off level, which `hold` checks below.
            (Some(_), VbusPlan::Hold { .. }) => {}
            (Some(path), _) => problems.push(format!(
                "{who}: PHY {} still has phy-supply {path}, which the generated overlay replaces with \
                 the stand-in that supplies nothing; this tree was not made from the board file",
                phy.path
            )),
        }
    }
    if let VbusPlan::Hold { line, off, .. } = &plan {
        hold(line, off, who, live, problems, notes);
    }
    if let Some(fact) = fact
        && fact.always_on
        && let Some(name) = fact.regulator_name.as_deref()
    {
        regulator(name, who, &plan, live, notes);
    }
}

/// The level a `vbus` line reads, against its off level: a mismatch is a problem; a line no driver
/// has requested cannot be read here, which is a note for a meter.
fn hold(
    line: &str,
    off: &str,
    who: &str,
    live: &Live<'_>,
    problems: &mut Vec<String>,
    notes: &mut Vec<String>,
) {
    let Some((label, number)) = line
        .strip_prefix("gpio:")
        .and_then(|rest| rest.rsplit_once(':'))
        .and_then(|(label, n)| Some((label, n.parse::<u32>().ok()?)))
    else {
        problems.push(format!("{who}: VBUS line {line:?} is not gpio:LABEL:LINE"));
        return;
    };
    let want = if off == "high" { "hi" } else { "lo" };
    match gpio_level(live.gpio_debug, label, number) {
        Err(why) => problems.push(format!("{who}: VBUS line {line}: {why}")),
        Ok(None) => notes.push(format!(
            "{who}: VBUS line {line} is held {off} from power-on by the firmware; no driver has \
             requested it, so its level cannot be read here: measure VBUS in device mode"
        )),
        Ok(Some(level)) if level == want => {}
        Ok(Some(level)) => problems.push(format!(
            "{who}: VBUS line {line} reads {level}, but the board says its off level is {off}: VBUS may \
             be on"
        )),
    }
}

/// An always-on supply the base names, read from `/sys/class/regulator`: enabled is a note, with
/// what the board says about it.
fn regulator(name: &str, who: &str, plan: &VbusPlan, live: &Live<'_>, notes: &mut Vec<String>) {
    let found = fs::read_dir(live.regulators).ok().and_then(|entries| {
        entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .find(|dir| {
                fs::read_to_string(dir.join("name"))
                    .map(|n| n.trim() == name)
                    .unwrap_or(false)
            })
    });
    let Some(dir) = found else {
        notes.push(format!(
            "{who}: always-on regulator {name:?} is not listed under {}; its state is not read here",
            live.regulators.display()
        ));
        return;
    };
    let state = fs::read_to_string(dir.join("state"))
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "unknown".into());
    let users = fs::read_to_string(dir.join("num_users"))
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "?".into());
    if state == "enabled" {
        let says = match plan {
            VbusPlan::Undriven { .. } => "the board says this port is not driven by it",
            VbusPlan::Hold { .. } => "the board says VBUS is switched by a line",
            VbusPlan::NoSupply => "the base has no supply on this port",
        };
        notes.push(format!(
            "{who}: always-on regulator {name:?} is enabled (num_users {users}); {says}: measure VBUS"
        ));
    }
}

/// The level of a GPIO line, from the kernel's debugfs GPIO listing: `lo` or `hi`, or `None` when no
/// driver has requested the line (so the listing has no entry for it). The listing names each line
/// by its global number, the chip's base plus the line, under the chip's header with its label.
pub fn gpio_level(debug: &Path, label: &str, line: u32) -> Result<Option<String>, String> {
    let text = fs::read_to_string(debug).map_err(|e| {
        format!(
            "cannot read the GPIO levels from {} ({e}); run as root with debugfs mounted",
            debug.display()
        )
    })?;
    let mut base = None;
    for raw in text.lines() {
        if raw.contains(": GPIOs ") {
            let header = raw.trim_end().trim_end_matches(':');
            let chip = header.rsplit(", ").find(|part| *part != "can sleep");
            base = match (chip, header.split("GPIOs ").nth(1)) {
                (Some(chip), Some(range)) if chip == label => range
                    .split(|c: char| !c.is_ascii_digit())
                    .next()
                    .and_then(|first| first.parse::<u32>().ok()),
                _ => None,
            };
            continue;
        }
        let Some(first) = base else {
            continue;
        };
        let Some(rest) = raw.trim_start().strip_prefix("gpio-") else {
            continue;
        };
        let number = rest
            .split(|c: char| !c.is_ascii_digit())
            .next()
            .and_then(|n| n.parse::<u32>().ok());
        if number != Some(first + line) {
            continue;
        }
        let after = rest.rfind(')').map(|i| &rest[i + 1..]).unwrap_or_default();
        return Ok(after
            .split_whitespace()
            .find(|word| matches!(*word, "lo" | "hi"))
            .map(str::to_string));
    }
    Ok(None)
}

/// The `status` a node has. A node with no `status` is enabled.
fn status_of(node: &Path) -> String {
    fs::read_to_string(node.join("status"))
        .map(|s| s.trim_end_matches('\0').trim().to_string())
        .unwrap_or_else(|_| "okay".into())
}

/// Which driver the kernel bound to a platform device, by the name of its driver link.
fn bound_driver(sys: &Path, device: &str) -> Option<String> {
    let link = fs::read_link(sys.join(device).join("driver")).ok()?;
    link.file_name().map(|n| n.to_string_lossy().into_owned())
}

/// The phandle of every node in the live tree, to its path.
fn phandles(tree: &Path) -> BTreeMap<u32, String> {
    let mut found = BTreeMap::new();
    walk(tree, "", &mut |path, dir| {
        if let Ok(bytes) = fs::read(dir.join("phandle"))
            && let Ok(four) = <[u8; 4]>::try_from(bytes.as_slice())
        {
            found.insert(u32::from_be_bytes(four), path.to_string());
        }
    });
    found
}

/// Calls `visit` for each node under `dir`, with its path from the root.
fn walk(dir: &Path, path: &str, visit: &mut dyn FnMut(&str, &Path)) {
    let here = if path.is_empty() {
        "/".to_string()
    } else {
        path.to_string()
    };
    visit(&here, dir);
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut children: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir())
        .collect();
    children.sort();
    for child in children {
        let name = child
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let child_path = format!("{}/{name}", path);
        walk(&child, &child_path, visit);
    }
}

/// A pin set that another enabled node also configures, sharing one of its GPIOs.
fn pin_conflicts(
    label: &str,
    own_path: &str,
    set: &PinSet,
    map: &SocMap,
    phandles: &BTreeMap<u32, String>,
    tree: &Path,
) -> Vec<String> {
    let mut problems = Vec::new();
    let mut seen = Vec::new();
    walk(tree, "", &mut |path, dir| {
        if path == own_path || status_of(dir) == "disabled" {
            return;
        }
        let Ok(bytes) = fs::read(dir.join("pinctrl-0")) else {
            return;
        };
        for four in bytes.as_chunks::<4>().0 {
            let phandle = u32::from_be_bytes(*four);
            let Some(target) = phandles.get(&phandle) else {
                continue;
            };
            let Some(gpios) = gpios_at(map, target) else {
                continue;
            };
            let shared: Vec<u16> = gpios
                .into_iter()
                .filter(|g| set.gpios.contains(g))
                .collect();
            if !shared.is_empty() && !seen.contains(&(path.to_string(), shared.clone())) {
                seen.push((path.to_string(), shared.clone()));
                problems.push(format!(
                    "{label}: pin {} is held by {path} (its pin set {target} is enabled)",
                    shared
                        .iter()
                        .map(u16::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }
    });
    problems
}

/// The GPIOs of the map's pin set at `path`, if the map has one there.
fn gpios_at(map: &SocMap, path: &str) -> Option<Vec<u16>> {
    map.controllers
        .values()
        .flat_map(|c| c.pins.values())
        .find(|set| set.path == path)
        .map(|set| set.gpios.clone())
}
