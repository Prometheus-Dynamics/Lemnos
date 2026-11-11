/// One setting from a device's `config` table in the board file.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ConfigValue<'a> {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(&'a str),
}

/// A device's settings, lent to it while it binds.
#[derive(Debug, Clone, Copy, Default)]
pub struct Config<'a> {
    entries: &'a [(&'a str, ConfigValue<'a>)],
}

impl<'a> Config<'a> {
    pub const fn new(entries: &'a [(&'a str, ConfigValue<'a>)]) -> Self {
        Self { entries }
    }

    /// Every setting, in the board file's order.
    pub const fn entries(&self) -> &'a [(&'a str, ConfigValue<'a>)] {
        self.entries
    }

    pub fn get(&self, key: &str) -> Option<ConfigValue<'a>> {
        self.entries
            .iter()
            .find(|(name, _)| *name == key)
            .map(|(_, value)| *value)
    }

    pub fn int(&self, key: &str) -> Option<i64> {
        match self.get(key)? {
            ConfigValue::Int(value) => Some(value),
            _ => None,
        }
    }

    /// A float, or an integer read as one.
    pub fn float(&self, key: &str) -> Option<f64> {
        match self.get(key)? {
            ConfigValue::Float(value) => Some(value),
            ConfigValue::Int(value) => Some(value as f64),
            _ => None,
        }
    }

    pub fn bool(&self, key: &str) -> Option<bool> {
        match self.get(key)? {
            ConfigValue::Bool(value) => Some(value),
            _ => None,
        }
    }

    pub fn str(&self, key: &str) -> Option<&'a str> {
        match self.get(key)? {
            ConfigValue::Str(value) => Some(value),
            _ => None,
        }
    }
}
