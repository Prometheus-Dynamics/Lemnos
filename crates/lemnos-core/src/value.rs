use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::string::ToString;
use alloc::vec::Vec;
use ordered_float::OrderedFloat;
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

pub type ValueMap = BTreeMap<String, Value>;

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ValueKind {
    Null,
    Bool,
    I64,
    U64,
    F64,
    String,
    Bytes,
    List,
    Map,
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Value {
    #[default]
    Null,
    Bool(bool),
    I64(i64),
    U64(u64),
    F64(OrderedFloat<f64>),
    String(String),
    Bytes(Vec<u8>),
    List(Vec<Value>),
    Map(ValueMap),
}

impl Value {
    pub const fn kind(&self) -> ValueKind {
        match self {
            Self::Null => ValueKind::Null,
            Self::Bool(_) => ValueKind::Bool,
            Self::I64(_) => ValueKind::I64,
            Self::U64(_) => ValueKind::U64,
            Self::F64(_) => ValueKind::F64,
            Self::String(_) => ValueKind::String,
            Self::Bytes(_) => ValueKind::Bytes,
            Self::List(_) => ValueKind::List,
            Self::Map(_) => ValueKind::Map,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(value) => Some(*value),
            _ => None,
        }
    }

    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Self::U64(value) => Some(*value),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Self::I64(value) => Some(*value),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::F64(value) => Some(value.0),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(value) => Some(value.as_str()),
            _ => None,
        }
    }

    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Bytes(value) => Some(value.as_slice()),
            _ => None,
        }
    }

    pub fn as_list(&self) -> Option<&[Value]> {
        match self {
            Self::List(values) => Some(values.as_slice()),
            _ => None,
        }
    }

    pub fn as_map(&self) -> Option<&ValueMap> {
        match self {
            Self::Map(values) => Some(values),
            _ => None,
        }
    }

    /// Renders a scalar value as a flat label string.
    ///
    /// Bytes render as lowercase `0x`-prefixed hex. `Null`, lists, and maps
    /// have no scalar form and return `None`; use [`Value::flatten_labels`]
    /// for nested values.
    pub fn to_label_string(&self) -> Option<String> {
        match self {
            Self::Null | Self::List(_) | Self::Map(_) => None,
            Self::Bool(value) => Some(value.to_string()),
            Self::I64(value) => Some(value.to_string()),
            Self::U64(value) => Some(value.to_string()),
            Self::F64(value) => Some(value.to_string()),
            Self::String(value) => Some(value.clone()),
            Self::Bytes(bytes) => {
                let mut out = String::with_capacity(2 + bytes.len() * 2);
                out.push_str("0x");
                for byte in bytes {
                    use core::fmt::Write as _;
                    let _ = write!(out, "{byte:02x}");
                }
                Some(out)
            }
        }
    }

    /// Flattens this value into `(key, label)` string pairs rooted at `key`.
    ///
    /// Scalars produce a single pair. Maps recurse as `key.child` and lists as
    /// `key.0`, `key.1`, and so on. `Null` values are skipped. This is the
    /// shape string-only label stores (resource labels, metrics tags) expect.
    pub fn flatten_labels(&self, key: impl Into<String>) -> Vec<(String, String)> {
        let mut out = Vec::new();
        self.flatten_labels_into(key.into(), &mut out);
        out
    }

    fn flatten_labels_into(&self, key: String, out: &mut Vec<(String, String)>) {
        match self {
            Self::Map(values) => {
                for (child, value) in values {
                    value.flatten_labels_into(format!("{key}.{child}"), out);
                }
            }
            Self::List(values) => {
                for (index, value) in values.iter().enumerate() {
                    value.flatten_labels_into(format!("{key}.{index}"), out);
                }
            }
            scalar => {
                if let Some(label) = scalar.to_label_string() {
                    out.push((key, label));
                }
            }
        }
    }
}

impl From<bool> for Value {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

impl From<i64> for Value {
    fn from(value: i64) -> Self {
        Self::I64(value)
    }
}

impl From<u64> for Value {
    fn from(value: u64) -> Self {
        Self::U64(value)
    }
}

impl From<f64> for Value {
    fn from(value: f64) -> Self {
        Self::F64(OrderedFloat(value))
    }
}

impl From<String> for Value {
    fn from(value: String) -> Self {
        Self::String(value)
    }
}

impl From<&str> for Value {
    fn from(value: &str) -> Self {
        Self::String(value.to_string())
    }
}

impl From<Vec<u8>> for Value {
    fn from(value: Vec<u8>) -> Self {
        Self::Bytes(value)
    }
}

impl From<Vec<Value>> for Value {
    fn from(value: Vec<Value>) -> Self {
        Self::List(value)
    }
}

impl From<ValueMap> for Value {
    fn from(value: ValueMap) -> Self {
        Self::Map(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn reports_value_kind() {
        assert_eq!(Value::from(true).kind(), ValueKind::Bool);
        assert_eq!(Value::from("gpio").kind(), ValueKind::String);
        assert_eq!(Value::from(1.25_f64).kind(), ValueKind::F64);
    }

    #[test]
    fn exposes_typed_accessors() {
        let value = Value::from(500_u64);
        assert_eq!(value.as_u64(), Some(500));
        assert_eq!(value.as_bool(), None);
    }

    #[test]
    fn renders_scalar_label_strings() {
        assert_eq!(Value::from(true).to_label_string().as_deref(), Some("true"));
        assert_eq!(Value::from(-3_i64).to_label_string().as_deref(), Some("-3"));
        assert_eq!(
            Value::from(1.5_f64).to_label_string().as_deref(),
            Some("1.5")
        );
        assert_eq!(
            Value::from(vec![0x0a_u8, 0xff])
                .to_label_string()
                .as_deref(),
            Some("0x0aff")
        );
        assert_eq!(Value::Null.to_label_string(), None);
        assert_eq!(Value::from(ValueMap::new()).to_label_string(), None);
    }

    #[test]
    fn flattens_nested_values_into_dotted_labels() {
        let mut inner = ValueMap::new();
        inner.insert("rpm".into(), Value::from(1200_u64));
        inner.insert("skipped".into(), Value::Null);
        let mut outer = ValueMap::new();
        outer.insert("fan".into(), Value::from(inner));
        outer.insert(
            "modes".into(),
            Value::from(vec![Value::from("auto"), Value::from("manual")]),
        );

        assert_eq!(
            Value::from(outer).flatten_labels("telemetry"),
            vec![
                ("telemetry.fan.rpm".to_string(), "1200".to_string()),
                ("telemetry.modes.0".to_string(), "auto".to_string()),
                ("telemetry.modes.1".to_string(), "manual".to_string()),
            ]
        );
        assert_eq!(
            Value::from(7_u64).flatten_labels("pwm"),
            vec![("pwm".to_string(), "7".to_string())]
        );
    }
}
