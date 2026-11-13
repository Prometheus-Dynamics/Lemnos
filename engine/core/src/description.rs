use crate::MethodId;

/// A value's unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Unit {
    MetrePerSecondSquared,
    RadianPerSecond,
    Tesla,
    Volt,
    Ampere,
    Watt,
    DegreeCelsius,
    RevolutionPerMinute,
    Radian,
    Pascal,
    Hertz,
    Second,
    /// A count of sensor lines, such as an exposure or a frame length.
    Line,
    /// Dimensionless: ratios, steps, levels, modes.
    #[default]
    One,
}

impl Unit {
    pub const fn symbol(self) -> &'static str {
        match self {
            Self::MetrePerSecondSquared => "m/s²",
            Self::RadianPerSecond => "rad/s",
            Self::Tesla => "T",
            Self::Volt => "V",
            Self::Ampere => "A",
            Self::Watt => "W",
            Self::DegreeCelsius => "°C",
            Self::RevolutionPerMinute => "rpm",
            Self::Radian => "rad",
            Self::Pascal => "Pa",
            Self::Hertz => "Hz",
            Self::Second => "s",
            Self::Line => "line",
            Self::One => "",
        }
    }
}

/// Whether an item's unit is known. An empty unit string means two different things, so the
/// description says which: a count, a ratio or a switch is `Dimensionless`; a value whose unit
/// the driver did not say is `Unknown`; any other unit is `Known`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum UnitState {
    /// The item has a unit, its symbol.
    Known,
    /// The item has no unit: a count, a ratio, a switch, a text.
    Dimensionless,
    /// The driver did not say.
    #[default]
    Unknown,
}

impl UnitState {
    /// The number this state has on the wire. Fixed.
    pub const fn code(self) -> u8 {
        match self {
            Self::Known => 0,
            Self::Dimensionless => 1,
            Self::Unknown => 2,
        }
    }

    /// The state for a wire number, or `None` for one this build does not know.
    pub const fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Self::Known),
            1 => Some(Self::Dimensionless),
            2 => Some(Self::Unknown),
            _ => None,
        }
    }

    /// What an item with no state from its host means: an empty unit is unknown, anything else
    /// is known. This is how descriptions from before the state decode.
    pub const fn from_unit(symbol: &str) -> Self {
        if symbol.is_empty() {
            Self::Unknown
        } else {
            Self::Known
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ItemKind {
    /// Something to read.
    Value,
    /// Something to read and set.
    Control,
    /// Something to do.
    Action,
}

/// How a value is shown: what kind of reading it is. Numbered in the wire's description, so the
/// codes never change; see [`Reading::code`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Reading {
    /// A plain number, in the item's unit.
    Number,
    /// A settable number within its range (a control such as a duty cycle).
    Range,
    /// On or off.
    Switch,
    /// Text.
    Text,
    /// A colour array: one entry per pixel or channel.
    ColourArray,
    /// One axis of a three-axis vector. The three items share a `group`; each one's `field` is
    /// its axis (`x`, `y` or `z`).
    ThreeAxes,
}

impl Reading {
    /// The number this reading has on the wire. Fixed: a new reading takes the next number.
    pub const fn code(self) -> u8 {
        match self {
            Self::Number => 0,
            Self::Range => 1,
            Self::Switch => 2,
            Self::Text => 3,
            Self::ColourArray => 4,
            Self::ThreeAxes => 5,
        }
    }

    /// The reading for a wire number, or `None` for one this build does not know.
    pub const fn from_code(code: u8) -> Option<Self> {
        Some(match code {
            0 => Self::Number,
            1 => Self::Range,
            2 => Self::Switch,
            3 => Self::Text,
            4 => Self::ColourArray,
            5 => Self::ThreeAxes,
            _ => return None,
        })
    }
}

/// One thing a device offers, tied to the method that does it.
///
/// A control's `method` is the one that sets it; a value's is the one that reads it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Item {
    pub kind: ItemKind,
    pub name: &'static str,
    pub unit: Unit,
    /// Whether `unit` is known, dimensionless or not said (see [`UnitState`]).
    pub unit_state: UnitState,
    /// The value is `raw * 10^exponent`, in `unit`.
    pub exponent: i8,
    /// The lowest and highest raw values the item can take. `None` when the driver does not know.
    pub min: Option<i64>,
    pub max: Option<i64>,
    /// The smallest change the item can take, in raw units. `None` when not known.
    pub step: Option<i64>,
    /// A raw value below `warn_low`, or above `warn_high`, is a warning (an under-voltage, an
    /// over-temperature). `None` when there is no such threshold.
    pub warn_low: Option<i64>,
    pub warn_high: Option<i64>,
    /// How the value reads. `None` takes the reading the method declares, and failing that the
    /// one its shape suggests.
    pub reading: Option<Reading>,
    /// Items with the same group are one value: a vector, whose parts are the items' fields.
    pub group: Option<&'static str>,
    pub method: MethodId,
    /// Which field of the method's record, when it returns several.
    pub field: Option<&'static str>,
}

impl Item {
    pub const fn new(kind: ItemKind, name: &'static str, method: MethodId) -> Self {
        Self {
            kind,
            name,
            unit: Unit::One,
            unit_state: UnitState::Unknown,
            exponent: 0,
            min: None,
            max: None,
            step: None,
            warn_low: None,
            warn_high: None,
            reading: None,
            group: None,
            method,
            field: None,
        }
    }

    pub const fn value(name: &'static str, method: MethodId) -> Self {
        Self::new(ItemKind::Value, name, method)
    }

    pub const fn control(name: &'static str, method: MethodId) -> Self {
        Self::new(ItemKind::Control, name, method)
    }

    /// An action has no value, so no unit: it is dimensionless.
    pub const fn action(name: &'static str, method: MethodId) -> Self {
        let mut item = Self::new(ItemKind::Action, name, method);
        item.unit_state = UnitState::Dimensionless;
        item
    }

    /// Gives the item its unit and exponent. `Unit::One` is dimensionless; any other unit is known.
    pub const fn unit(mut self, unit: Unit, exponent: i8) -> Self {
        self.unit = unit;
        self.exponent = exponent;
        self.unit_state = if matches!(unit, Unit::One) {
            UnitState::Dimensionless
        } else {
            UnitState::Known
        };
        self
    }

    /// The item is a count, a ratio, a switch or a text: it has no unit, and says so.
    pub const fn dimensionless(mut self) -> Self {
        self.unit = Unit::One;
        self.exponent = 0;
        self.unit_state = UnitState::Dimensionless;
        self
    }

    pub const fn range(mut self, min: i64, max: i64) -> Self {
        self.min = Some(min);
        self.max = Some(max);
        self
    }

    /// A signed span: from `-full` to `full`, the span of a sensor axis. Used by drivers that
    /// measure both ways.
    pub const fn range_of_scale(mut self, full: i64, step: i64) -> Self {
        self.min = Some(-full);
        self.max = Some(full);
        self.step = Some(step);
        self
    }

    /// The smallest change the item can take, in raw units.
    pub const fn step(mut self, step: i64) -> Self {
        self.step = Some(step);
        self
    }

    /// Warns below `raw`.
    pub const fn warn_below(mut self, raw: i64) -> Self {
        self.warn_low = Some(raw);
        self
    }

    /// Warns above `raw`.
    pub const fn warn_above(mut self, raw: i64) -> Self {
        self.warn_high = Some(raw);
        self
    }

    pub const fn reading(mut self, reading: Reading) -> Self {
        self.reading = Some(reading);
        self
    }

    /// Puts the item in a vector: the items of one group are its parts.
    pub const fn group(mut self, group: &'static str) -> Self {
        self.group = Some(group);
        self
    }

    pub const fn field(mut self, field: &'static str) -> Self {
        self.field = Some(field);
        self
    }
}

/// The most items one device can describe.
pub const MAX_ITEMS: usize = 32;

/// The most bytes a device's location text carries (a sysfs path fits). A longer one is cut at a
/// character boundary.
pub const MAX_LOCATION: usize = 128;

/// What a device says about itself.
#[derive(Debug, Clone)]
pub struct Description {
    items: [Option<Item>; MAX_ITEMS],
    len: usize,
    dropped: usize,
    address: Option<u8>,
    location: [u8; MAX_LOCATION],
    location_len: u8,
}

impl Default for Description {
    fn default() -> Self {
        Self::new()
    }
}

impl Description {
    pub const fn new() -> Self {
        Self {
            items: [None; MAX_ITEMS],
            len: 0,
            dropped: 0,
            address: None,
            location: [0; MAX_LOCATION],
            location_len: 0,
        }
    }

    /// Says where the device answers on its bus: a 7-bit I2C address. The host renders it.
    /// Left unset when the device has none to name (one driver that drives several chips
    /// on one bus names none).
    pub fn set_address(&mut self, address: u8) -> &mut Self {
        self.address = Some(address);
        self
    }

    /// The address the driver named, if any.
    pub const fn address(&self) -> Option<u8> {
        self.address
    }

    /// Says where the device is, in words, for what a 7-bit address cannot say: a sysfs directory,
    /// a chip select, a port path, a UART name. The bus is the board's own word, and is not
    /// repeated here. Text past [`MAX_LOCATION`] bytes is cut at a character boundary.
    pub fn set_location(&mut self, text: &str) -> &mut Self {
        let mut end = text.len().min(MAX_LOCATION);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        self.location = [0; MAX_LOCATION];
        self.location[..end].copy_from_slice(&text.as_bytes()[..end]);
        self.location_len = end as u8;
        self
    }

    /// The location the driver named, or the empty string.
    pub fn location(&self) -> &str {
        core::str::from_utf8(&self.location[..usize::from(self.location_len)]).unwrap_or("")
    }

    /// Adds an item. Past [`MAX_ITEMS`] it is counted in
    /// [`dropped`](Self::dropped) rather than failing the device.
    pub fn add(&mut self, item: Item) -> &mut Self {
        match self.items.get_mut(self.len) {
            Some(slot) => {
                *slot = Some(item);
                self.len += 1;
            }
            None => self.dropped += 1,
        }
        self
    }

    pub fn items(&self) -> impl Iterator<Item = &Item> {
        self.items[..self.len].iter().flatten()
    }

    pub fn find(&self, name: &str) -> Option<&Item> {
        self.items().find(|item| item.name == name)
    }

    pub const fn dropped(&self) -> usize {
        self.dropped
    }
}
