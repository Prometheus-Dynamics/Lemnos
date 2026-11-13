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
    lists: &'a [(&'a str, &'a [&'a str])],
    host_owned: bool,
    board: &'a str,
    host_version: &'a str,
}

impl<'a> Config<'a> {
    pub const fn new(entries: &'a [(&'a str, ConfigValue<'a>)]) -> Self {
        Self {
            entries,
            lists: &[],
            host_owned: false,
            board: "",
            host_version: "",
        }
    }

    /// The same settings, with the lists the board file gives this device: a key whose value
    /// is an array of strings (`health = ["device:x"]`). Only a driver built into the host sees
    /// them; a driver package gets its settings alone.
    #[must_use]
    pub const fn with_lists(mut self, lists: &'a [(&'a str, &'a [&'a str])]) -> Self {
        self.lists = lists;
        self
    }

    /// The array of strings the board file gives under `key`, if it does.
    pub fn list(&self, key: &str) -> Option<&'a [&'a str]> {
        self.lists
            .iter()
            .find(|(name, _)| *name == key)
            .map(|(_, items)| *items)
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

/// The kind of value a setting takes, as a board file writes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A whole number.
    Int,
    /// A number, with or without a fraction. A whole number is read as one.
    Float,
    Bool,
    Str,
    /// An array of strings (`health = ["device:x"]`), read with [`Config::list`].
    List,
}

/// One setting a driver reads: its key, the kind it takes, and whether the board must give it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Key {
    pub name: &'static str,
    pub kind: Kind,
    pub required: bool,
}

impl Key {
    /// A setting the board must give.
    pub const fn required(name: &'static str, kind: Kind) -> Self {
        Self {
            name,
            kind,
            required: true,
        }
    }

    /// A setting the board may leave out.
    pub const fn optional(name: &'static str, kind: Kind) -> Self {
        Self {
            name,
            kind,
            required: false,
        }
    }
}

/// A setting a driver refuses, and why. Named by key so the person who wrote the board file can
/// find it. A driver returns it from [`Driver::check_config`](crate::Driver::check_config) with
/// no hardware, so the refusal comes before any device is made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfigError<'a> {
    /// The setting's key. A key the driver does not read is the one the board file gave.
    pub key: &'a str,
    /// Why, in words that follow the key: "must be a whole number", "is required".
    pub reason: &'static str,
    /// The key the driver does read, when the one given looks like a misspelling of it.
    pub suggestion: Option<&'static str>,
}

impl<'a> ConfigError<'a> {
    /// A refusal of `key` for `reason`, with no suggestion.
    pub const fn new(key: &'a str, reason: &'static str) -> Self {
        Self {
            key,
            reason,
            suggestion: None,
        }
    }
}

impl core::fmt::Display for ConfigError<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "setting {:?} {}", self.key, self.reason)?;
        match self.suggestion {
            Some(name) => write!(f, " (did you mean {name:?}?)"),
            None => Ok(()),
        }
    }
}

/// The largest edit distance that still reads as a misspelling of a key this long.
fn near(given: &str, key: &str) -> bool {
    const MAX: usize = 48;
    let (a, b) = (given.as_bytes(), key.as_bytes());
    if a.len() > MAX || b.len() > MAX {
        return false;
    }
    // One row of the edit distance table, on the stack.
    let mut row = [0usize; MAX + 1];
    for (j, cell) in row.iter_mut().enumerate().take(b.len() + 1) {
        *cell = j;
    }
    for (i, &x) in a.iter().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, &y) in b.iter().enumerate() {
            let above = row[j + 1];
            let cost = usize::from(x != y);
            row[j + 1] = (diagonal + cost).min(above + 1).min(row[j] + 1);
            diagonal = above;
        }
    }
    let distance = row[b.len()];
    distance <= 2 && distance * 3 <= a.len().max(b.len())
}

impl<'a> Config<'a> {
    /// Checks the settings against the keys a driver reads: every given key must be one of them
    /// and take the kind it says, and every required key must be given. The first problem found
    /// is returned, in the board file's order; a key the driver does not read gets the nearest
    /// key it reads as a suggestion, when one is close.
    ///
    /// This checks names and kinds only. A range or a combination is the driver's own check.
    ///
    /// # Errors
    ///
    /// The first setting that is not one of `keys`, has the wrong kind, or a required key that is
    /// missing.
    pub fn check_keys(&self, keys: &[Key]) -> Result<(), ConfigError<'a>> {
        let find = |name: &str| keys.iter().find(|key| key.name == name);
        let unknown = |key: &'a str| ConfigError {
            key,
            reason: "is not a setting this driver reads",
            suggestion: keys.iter().find(|k| near(key, k.name)).map(|k| k.name),
        };
        for &(name, value) in self.entries {
            let Some(key) = find(name) else {
                return Err(unknown(name));
            };
            let ok = matches!(
                (key.kind, value),
                (Kind::Int, ConfigValue::Int(_))
                    | (Kind::Float, ConfigValue::Int(_) | ConfigValue::Float(_))
                    | (Kind::Bool, ConfigValue::Bool(_))
                    | (Kind::Str, ConfigValue::Str(_))
            );
            if !ok {
                return Err(ConfigError::new(name, kind_reason(key.kind)));
            }
        }
        for &(name, _) in self.lists {
            let Some(key) = find(name) else {
                return Err(unknown(name));
            };
            if key.kind != Kind::List {
                return Err(ConfigError::new(name, kind_reason(key.kind)));
            }
        }
        for key in keys {
            if key.required && self.get(key.name).is_none() && self.list(key.name).is_none() {
                return Err(ConfigError::new(key.name, "is required"));
            }
        }
        Ok(())
    }
}

/// Why a setting of `kind` is refused when it is given another kind.
const fn kind_reason(kind: Kind) -> &'static str {
    match kind {
        Kind::Int => "must be a whole number",
        Kind::Float => "must be a number",
        Kind::Bool => "must be true or false",
        Kind::Str => "must be text",
        Kind::List => "must be a list of text",
    }
}
