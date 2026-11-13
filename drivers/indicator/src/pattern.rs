//! The patterns a board gives each state, as text, and how each one draws a frame.
//!
//! A pattern is one of `off`, `solid`, `blink`, `chase` or `bar`, then its colour (`#rgb` or
//! `#rrggbb`), then its options: `period=MS` (blink and chase), `off=#rrggbb` (bar's unlit colour),
//! `brightness=N` (0 to 255, every kind). See the crate's documentation for what each one looks like.

use alloc::vec::Vec;

/// The shortest period a blink or chase may have, in milliseconds.
pub const MIN_PERIOD_MS: u32 = 100;

/// A colour, red, green and blue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rgb(pub [u8; 3]);

/// What a pattern does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Off,
    Solid,
    Blink { period_ms: u32 },
    Chase { period_ms: u32 },
    Bar,
}

/// One state's pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pattern {
    pub kind: Kind,
    /// The lit colour.
    pub on: Rgb,
    /// The unlit colour (bar only; the rest of a pattern is black).
    pub off: Rgb,
    /// Scales every colour, 0 to 255.
    pub brightness: u8,
}

/// Why a pattern text was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Refused(pub &'static str);

/// Parses `text`, with `default_brightness` where it sets none.
///
/// # Errors
///
/// When a word is unknown or repeated, a colour is not hex, a period is missing or under
/// [`MIN_PERIOD_MS`], or a pattern takes an option it doesn't have.
pub fn parse(text: &str, default_brightness: u8) -> Result<Pattern, Refused> {
    let mut words = text.split_whitespace();
    let kind_word = words.next().ok_or(Refused("the pattern is empty"))?;
    let kind_name = match kind_word {
        "off" => Kind::Off,
        "solid" => Kind::Solid,
        "blink" => Kind::Blink { period_ms: 0 },
        "chase" => Kind::Chase { period_ms: 0 },
        "bar" => Kind::Bar,
        _ => {
            return Err(Refused(
                "the pattern kind is not off, solid, blink, chase or bar",
            ));
        }
    };
    let mut on = None;
    let mut off = None;
    let mut period = None;
    let mut brightness = None;
    for word in words {
        if let Some(hex) = word.strip_prefix('#') {
            set_once(&mut on, parse_hex(hex)?, "the colour is given twice")?;
        } else if let Some(hex) = word.strip_prefix("off=#") {
            set_once(&mut off, parse_hex(hex)?, "off= is given twice")?;
        } else if let Some(ms) = word.strip_prefix("period=") {
            let ms: u32 = ms
                .parse()
                .map_err(|_| Refused("period is not a number of ms"))?;
            set_once(&mut period, ms, "period is given twice")?;
        } else if let Some(level) = word.strip_prefix("brightness=") {
            let level: u8 = level
                .parse()
                .map_err(|_| Refused("brightness is not 0 to 255"))?;
            set_once(&mut brightness, level, "brightness is given twice")?;
        } else {
            return Err(Refused(
                "an option is not colour, period=, off= or brightness=",
            ));
        }
    }
    let kind = match kind_name {
        Kind::Off => {
            if on.is_some() || off.is_some() || period.is_some() {
                return Err(Refused("off takes no colour or options but brightness="));
            }
            Kind::Off
        }
        Kind::Solid => {
            if off.is_some() || period.is_some() {
                return Err(Refused("solid takes a colour and brightness= only"));
            }
            Kind::Solid
        }
        Kind::Blink { .. } | Kind::Chase { .. } => {
            if off.is_some() {
                return Err(Refused("off= is for bar only"));
            }
            let period_ms = period.ok_or(Refused("blink and chase need period="))?;
            if period_ms < MIN_PERIOD_MS {
                return Err(Refused("period is under 100 ms"));
            }
            match kind_name {
                Kind::Blink { .. } => Kind::Blink { period_ms },
                _ => Kind::Chase { period_ms },
            }
        }
        Kind::Bar => {
            if period.is_some() {
                return Err(Refused("bar takes no period="));
            }
            Kind::Bar
        }
    };
    if kind != Kind::Off && on.is_none() {
        return Err(Refused("the pattern needs a colour"));
    }
    Ok(Pattern {
        kind,
        on: on.unwrap_or_default(),
        off: off.unwrap_or_default(),
        brightness: brightness.unwrap_or(default_brightness),
    })
}

fn set_once<T>(slot: &mut Option<T>, value: T, twice: &'static str) -> Result<(), Refused> {
    if slot.is_some() {
        return Err(Refused(twice));
    }
    *slot = Some(value);
    Ok(())
}

/// `rgb` or `rrggbb`, without the `#`.
fn parse_hex(hex: &str) -> Result<Rgb, Refused> {
    let bytes = hex.as_bytes();
    let nibble = |b: u8| -> Result<u8, Refused> {
        match b {
            b'0'..=b'9' => Ok(b - b'0'),
            b'a'..=b'f' => Ok(b - b'a' + 10),
            b'A'..=b'F' => Ok(b - b'A' + 10),
            _ => Err(Refused("a colour is not hex")),
        }
    };
    match bytes.len() {
        3 => {
            let r = nibble(bytes[0])?;
            let g = nibble(bytes[1])?;
            let b = nibble(bytes[2])?;
            Ok(Rgb([r * 17, g * 17, b * 17]))
        }
        6 => {
            let mut out = [0u8; 3];
            for (i, pair) in bytes.chunks(2).enumerate() {
                out[i] = nibble(pair[0])? << 4 | nibble(pair[1])?;
            }
            Ok(Rgb(out))
        }
        _ => Err(Refused("a colour is #rgb or #rrggbb")),
    }
}

/// The colour `c` at `brightness` (0 to 255), rounded.
fn scale(c: u8, brightness: u8) -> u8 {
    ((u32::from(c) * u32::from(brightness) + 127) / 255) as u8
}

fn scaled(colour: Rgb, brightness: u8) -> [u8; 4] {
    let [r, g, b] = colour.0;
    [
        scale(r, brightness),
        scale(g, brightness),
        scale(b, brightness),
        0,
    ]
}

/// Draws `pattern` on `count` LEDs into `frame` (4 bytes each: r, g, b, w), at `elapsed_ms` since
/// the state began. `progress` is per mille, or `None` for a state with none.
///
/// # Panics
///
/// When `frame` is not `count * 4` bytes.
pub fn draw(
    pattern: &Pattern,
    count: usize,
    progress: Option<u16>,
    elapsed_ms: u64,
    frame: &mut [u8],
) {
    assert_eq!(frame.len(), count * 4, "the frame is sized for the strip");
    let black = [0u8; 4];
    let on = scaled(pattern.on, pattern.brightness);
    let off = scaled(pattern.off, pattern.brightness);
    for (i, led) in frame.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let lit = match pattern.kind {
            Kind::Off => false,
            Kind::Solid => true,
            Kind::Blink { period_ms } => {
                let half = u64::from(period_ms / 2);
                (elapsed_ms % (2 * half)) < half
            }
            Kind::Chase { period_ms } => {
                let len = (count / 8).max(1);
                let pos = chase_pos(count, period_ms, elapsed_ms);
                (i + count - pos) % count < len
            }
            Kind::Bar => match progress {
                Some(p) => i < bar_lit(p, count),
                None => false,
            },
        };
        let colour = if lit {
            on
        } else if pattern.kind == Kind::Bar {
            off
        } else {
            black
        };
        led.copy_from_slice(&colour);
    }
}

/// The LED a chase's lit segment starts at: one LED per step, a revolution per period.
fn chase_pos(count: usize, period_ms: u32, elapsed_ms: u64) -> usize {
    if count == 0 {
        return 0;
    }
    ((elapsed_ms * count as u64) / u64::from(period_ms)) as usize % count
}

/// How many LEDs of a bar are lit at `progress` per mille, rounded to the nearest.
pub fn bar_lit(progress: u16, count: usize) -> usize {
    let p = u64::from(progress.min(1000));
    ((p * count as u64 + 500) / 1000) as usize
}

/// The first elapsed time (ms since the state began) after `elapsed_ms` at which `pattern` draws
/// something different, or `None` for a pattern that never changes.
pub fn next_change(pattern: &Pattern, count: usize, elapsed_ms: u64) -> Option<u64> {
    match pattern.kind {
        Kind::Off | Kind::Solid | Kind::Bar => None,
        Kind::Blink { period_ms } => {
            let half = u64::from(period_ms / 2);
            Some((elapsed_ms / half + 1) * half)
        }
        Kind::Chase { period_ms } => {
            if count == 0 {
                return None;
            }
            let period = u64::from(period_ms);
            let count = count as u64;
            // The step after this one begins at ceil((k + 1) * period / count).
            let step = (elapsed_ms * count) / period + 1;
            Some((step * period).div_ceil(count))
        }
    }
}

/// The patterns for every state, in [`lemnos_contract_indicator::STATES`]'s order.
pub fn defaults() -> Vec<&'static str> {
    alloc::vec![
        "off",
        "blink #0040ff period=800",
        "bar #00c040 off=#001000",
        "chase #ffc000 period=1200",
        "solid #00ff00",
        "blink #ff0000 period=400",
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_built_in_patterns_all_parse() {
        for text in defaults() {
            parse(text, 255).unwrap_or_else(|e| panic!("{text}: {e:?}"));
        }
    }

    #[test]
    fn colours_are_hex_in_three_or_six_digits() {
        assert_eq!(parse("solid #f80", 255).unwrap().on, Rgb([255, 136, 0]));
        assert_eq!(parse("solid #FF8000", 255).unwrap().on, Rgb([255, 128, 0]));
        assert!(parse("solid #ff80", 255).is_err());
        assert!(parse("solid ff8000", 255).is_err());
    }

    #[test]
    fn a_pattern_takes_only_its_own_options() {
        assert!(parse("off #fff", 255).is_err());
        assert!(parse("solid #fff period=400", 255).is_err());
        assert!(parse("solid #fff off=#000", 255).is_err());
        assert!(parse("bar #fff period=400", 255).is_err());
        assert!(parse("blink #fff", 255).is_err(), "blink needs its period");
        assert!(parse("blink #fff period=99", 255).is_err());
        assert!(parse("solid #fff #000", 255).is_err(), "two colours");
        assert!(parse("solid #fff brightness=256", 255).is_err());
        assert_eq!(parse("solid #fff brightness=7", 255).unwrap().brightness, 7);
        assert_eq!(parse("solid #fff", 9).unwrap().brightness, 9);
    }

    #[test]
    fn draw_scales_by_brightness_with_rounding() {
        let p = parse("solid #ff8000 brightness=128", 255).unwrap();
        let mut frame = [0u8; 8];
        draw(&p, 2, None, 0, &mut frame);
        assert_eq!(&frame[..4], &[128, 64, 0, 0]);
    }

    #[test]
    fn a_bar_rounds_its_lit_count_to_the_nearest_led() {
        assert_eq!(bar_lit(0, 8), 0);
        assert_eq!(bar_lit(500, 8), 4);
        assert_eq!(bar_lit(1000, 8), 8);
        assert_eq!(bar_lit(1000, 7), 7);
        assert_eq!(bar_lit(60, 7), 0, "0.42 of one LED rounds down");
        assert_eq!(bar_lit(70, 7), 0, "0.49 of one LED rounds down");
        assert_eq!(bar_lit(72, 7), 1, "0.504 of one LED rounds up");
    }

    #[test]
    fn next_change_of_a_chase_lands_on_its_step_boundaries() {
        let p = parse("chase #fff period=1000", 255).unwrap();
        // Eight LEDs, a step every 125 ms.
        assert_eq!(next_change(&p, 8, 0), Some(125));
        assert_eq!(next_change(&p, 8, 124), Some(125));
        assert_eq!(next_change(&p, 8, 125), Some(250));
        // Three LEDs: the boundaries fall at ceil(k * 1000 / 3).
        assert_eq!(next_change(&p, 3, 0), Some(334));
        assert_eq!(next_change(&p, 3, 334), Some(667));
    }
}
