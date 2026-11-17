//! The backend resource names a board file names, checked against the SoC map at validate time.
//!
//! A name such as `i2c-10` or `watchdog:/path` means a bus, node or line of the running kernel, and
//! the backend finds out at bind whether it is there. The SoC map says which buses, nodes, chips
//! and UDCs the SoC has, so a name the map does not have is refused here, by name, before any
//! device is made. Only the forms the map describes are checked: `i2c-N`, `spidevB.C`,
//! `watchdog:NODE`, `pwm:NODE:CHANNEL`, `gpio:LABEL:LINE` and `udc:NAME`. The rest (`usb:`,
//! `pio-i2c:`, `dma:`, the numbered `gpiochipN:LINE` and `pwmchipN:CHANNEL`) are left to the bind.

use lemnos_host::board::BoardFile;

use crate::map::{Controller, Kind, SocMap};

/// Whether the board names any checkable resource: the only case where it needs the SoC map.
pub fn any_checkable(board: &BoardFile) -> bool {
    board
        .devices
        .iter()
        .flat_map(|device| device.bus.iter().chain(device.uses.values()))
        .chain(board.lines.iter().map(|line| &line.line))
        .any(|name| form(name).is_some())
}

/// Each checkable name the board names (in a device's `bus` or `uses`, or a `[[lines]]` entry)
/// that the map does not have, as a refusal naming the device or line, the name, and what the SoC
/// has. Empty when every checkable name is in the map.
pub fn refusals(board: &BoardFile, map: &SocMap) -> Vec<String> {
    let mut found = Vec::new();
    for device in &board.devices {
        for name in device.bus.iter().chain(device.uses.values()) {
            if let Some(why) = refusal(name, map) {
                found.push(format!("device {:?}: {name}: {why}", device.id));
            }
        }
    }
    for line in &board.lines {
        if let Some(why) = refusal(&line.line, map) {
            found.push(format!("[[lines]] {}: {why}", line.line));
        }
    }
    found
}

/// A name of a form the map describes, with its parts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Form<'a> {
    /// `i2c-N`: the bus whose alias is `i2cN`.
    I2c(u32),
    /// `spidevB.C`: the SPI bus whose alias is `spiB`.
    Spi(u32),
    /// `watchdog:NODE`.
    Watchdog(&'a str),
    /// `pwm:NODE:CHANNEL`.
    Pwm(&'a str),
    /// `gpio:LABEL:LINE`.
    Gpio(&'a str),
    /// `udc:NAME`.
    Udc(&'a str),
}

/// The form of `name`, or `None` when it is not one the map describes.
fn form(name: &str) -> Option<Form<'_>> {
    if let Some(number) = name.strip_prefix("i2c-") {
        return digits(number).map(Form::I2c);
    }
    if let Some(rest) = name.strip_prefix("spidev") {
        let (bus, _) = rest.split_once('.')?;
        return digits(bus).map(Form::Spi);
    }
    if let Some(node) = name.strip_prefix("watchdog:") {
        return Some(Form::Watchdog(node));
    }
    if let Some(rest) = name.strip_prefix("pwm:") {
        let (node, channel) = rest.rsplit_once(':')?;
        channel.parse::<u32>().ok()?;
        return Some(Form::Pwm(node));
    }
    if let Some(rest) = name.strip_prefix("gpio:") {
        let (label, line) = rest.rsplit_once(':')?;
        line.parse::<u32>().ok()?;
        return Some(Form::Gpio(label));
    }
    name.strip_prefix("udc:").map(Form::Udc)
}

/// Why the map does not have `name`, or `None` when it has it or the name is not of a checked form.
fn refusal(name: &str, map: &SocMap) -> Option<String> {
    match form(name)? {
        Form::I2c(number) => missing(map, Kind::I2c, "I2C bus", name, |c| {
            c.alias == Some(format!("i2c{number}"))
        }),
        Form::Spi(bus) => missing(map, Kind::Spi, "SPI bus", name, |c| {
            c.alias == Some(format!("spi{bus}"))
        }),
        Form::Watchdog(node) => missing(map, Kind::Watchdog, "watchdog node", node, |c| {
            c.path == node
        }),
        Form::Pwm(node) => missing(map, Kind::Pwm, "PWM node", node, |c| c.path == node),
        Form::Gpio(label) => {
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
        Form::Udc(udc) => missing(map, Kind::UsbPeripheral, "UDC", udc, |c| {
            c.sysfs.as_deref() == Some(udc)
        }),
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

/// The digits of `text` as a number, or `None` when it is empty or not all digits.
fn digits(text: &str) -> Option<u32> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

fn list(items: &[&str]) -> String {
    if items.is_empty() {
        "none".to_string()
    } else {
        items.join(", ")
    }
}
