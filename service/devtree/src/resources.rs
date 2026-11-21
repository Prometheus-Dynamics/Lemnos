//! The backend resource names a board file names, checked at validate time.
//!
//! Every name in a device's `bus` or `uses` (a name that is not one of the board's device ids) and
//! every `[[lines]]` entry is parsed with the one grammar the Linux backend's parsers agree with
//! (`lemnos_linux_names`). A name of no known form, or a malformed one, is refused by name on every
//! board, with or without a `soc`.
//!
//! Where the SoC map holds the fact, the name is also checked against it: `i2c-N` and `spidevB.C`
//! against the SoC's buses, `i2c:of=`, `watchdog:` and `pwm:` against its device-tree nodes,
//! `gpio:LABEL:LINE` against its chips, `udc:` against its USB controllers, and `pio-i2c:` and
//! `pio-out:` against its PIO block. The numbered forms (`gpiochipN`, `pwmchipN`, `watchdogN`,
//! `ttyNAME`, `usb:` ports, the module's blocks) name a thing of the running kernel that the map does
//! not hold, so they are checked for their grammar only.

use std::collections::BTreeSet;

use lemnos_host::board::BoardFile;
use lemnos_linux_names::{Name, names_a_form, parse};

use crate::map::{Controller, Kind, SocMap};

/// Every name the board names as a resource, with where it is named: a device's `bus` or `uses`
/// (a value that is one of the board's device ids is a device, not a name), and a line.
fn names(board: &BoardFile) -> Vec<(String, &str)> {
    let ids: BTreeSet<&str> = board.devices.iter().map(|d| d.id.as_str()).collect();
    let mut found = Vec::new();
    for device in &board.devices {
        // A `uses` value is a device id or a resource name. One that names no form and no device
        // is left to the uses check, which refuses it as a device the board does not have.
        let uses = device
            .uses
            .values()
            .filter(|name| !ids.contains(name.as_str()) && names_a_form(name));
        for name in device.bus.iter().chain(uses) {
            found.push((format!("device {:?}", device.id), name.as_str()));
        }
    }
    for line in &board.lines {
        found.push((format!("[[lines]] {}", line.line), line.line.as_str()));
    }
    found
}

/// Whether a name is one the SoC map can check: the board needs the map only then.
fn soc_checked(name: &str) -> bool {
    matches!(
        parse(name),
        Ok(Name::I2c { .. }
            | Name::I2cOf { .. }
            | Name::PioI2c
            | Name::PioOut { .. }
            | Name::Spidev { .. }
            | Name::GpioLabel { .. }
            | Name::PwmNode { .. }
            | Name::WatchdogNode { .. }
            | Name::Udc { .. })
    )
}

/// Whether the board names a resource the SoC map can check: the only case where it needs the map.
pub fn any_checkable(board: &BoardFile) -> bool {
    names(board).iter().any(|(_, name)| soc_checked(name))
}

/// Each name of the board that is not a name of a known form, or is malformed, as a refusal
/// naming the device or line and the name. Empty when every name is well formed. Checked on every
/// board, with or without a `soc`.
pub fn grammar_refusals(board: &BoardFile) -> Vec<String> {
    names(board)
        .into_iter()
        .filter_map(|(at, name)| {
            parse(name)
                .err()
                .map(|why| format!("{at}: {name:?}: {why}"))
        })
        .collect()
}

/// Each name the map can check that the SoC does not have, as a refusal naming the device or line,
/// the name, and what the SoC has. Empty when every such name is in the map. Only for a board with
/// a `soc`, with its map.
pub fn refusals(board: &BoardFile, map: &SocMap) -> Vec<String> {
    names(board)
        .into_iter()
        .filter_map(|(at, name)| refusal(name, map).map(|why| format!("{at}: {name}: {why}")))
        .collect()
}

/// Why the map does not have `name`, or `None` when it has it, or the map does not hold the fact.
fn refusal(name: &str, map: &SocMap) -> Option<String> {
    match parse(name).ok()? {
        Name::I2c { number } => {
            let alias = format!("i2c{number}");
            missing(map, Kind::I2c, "I2C bus", name, |c| {
                c.alias.as_deref() == Some(alias.as_str())
            })
        }
        Name::I2cOf { node } => missing(map, Kind::I2c, "I2C bus at", node, |c| c.path == node),
        Name::PioI2c | Name::PioOut { .. } => {
            if map.controllers.values().any(|c| c.kind == Kind::Pio) {
                return None;
            }
            Some(format!(
                "the {} SoC has no PIO block (a pio-i2c: or pio-out: name needs one)",
                map.soc
            ))
        }
        Name::Spidev { bus, .. } => {
            let alias = format!("spi{bus}");
            missing(map, Kind::Spi, "SPI bus", name, |c| {
                c.alias.as_deref() == Some(alias.as_str())
            })
        }
        Name::WatchdogNode { node } => missing(map, Kind::Watchdog, "watchdog node", node, |c| {
            c.path == node
        }),
        Name::PwmNode { node, .. } => missing(map, Kind::Pwm, "PWM node", node, |c| c.path == node),
        Name::GpioLabel { label, .. } => {
            if map.chips.values().any(|chip| chip.label == label) {
                return None;
            }
            let known: Vec<&str> = map.chips.values().map(|chip| chip.label.as_str()).collect();
            Some(format!(
                "the {} SoC has no GPIO chip labelled {label:?} (its chips: {})",
                map.soc,
                list(&known)
            ))
        }
        Name::Udc { name: udc } => missing(map, Kind::UsbPeripheral, "UDC", udc, |c| {
            c.sysfs.as_deref() == Some(udc)
        }),
        // The numbered forms and the module's blocks name a thing of the running kernel that the
        // map does not hold: their grammar is all that is checked.
        Name::GpioChip { .. }
        | Name::PwmChip { .. }
        | Name::WatchdogNumber { .. }
        | Name::Tty { .. }
        | Name::UsbPort
        | Name::UsbId
        | Name::UsbClass
        | Name::Block { .. } => None,
    }
}

/// The refusal for a name no controller of `kind` matches (`matches`), or `None` when one does.
/// `what` names the kind of thing in the message and `named` is the name as the board wrote it.
fn missing(
    map: &SocMap,
    kind: Kind,
    what: &str,
    named: &str,
    matches: impl Fn(&Controller) -> bool,
) -> Option<String> {
    let of_kind = || map.controllers.values().filter(move |c| c.kind == kind);
    if of_kind().any(&matches) {
        return None;
    }
    let have: Vec<&str> = of_kind()
        .map(|c| {
            c.alias
                .as_deref()
                .or(c.sysfs.as_deref())
                .unwrap_or(c.path.as_str())
        })
        .collect();
    Some(format!(
        "the {} SoC has no {what} {named} (it has: {})",
        map.soc,
        list(&have)
    ))
}

fn list(items: &[&str]) -> String {
    if items.is_empty() {
        "none".to_string()
    } else {
        items.join(", ")
    }
}
