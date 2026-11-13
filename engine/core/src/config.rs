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
    host_owned: bool,
    board: &'a str,
    host_version: &'a str,
}

impl<'a> Config<'a> {
    pub const fn new(entries: &'a [(&'a str, ConfigValue<'a>)]) -> Self {
        Self {
            entries,
            host_owned: false,
            board: "",
            host_version: "",
        }
    }

    /// The same settings, on the board the board file's `[board] id` names.
    #[must_use]
    pub const fn on_board(mut self, board: &'a str) -> Self {
        self.board = board;
        self
    }

    /// The board file's `[board] id`: which board this is, as the board file names it. Empty
    /// where nothing says (a driver package, or a test that sets none).
    pub const fn board(&self) -> &'a str {
        self.board
    }

    /// The same settings, with the daemon's own version: the `version.lemnos` identity fact.
    #[must_use]
    pub const fn on_host_version(mut self, version: &'a str) -> Self {
        self.host_version = version;
        self
    }

    /// The version of the daemon that runs this driver (its package version). Empty where the
    /// host was started without one (a test, or a driver package on its own).
    pub const fn host_version(&self) -> &'a str {
        self.host_version
    }

    /// The same settings, for a device the board file gives to the host (`owner = "host"`).
    #[must_use]
    pub const fn for_host_owned(mut self) -> Self {
        self.host_owned = true;
        self
    }

    /// Whether the operating system's own driver owns this device, so that Lemnos may read it
    /// and must not change it. What a driver takes through its needs is already read-only
    /// then. A driver that reaches hardware by itself (a file, a device node) looks here, and
    /// opens what it opens for reading only: see [`HostOwned`](crate::HostOwned).
    pub const fn host_owned(&self) -> bool {
        self.host_owned
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
