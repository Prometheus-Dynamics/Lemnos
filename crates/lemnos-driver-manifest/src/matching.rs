use crate::{ManifestError, ManifestResult};
use lemnos_core::{CapabilityId, DeviceDescriptor, DeviceKind, InterfaceKind, Value};
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MatchCondition {
    Interface(InterfaceKind),
    Kind(DeviceKind),
    Vendor(String),
    Model(String),
    Revision(String),
    Modalias(String),
    Compatible(String),
    Capability(CapabilityId),
    LabelEq { key: String, value: String },
    PropertyEq { key: String, value: Value },
    HardwareIdEq { key: String, value: String },
}

impl MatchCondition {
    pub fn matches(&self, device: &DeviceDescriptor) -> bool {
        match self {
            Self::Interface(interface) => device.interface == *interface,
            Self::Kind(kind) => device.kind == *kind,
            Self::Vendor(vendor) => device.match_hints.vendor.as_deref() == Some(vendor.as_str()),
            Self::Model(model) => device.match_hints.model.as_deref() == Some(model.as_str()),
            Self::Revision(revision) => {
                device.match_hints.revision.as_deref() == Some(revision.as_str())
            }
            Self::Modalias(modalias) => {
                device.match_hints.modalias.as_deref() == Some(modalias.as_str())
            }
            Self::Compatible(compatible) => device
                .match_hints
                .compatible
                .iter()
                .any(|value| value == compatible),
            Self::Capability(capability) => device
                .capabilities
                .iter()
                .any(|value| &value.id == capability),
            Self::LabelEq { key, value } => device.labels.get(key) == Some(value),
            Self::PropertyEq { key, value } => device.properties.get(key) == Some(value),
            Self::HardwareIdEq { key, value } => {
                device.match_hints.hardware_ids.get(key) == Some(value)
            }
        }
    }

    pub(crate) fn validate(&self, driver_id: &str) -> ManifestResult<()> {
        if let Self::Capability(capability) = self {
            CapabilityId::new(capability.as_str()).map_err(|source| {
                ManifestError::InvalidCapability {
                    id: driver_id.to_string(),
                    capability: capability.to_string(),
                    source,
                }
            })?;
        }
        Ok(())
    }
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MatchRule {
    pub description: Option<String>,
    pub score: u32,
    pub all_of: Vec<MatchCondition>,
    pub any_of: Vec<MatchCondition>,
    pub none_of: Vec<MatchCondition>,
}

impl MatchRule {
    pub fn new(score: u32) -> Self {
        Self {
            description: None,
            score,
            all_of: Vec::new(),
            any_of: Vec::new(),
            none_of: Vec::new(),
        }
    }

    pub fn described(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn require(mut self, condition: MatchCondition) -> Self {
        self.all_of.push(condition);
        self
    }

    pub fn require_any(mut self, condition: MatchCondition) -> Self {
        self.any_of.push(condition);
        self
    }

    pub fn exclude(mut self, condition: MatchCondition) -> Self {
        self.none_of.push(condition);
        self
    }

    pub(crate) fn matches(&self, device: &DeviceDescriptor) -> bool {
        let all_match = self
            .all_of
            .iter()
            .all(|condition| condition.matches(device));
        let any_match = self.any_of.is_empty()
            || self
                .any_of
                .iter()
                .any(|condition| condition.matches(device));
        let none_match = self
            .none_of
            .iter()
            .all(|condition| !condition.matches(device));

        all_match && any_match && none_match
    }

    pub(crate) fn validate(&self, driver_id: &str) -> ManifestResult<()> {
        for condition in self
            .all_of
            .iter()
            .chain(self.any_of.iter())
            .chain(self.none_of.iter())
        {
            condition.validate(driver_id)?;
        }
        Ok(())
    }
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestMatch {
    pub matched: bool,
    pub priority: crate::DriverPriority,
    pub score: u32,
    pub reasons: Vec<String>,
    pub matched_rule: Option<usize>,
}

impl ManifestMatch {
    pub fn unsupported(reason: impl Into<String>) -> Self {
        Self {
            matched: false,
            priority: crate::DriverPriority::Fallback,
            score: 0,
            reasons: vec![reason.into()],
            matched_rule: None,
        }
    }
}
