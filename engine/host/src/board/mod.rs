//! The board file: which devices a board has and how they are wired. A parser
//! only. It knows no drivers and no kinds of device.

use std::collections::{BTreeMap, BTreeSet};

use lemnos_core::ConfigValue;
use serde::Deserialize;

pub const FORMAT: &str = "lemnos.board";
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum BoardError {
    #[error("{0}")]
    Parse(String),
    #[error("format is {0:?}, expected {FORMAT:?}")]
    Format(String),
    #[error("schema_version {0} is newer than {SCHEMA_VERSION}")]
    Version(u32),
    #[error("device id {0:?} is used twice")]
    Duplicate(String),
    #[error("device {0:?} uses itself")]
    SelfUse(String),
    #[error("device {0:?} config key {1:?}: values are numbers, booleans or strings")]
    Config(String, String),
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoardInfo {
    pub id: String,
}

/// One device.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceSpec {
    pub id: String,
    pub driver: String,
    /// The bus or block it sits on. What the text means is the backend's.
    #[serde(default)]
    pub bus: Option<String>,
    /// Need name to device id or bus name.
    #[serde(default)]
    pub uses: BTreeMap<String, String>,
    /// Another device this one is wired to. The host only hands it over.
    #[serde(default)]
    pub link: Option<String>,
    #[serde(default)]
    pub config: toml::Table,
    #[serde(default)]
    pub disabled: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoardFile {
    pub format: String,
    pub schema_version: u32,
    pub board: BoardInfo,
    #[serde(default)]
    pub devices: Vec<DeviceSpec>,
}

/// A config value the host owns.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum OwnedValue {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
}

impl OwnedValue {
    pub(crate) fn as_value(&self) -> ConfigValue<'_> {
        match self {
            Self::Int(v) => ConfigValue::Int(*v),
            Self::Float(v) => ConfigValue::Float(*v),
            Self::Bool(v) => ConfigValue::Bool(*v),
            Self::Str(v) => ConfigValue::Str(v),
        }
    }
}

impl DeviceSpec {
    pub(crate) fn owned_config(&self) -> Result<Vec<(String, OwnedValue)>, BoardError> {
        self.config
            .iter()
            .map(|(key, value)| {
                let owned = match value {
                    toml::Value::Integer(v) => OwnedValue::Int(*v),
                    toml::Value::Float(v) => OwnedValue::Float(*v),
                    toml::Value::Boolean(v) => OwnedValue::Bool(*v),
                    toml::Value::String(v) => OwnedValue::Str(v.clone()),
                    _ => return Err(BoardError::Config(self.id.clone(), key.clone())),
                };
                Ok((key.clone(), owned))
            })
            .collect()
    }
}

/// Parses and checks a board file. Unknown keys are refused by name.
pub fn parse(text: &str) -> Result<BoardFile, BoardError> {
    let board: BoardFile = toml::from_str(text).map_err(|e| BoardError::Parse(e.to_string()))?;
    if board.format != FORMAT {
        return Err(BoardError::Format(board.format));
    }
    if board.schema_version > SCHEMA_VERSION {
        return Err(BoardError::Version(board.schema_version));
    }
    let mut seen = BTreeSet::new();
    for device in &board.devices {
        if !seen.insert(device.id.as_str()) {
            return Err(BoardError::Duplicate(device.id.clone()));
        }
        if device.uses.values().any(|used| *used == device.id) {
            return Err(BoardError::SelfUse(device.id.clone()));
        }
        device.owned_config()?;
    }
    Ok(board)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEAD: &str = "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n";

    fn with(devices: &str) -> Result<BoardFile, BoardError> {
        parse(&format!("{HEAD}{devices}"))
    }

    #[test]
    fn a_full_device_parses() {
        let board = with(
            r#"
            [[devices]]
            id = "cam0-sensor"
            driver = "some-chip"
            bus = "i2c-cam0"
            link = "csi0"
            disabled = true
            uses = { supply = "rail" }
            config = { rate = 120, gain = 1.5, flip = true, mode = "fast" }
            "#,
        )
        .unwrap();
        let device = &board.devices[0];
        assert_eq!(device.bus.as_deref(), Some("i2c-cam0"));
        assert_eq!(device.link.as_deref(), Some("csi0"));
        assert!(device.disabled);
        assert_eq!(device.uses["supply"], "rail");
        let config = device.owned_config().unwrap();
        assert!(config.contains(&("rate".to_string(), OwnedValue::Int(120))));
        assert!(config.contains(&("mode".to_string(), OwnedValue::Str("fast".to_string()))));
    }

    #[test]
    fn no_devices_is_a_board() {
        assert!(with("").unwrap().devices.is_empty());
    }

    #[test]
    fn an_unknown_key_is_refused_by_name() {
        let error = with("[[devices]]\nid = \"a\"\ndriver = \"b\"\nspeeed = 1\n").unwrap_err();
        assert!(error.to_string().contains("speeed"), "{error}");
    }

    #[test]
    fn an_unknown_top_level_key_is_refused_by_name() {
        let error = parse(&format!("{HEAD}looks = 1\n")).unwrap_err();
        assert!(error.to_string().contains("looks"), "{error}");
    }

    #[test]
    fn the_wrong_format_is_refused() {
        let text = HEAD.replace("lemnos.board", "something.else");
        assert_eq!(
            parse(&text),
            Err(BoardError::Format("something.else".to_string()))
        );
    }

    #[test]
    fn a_newer_schema_is_refused() {
        let text = HEAD.replace("schema_version = 1", "schema_version = 2");
        assert_eq!(parse(&text), Err(BoardError::Version(2)));
    }

    #[test]
    fn a_duplicate_id_is_refused() {
        let text =
            "[[devices]]\nid = \"a\"\ndriver = \"x\"\n[[devices]]\nid = \"a\"\ndriver = \"y\"\n";
        assert_eq!(with(text), Err(BoardError::Duplicate("a".to_string())));
    }

    #[test]
    fn a_device_using_itself_is_refused() {
        let text = "[[devices]]\nid = \"a\"\ndriver = \"x\"\nuses = { b = \"a\" }\n";
        assert_eq!(with(text), Err(BoardError::SelfUse("a".to_string())));
    }

    #[test]
    fn a_nested_config_value_is_refused() {
        let text = "[[devices]]\nid = \"a\"\ndriver = \"x\"\nconfig = { list = [1] }\n";
        assert_eq!(
            with(text),
            Err(BoardError::Config("a".to_string(), "list".to_string()))
        );
    }

    #[test]
    fn missing_required_keys_are_named() {
        let error = with("[[devices]]\nid = \"a\"\n").unwrap_err();
        assert!(error.to_string().contains("driver"), "{error}");
    }
}
