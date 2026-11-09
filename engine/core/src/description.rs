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
            Self::One => "",
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

/// One thing a device offers, tied to the method that does it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Item {
    pub kind: ItemKind,
    pub name: &'static str,
    pub unit: Unit,
    /// The value is `raw * 10^exponent`, in `unit`.
    pub exponent: i8,
    pub min: Option<i64>,
    pub max: Option<i64>,
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
            exponent: 0,
            min: None,
            max: None,
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

    pub const fn action(name: &'static str, method: MethodId) -> Self {
        Self::new(ItemKind::Action, name, method)
    }

    pub const fn unit(mut self, unit: Unit, exponent: i8) -> Self {
        self.unit = unit;
        self.exponent = exponent;
        self
    }

    pub const fn range(mut self, min: i64, max: i64) -> Self {
        self.min = Some(min);
        self.max = Some(max);
        self
    }

    pub const fn field(mut self, field: &'static str) -> Self {
        self.field = Some(field);
        self
    }
}

/// The most items one device can describe.
pub const MAX_ITEMS: usize = 32;

/// What a device says about itself.
#[derive(Debug, Clone)]
pub struct Description {
    items: [Option<Item>; MAX_ITEMS],
    len: usize,
    dropped: usize,
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
        }
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
