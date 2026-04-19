use std::borrow::Borrow;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DriverId(String);

impl DriverId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl AsRef<str> for DriverId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl Borrow<str> for DriverId {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for DriverId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<String> for DriverId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&String> for DriverId {
    fn from(value: &String) -> Self {
        Self(value.clone())
    }
}

impl From<&str> for DriverId {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}
