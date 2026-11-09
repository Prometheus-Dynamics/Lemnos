//! The `pio-i2c:sda=N,scl=N[,hz=N]` board-file name, parsed strictly: each key
//! once, no empty parts, plain decimal numbers without signs or leading zeros,
//! distinct RP1 GPIOs.

/// The prefix that makes a board-file name a PIO I2C selector.
pub(crate) const PREFIX: &str = "pio-i2c:";
/// GPIOs the PIO block can address (`rp1_pio::GPIO_COUNT`).
const GPIO_COUNT: u32 = lemnos_linux_sys::rp1_pio::GPIO_COUNT;
/// Clock used when the name gives none.
pub(crate) const DEFAULT_HZ: u32 = 400_000;
/// Fastest clock accepted. Above it the bit time is too short for the program.
pub(crate) const MAX_HZ: u32 = 1_000_000;

const SYNTAX: &str = "expected sda=<gpio>,scl=<gpio>[,hz=<hertz>]";
const NUMBER: &str = "a number is decimal digits without a sign or leading zero";

/// The pins and clock of one PIO I2C bus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct Pins {
    /// The SDA GPIO.
    pub sda: u8,
    /// The SCL GPIO.
    pub scl: u8,
    /// The SCL frequency in hertz.
    pub hz: u32,
}

/// Whether a board-file name is a PIO I2C selector at all.
pub(crate) fn is_selector(need: &str) -> bool {
    need.starts_with(PREFIX)
}

/// Parses a whole board-file name. `None` when it is not a PIO I2C name.
pub(crate) fn parse(need: &str) -> Option<Result<Pins, &'static str>> {
    need.strip_prefix(PREFIX).map(parse_body)
}

/// Parses the part after the prefix.
pub(crate) fn parse_body(text: &str) -> Result<Pins, &'static str> {
    let mut sda = None;
    let mut scl = None;
    let mut hz = None;
    for part in text.split(',') {
        let (key, value) = part.split_once('=').ok_or(SYNTAX)?;
        let slot = match key {
            "sda" => &mut sda,
            "scl" => &mut scl,
            "hz" => &mut hz,
            _ => return Err("the keys are sda, scl and hz"),
        };
        if slot.is_some() {
            return Err("each key is given once");
        }
        *slot = Some(number(value)?);
    }
    let sda = sda.ok_or("sda is missing")?;
    let scl = scl.ok_or("scl is missing")?;
    if sda == scl {
        return Err("sda and scl must be different pins");
    }
    if sda >= GPIO_COUNT || scl >= GPIO_COUNT {
        return Err("a GPIO is 0 to 27");
    }
    let hz = hz.unwrap_or(DEFAULT_HZ);
    if hz == 0 || hz > MAX_HZ {
        return Err("hz is 1 to 1000000");
    }
    Ok(Pins {
        // Both were checked against `GPIO_COUNT` above.
        sda: sda as u8,
        scl: scl as u8,
        hz,
    })
}

/// A plain decimal number: digits only, no sign, no leading zero, no empty text.
fn number(text: &str) -> Result<u32, &'static str> {
    let digits = !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit());
    if !digits || (text.len() > 1 && text.starts_with('0')) {
        return Err(NUMBER);
    }
    text.parse().map_err(|_| NUMBER)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_selector_names_its_pins_and_clock() {
        assert_eq!(
            parse("pio-i2c:sda=8,scl=7"),
            Some(Ok(Pins {
                sda: 8,
                scl: 7,
                hz: DEFAULT_HZ
            }))
        );
        assert_eq!(
            parse("pio-i2c:scl=7,sda=8,hz=100000"),
            Some(Ok(Pins {
                sda: 8,
                scl: 7,
                hz: 100_000
            }))
        );
    }

    #[test]
    fn a_name_that_is_not_a_selector_is_not_parsed() {
        assert_eq!(parse("i2c-1"), None);
        assert_eq!(parse("pio-i2c"), None);
        assert!(!is_selector("pio-i2c"));
        assert!(is_selector("pio-i2c:"));
    }

    #[test]
    fn malformed_selectors_are_refused() {
        for bad in [
            "pio-i2c:",
            "pio-i2c:sda=8",
            "pio-i2c:scl=7",
            "pio-i2c:sda=8,scl=8",
            "pio-i2c:sda=8,scl=28",
            "pio-i2c:sda=28,scl=7",
            "pio-i2c:sda=8,scl=7,hz=0",
            "pio-i2c:sda=8,scl=7,hz=1000001",
            "pio-i2c:sda=8,scl=7,speed=1",
            "pio-i2c:sda=8,scl=7,",
            "pio-i2c:,sda=8,scl=7",
            "pio-i2c:sda=8,,scl=7",
            "pio-i2c:sda=8,sda=9,scl=7",
            "pio-i2c:sda=+8,scl=7",
            "pio-i2c:sda=-8,scl=7",
            "pio-i2c:sda=08,scl=7",
            "pio-i2c:sda=,scl=7",
            "pio-i2c:sda=8,scl=7x",
            "pio-i2c:sda=8 ,scl=7",
            "pio-i2c:sda=99999999999999999999,scl=7",
            "pio-i2c:sda8,scl=7",
        ] {
            assert!(
                matches!(parse(bad), Some(Err(_))),
                "{bad:?} should be refused"
            );
        }
    }

    #[test]
    fn the_largest_gpio_is_accepted() {
        assert_eq!(
            parse_body("sda=27,scl=0").map(|p| (p.sda, p.scl)),
            Ok((27, 0))
        );
    }
}
