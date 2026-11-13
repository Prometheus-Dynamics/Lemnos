//! Compares a live device tree (`/proc/device-tree`) and the kernel's platform devices with what a
//! board file needs. A mismatch is reported, never worked around: the message names the node, and
//! what is in its way.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use lemnos_host::board::BoardFile;

use crate::map::{PinSet, SocMap};

/// What the check reads: the live device tree and the platform device directory.
pub struct Live<'a> {
    /// The device tree, as `/proc/device-tree` shows it (a directory per node).
    pub tree: &'a Path,
    /// The platform devices, as `/sys/bus/platform/devices` shows them.
    pub sys: &'a Path,
    /// The GPIO chips on the machine, as `(label, line count)`: the kernel's own chip information.
    pub gpio_chips: &'a [(String, u32)],
}

/// Every problem with the live system against the board, one message each. Empty when it matches.
pub fn check(board: &BoardFile, map: &SocMap, live: &Live<'_>) -> Vec<String> {
    let mut problems = Vec::new();
    let phandles = phandles(live.tree);
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
    // A PWM name is a node of the live tree: it must be there and enabled, as the overlay makes it.
    for device in &board.devices {
        for name in device.bus.iter().chain(device.uses.values()) {
            let Some(node) = name
                .strip_prefix("pwm:")
                .and_then(|rest| rest.rsplit_once(':'))
                .map(|(node, _)| node)
            else {
                continue;
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
    problems
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
