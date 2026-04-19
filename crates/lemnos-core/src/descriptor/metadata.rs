use crate::{DeviceId, Value, ValueMap};
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceControlSurface {
    LinuxClass { root: String },
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceRelation {
    Parent,
    Controller,
    Bus,
    Transport,
    Interface,
    Channel,
    Dependency,
    Peer,
    Consumer,
    Provider,
    Custom(String),
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceLink {
    pub target: DeviceId,
    pub relation: DeviceRelation,
    pub attributes: ValueMap,
}

impl DeviceLink {
    pub fn new(target: DeviceId, relation: DeviceRelation) -> Self {
        Self {
            target,
            relation,
            attributes: ValueMap::new(),
        }
    }

    pub fn with_attribute(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.attributes.insert(key.into(), value.into());
        self
    }
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MatchHints {
    pub driver_hint: Option<String>,
    pub modalias: Option<String>,
    pub compatible: Vec<String>,
    pub vendor: Option<String>,
    pub model: Option<String>,
    pub revision: Option<String>,
    pub serial_number: Option<String>,
    pub hardware_ids: BTreeMap<String, String>,
}
