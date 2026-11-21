//! The resource names a board file gives to the Linux backend, and their grammar.
//!
//! This is the one table of forms. `lemnos-ctl validate` reads it to refuse a name of no form, and
//! the backend's tests check that its own parsers take every example here and refuse the malformed
//! ones, so validate and bind cannot drift apart. The grammar is fixed: a name of no form is wrong
//! on every board, with or without a SoC map. Which of the forms exist on a SoC is the map's
//! business, and is not decided here.
//!
//! A name is only a form. Whether the running kernel has the thing it names is found at bind.

/// One form of a resource name: what it is, how it is written and one example that parses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Form {
    /// The form's name, as a message gives it.
    pub name: &'static str,
    /// How the form is written.
    pub shape: &'static str,
    /// An example that parses.
    pub example: &'static str,
}

/// Every form, in the order a message lists them.
pub const FORMS: &[Form] = &[
    Form {
        name: "i2c",
        shape: "i2c-N",
        example: "i2c-1",
    },
    Form {
        name: "i2c-of",
        shape: "i2c:of=/DT/NODE",
        example: "i2c:of=/axi/pcie@1000120000/rp1/i2c@74000",
    },
    Form {
        name: "pio-i2c",
        shape: "pio-i2c:sda=N,scl=N[,hz=N]",
        example: "pio-i2c:sda=8,scl=7",
    },
    Form {
        name: "pio-out",
        shape: "pio-out:gpio=N",
        example: "pio-out:gpio=13",
    },
    Form {
        name: "spidev",
        shape: "spidevB.C",
        example: "spidev0.0",
    },
    Form {
        name: "gpio",
        shape: "gpio:LABEL:LINE",
        example: "gpio:pinctrl-rp1:17",
    },
    Form {
        name: "gpiochip",
        shape: "gpiochipN:LINE",
        example: "gpiochip0:17",
    },
    Form {
        name: "pwm",
        shape: "pwm:/DT/NODE:CHANNEL",
        example: "pwm:/axi/pcie@1000120000/rp1/pwm@9c000:1",
    },
    Form {
        name: "pwmchip",
        shape: "pwmchipN:CHANNEL",
        example: "pwmchip0:1",
    },
    Form {
        name: "watchdog",
        shape: "watchdog:/DT/NODE",
        example: "watchdog:/soc@107c000000/watchdog@7d200000",
    },
    Form {
        name: "watchdogN",
        shape: "watchdogN",
        example: "watchdog0",
    },
    Form {
        name: "tty",
        shape: "ttyNAME",
        example: "ttyAMA0",
    },
    Form {
        name: "usb-port",
        shape: "usb:BUS-PORT",
        example: "usb:1-1.4",
    },
    Form {
        name: "usb-id",
        shape: "usb:VVVV:PPPP",
        example: "usb:1a2b:3c4d",
    },
    Form {
        name: "usb-class",
        shape: "usb:class:CC[:SS[:PP]]",
        example: "usb:class:0e:01",
    },
    Form {
        name: "udc",
        shape: "udc:NAME",
        example: "udc:fe980000.usb",
    },
    Form {
        name: "dma",
        shape: "dma:BLOCK",
        example: "dma:90000000.isp",
    },
    Form {
        name: "regs",
        shape: "regs:BLOCK[/N]",
        example: "regs:blk/1",
    },
    Form {
        name: "irq",
        shape: "irq:BLOCK[/N]",
        example: "irq:blk",
    },
    Form {
        name: "clocks",
        shape: "clocks:BLOCK[/NAME,NAME]",
        example: "clocks:blk/core",
    },
    Form {
        name: "resets",
        shape: "resets:BLOCK/NAME,NAME",
        example: "resets:blk/rst",
    },
];

/// A name of a known form, with its parts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Name<'a> {
    /// `i2c-N`.
    I2c { number: u32 },
    /// `i2c:of=/DT/NODE`: the bus whose adapter's device-tree node is `node`.
    I2cOf { node: &'a str },
    /// `pio-i2c:sda=N,scl=N[,hz=N]`.
    PioI2c,
    /// `pio-out:gpio=N`.
    PioOut { gpio: u8 },
    /// `spidevB.C`.
    Spidev { bus: u32, chip: u32 },
    /// `gpio:LABEL:LINE`.
    GpioLabel { label: &'a str, line: u32 },
    /// `gpiochipN:LINE`.
    GpioChip { chip: u32, line: u32 },
    /// `pwm:/DT/NODE:CHANNEL`.
    PwmNode { node: &'a str, channel: u32 },
    /// `pwmchipN:CHANNEL`.
    PwmChip { chip: u32, channel: u32 },
    /// `watchdog:/DT/NODE`.
    WatchdogNode { node: &'a str },
    /// `watchdogN`.
    WatchdogNumber { number: u32 },
    /// `ttyNAME`.
    Tty { name: &'a str },
    /// `usb:BUS-PORT`.
    UsbPort,
    /// `usb:VVVV:PPPP`.
    UsbId,
    /// `usb:class:CC[:SS[:PP]]`.
    UsbClass,
    /// `udc:NAME`.
    Udc { name: &'a str },
    /// `dma:`, `regs:`, `irq:`, `clocks:` or `resets:` a block the kernel module drives.
    Block { kind: &'static str, block: &'a str },
}

/// Parses a name. An unknown prefix is refused with the forms known; a name of a known prefix that
/// is malformed is refused with what the form is.
///
/// # Errors
/// The refusal's text, for a message to print after the name.
pub fn parse(name: &str) -> Result<Name<'_>, String> {
    parse_form(name).ok_or_else(|| {
        let known: Vec<&str> = FORMS.iter().map(|form| form.shape).collect();
        format!(
            "not a resource name of a known form (the forms are: {})",
            known.join(", ")
        )
    })?
}

/// Whether `name` starts with the prefix of a known form, malformed or not. A name that does not is
/// not a resource name at all: a board's `uses` may name a device by its id instead.
pub fn names_a_form(name: &str) -> bool {
    parse_form(name).is_some()
}

/// `None` for a name of no known prefix; otherwise the parse, which may refuse a malformed name of
/// a known prefix with its reason.
fn parse_form(name: &str) -> Option<Result<Name<'_>, String>> {
    if let Some(number) = name.strip_prefix("i2c-") {
        return Some(digits(number, "i2c-N").map(|number| Name::I2c { number }));
    }
    if let Some(node) = name.strip_prefix("i2c:of=") {
        return Some(node_path(node, "i2c:of=/DT/NODE").map(|node| Name::I2cOf { node }));
    }
    if name.starts_with("pio-i2c:") {
        return Some(Ok(Name::PioI2c));
    }
    if let Some(rest) = name.strip_prefix("pio-out:") {
        return Some(
            match rest
                .strip_prefix("gpio=")
                .and_then(|n| n.parse::<u8>().ok())
            {
                Some(gpio) if u32::from(gpio) < 28 => Ok(Name::PioOut { gpio }),
                _ => Err("pio-out:gpio=N, with N a GPIO of 0 to 27".to_string()),
            },
        );
    }
    if let Some(rest) = name.strip_prefix("spidev") {
        return Some(match rest.split_once('.') {
            Some((bus, chip)) => match (digits(bus, "spidevB.C"), digits(chip, "spidevB.C")) {
                (Ok(bus), Ok(chip)) => Ok(Name::Spidev { bus, chip }),
                (Err(e), _) | (_, Err(e)) => Err(e),
            },
            None => Err("spidevB.C".to_string()),
        });
    }
    if let Some(rest) = name.strip_prefix("gpio:") {
        return Some(match rest.rsplit_once(':') {
            Some((label, line)) if !label.is_empty() && !label.contains('/') => line
                .parse()
                .map_err(|_| "gpio:LABEL:LINE".to_string())
                .map(|line| Name::GpioLabel { label, line }),
            _ => Err("gpio:LABEL:LINE".to_string()),
        });
    }
    if let Some(rest) = name.strip_prefix("gpiochip") {
        return Some(match rest.split_once(':') {
            Some((chip, line)) => match (
                digits(chip, "gpiochipN:LINE"),
                digits(line, "gpiochipN:LINE"),
            ) {
                (Ok(chip), Ok(line)) => Ok(Name::GpioChip { chip, line }),
                (Err(e), _) | (_, Err(e)) => Err(e),
            },
            None => Err("gpiochipN:LINE".to_string()),
        });
    }
    if let Some(rest) = name.strip_prefix("pwm:") {
        return Some(match rest.rsplit_once(':') {
            Some((node, channel)) => node_path(node, "pwm:/DT/NODE:CHANNEL").and_then(|node| {
                digits(channel, "pwm:/DT/NODE:CHANNEL")
                    .map(|channel| Name::PwmNode { node, channel })
            }),
            None => Err("pwm:/DT/NODE:CHANNEL".to_string()),
        });
    }
    if let Some(rest) = name.strip_prefix("pwmchip") {
        return Some(match rest.split_once(':') {
            Some((chip, channel)) => match (
                digits(chip, "pwmchipN:CHANNEL"),
                digits(channel, "pwmchipN:CHANNEL"),
            ) {
                (Ok(chip), Ok(channel)) => Ok(Name::PwmChip { chip, channel }),
                (Err(e), _) | (_, Err(e)) => Err(e),
            },
            None => Err("pwmchipN:CHANNEL".to_string()),
        });
    }
    if let Some(node) = name.strip_prefix("watchdog:") {
        return Some(node_path(node, "watchdog:/DT/NODE").map(|node| Name::WatchdogNode { node }));
    }
    if let Some(number) = name.strip_prefix("watchdog") {
        return Some(digits(number, "watchdogN").map(|number| Name::WatchdogNumber { number }));
    }
    if let Some(rest) = name.strip_prefix("tty") {
        return Some(if rest.is_empty() || rest.contains('/') {
            Err("ttyNAME".to_string())
        } else {
            Ok(Name::Tty { name: rest })
        });
    }
    if let Some(rest) = name.strip_prefix("usb:") {
        return Some(usb(rest));
    }
    if let Some(rest) = name.strip_prefix("udc:") {
        return Some(if rest.is_empty() || rest.contains('/') {
            Err("udc:NAME".to_string())
        } else {
            Ok(Name::Udc { name: rest })
        });
    }
    for kind in ["dma", "regs", "irq", "clocks", "resets"] {
        let Some(rest) = name.strip_prefix(kind).and_then(|r| r.strip_prefix(':')) else {
            continue;
        };
        return Some(block(kind, rest));
    }
    None
}

/// A device-tree path: absolute, with no `..` in it.
fn node_path<'a>(node: &'a str, shape: &str) -> Result<&'a str, String> {
    if node.starts_with('/') && !node.contains("..") && node.len() > 1 {
        Ok(node)
    } else {
        Err(format!("{shape}: the path is absolute, with no `..`"))
    }
}

/// A plain decimal number: digits only, nothing else.
fn digits(text: &str, shape: &str) -> Result<u32, String> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("{shape}: a number is decimal digits"));
    }
    text.parse()
        .map_err(|_| format!("{shape}: a number is decimal digits"))
}

/// `usb:` followed by a port, a vendor and product, or a class: the backend's own rules.
fn usb(rest: &str) -> Result<Name<'_>, String> {
    if let Some(codes) = rest.strip_prefix("class:") {
        let parts: Vec<&str> = codes.split(':').collect();
        let hex2 = |p: &str| p.len() == 2 && p.bytes().all(|b| b.is_ascii_hexdigit());
        return if (1..=3).contains(&parts.len()) && parts.iter().all(|p| hex2(p)) {
            Ok(Name::UsbClass)
        } else {
            Err("usb:class:CC[:SS[:PP]], two hex digits each".to_string())
        };
    }
    if let Some((vendor, product)) = rest.split_once(':') {
        let hex4 = |p: &str| p.len() == 4 && p.bytes().all(|b| b.is_ascii_hexdigit());
        return if hex4(vendor) && hex4(product) {
            Ok(Name::UsbId)
        } else {
            Err("usb:VVVV:PPPP, four hex digits each".to_string())
        };
    }
    let number = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
    match rest.split_once('-') {
        Some((bus, ports)) if number(bus) && ports.split('.').all(number) => Ok(Name::UsbPort),
        _ => Err("usb:BUS-PORT, such as usb:1-1.4".to_string()),
    }
}

/// `dma:BLOCK`, `regs:BLOCK[/N]`, `irq:BLOCK[/N]`, `clocks:BLOCK[/NAME,NAME]` or
/// `resets:BLOCK/NAME,NAME`, with the block a name with no `/` in it.
fn block<'a>(kind: &'static str, rest: &'a str) -> Result<Name<'a>, String> {
    let (block, tail) = match rest.split_once('/') {
        Some((block, tail)) => (block, Some(tail)),
        None => (rest, None),
    };
    let plain = !block.is_empty() && !block.contains(',');
    let shaped = match (kind, tail) {
        ("dma", None) => true,
        ("regs" | "irq", None) => true,
        ("regs" | "irq", Some(n)) => digits(n, kind).is_ok(),
        ("clocks", None) => true,
        ("clocks", Some(names)) => names.split(',').all(|n| !n.is_empty()),
        ("resets", Some(names)) => names.split(',').all(|n| !n.is_empty()),
        _ => false,
    };
    if plain && shaped {
        Ok(Name::Block { kind, block })
    } else {
        Err(format!("{kind}:BLOCK, as the block's name and its part"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_example_in_the_table_parses() {
        for form in FORMS {
            assert!(
                parse(form.example).is_ok(),
                "{}: {}",
                form.name,
                form.example
            );
        }
    }

    #[test]
    fn a_name_of_no_form_is_refused_and_the_forms_are_listed() {
        let reason = parse("bogus:thing").unwrap_err();
        assert!(reason.contains("i2c-N"), "{reason}");
        assert!(reason.contains("i2c:of=/DT/NODE"), "{reason}");
        assert!(parse("i2cc-0").is_err());
    }

    #[test]
    fn the_parts_of_each_form_are_read() {
        assert_eq!(parse("i2c-10"), Ok(Name::I2c { number: 10 }));
        assert_eq!(
            parse("i2c:of=/axi/i2c@74000"),
            Ok(Name::I2cOf {
                node: "/axi/i2c@74000"
            })
        );
        assert_eq!(parse("pio-out:gpio=13"), Ok(Name::PioOut { gpio: 13 }));
        assert_eq!(
            parse("gpiochip0:17"),
            Ok(Name::GpioChip { chip: 0, line: 17 })
        );
        assert_eq!(parse("usb:class:0e:01"), Ok(Name::UsbClass));
        assert_eq!(
            parse("regs:blk/1"),
            Ok(Name::Block {
                kind: "regs",
                block: "blk"
            })
        );
    }

    #[test]
    fn malformed_names_of_known_prefixes_are_refused_with_their_form() {
        for (name, shape) in [
            ("i2c:of=axi/i2c@74000", "i2c:of="),
            ("i2c:of=/axi/../x", "i2c:of="),
            ("i2c-", "i2c-N"),
            ("i2c-1a", "i2c-N"),
            ("gpio:pinctrl-rp1", "gpio:LABEL:LINE"),
            ("gpiochip0:x", "gpiochipN:LINE"),
            ("pwmchip0", "pwmchipN:CHANNEL"),
            ("pio-out:gpio=40", "pio-out:gpio=N"),
            ("usb:1a2b:3c4", "usb:VVVV:PPPP"),
            ("usb:class:e", "usb:class:CC"),
            ("usb:1-", "usb:BUS-PORT"),
            ("clocks:blk/", "clocks:BLOCK"),
            ("irq:blk/x", "irq:BLOCK"),
            ("resets:blk", "resets:BLOCK"),
            ("tty", "ttyNAME"),
        ] {
            let reason = parse(name).unwrap_err();
            assert!(reason.contains(shape), "{name}: {reason}");
        }
    }
}
