use crate::{
    InteractionManifest, ManifestError, ManifestMatch, ManifestResult, MatchRule,
    validation::validate_driver_id, version::DriverVersion,
};
use lemnos_core::{DeviceDescriptor, DeviceKind, InterfaceKind};
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DriverPriority {
    Fallback,
    Generic,
    Preferred,
    Exact,
}

impl DriverPriority {
    pub const fn base_score(self) -> u32 {
        match self {
            Self::Fallback => 25,
            Self::Generic => 100,
            Self::Preferred => 200,
            Self::Exact => 300,
        }
    }
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriverManifest {
    pub id: String,
    pub version: DriverVersion,
    pub summary: String,
    pub description: Option<String>,
    pub interfaces: Vec<InterfaceKind>,
    pub kinds: Vec<DeviceKind>,
    pub priority: DriverPriority,
    pub standard_interactions: Vec<InteractionManifest>,
    pub custom_interactions: Vec<InteractionManifest>,
    pub rules: Vec<MatchRule>,
    pub tags: Vec<String>,
}

impl DriverManifest {
    pub fn new(
        id: impl Into<String>,
        summary: impl Into<String>,
        interfaces: Vec<InterfaceKind>,
    ) -> Self {
        Self {
            id: id.into(),
            version: DriverVersion::default(),
            summary: summary.into(),
            description: None,
            interfaces,
            kinds: Vec::new(),
            priority: DriverPriority::Generic,
            standard_interactions: Vec::new(),
            custom_interactions: Vec::new(),
            rules: Vec::new(),
            tags: Vec::new(),
        }
    }

    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn with_version(mut self, version: DriverVersion) -> Self {
        self.version = version;
        self
    }

    pub fn with_priority(mut self, priority: DriverPriority) -> Self {
        self.priority = priority;
        self
    }

    pub fn with_kind(mut self, kind: DeviceKind) -> Self {
        self.kinds.push(kind);
        self
    }

    pub fn with_standard_interaction(
        mut self,
        id: impl Into<String>,
        summary: impl Into<String>,
    ) -> Self {
        self.standard_interactions
            .push(InteractionManifest::standard(id, summary));
        self
    }

    pub fn with_custom_interaction(
        mut self,
        id: impl Into<String>,
        summary: impl Into<String>,
    ) -> Self {
        self.custom_interactions
            .push(InteractionManifest::custom(id, summary));
        self
    }

    pub fn with_rule(mut self, rule: MatchRule) -> Self {
        self.rules.push(rule);
        self
    }

    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.push(tag.into());
        self
    }

    pub fn validate(&self) -> ManifestResult<()> {
        validate_driver_id(&self.id)?;
        if self.interfaces.is_empty() {
            return Err(ManifestError::MissingInterfaces {
                id: self.id.clone(),
            });
        }
        for interaction in self
            .standard_interactions
            .iter()
            .chain(self.custom_interactions.iter())
        {
            interaction.validate(&self.id)?;
        }
        for rule in &self.rules {
            rule.validate(&self.id)?;
        }
        Ok(())
    }

    #[cfg(feature = "serde")]
    pub fn to_json_pretty(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    #[cfg(feature = "serde")]
    pub fn from_json(value: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(value)
    }

    pub fn supports_interface(&self, interface: InterfaceKind) -> bool {
        self.interfaces.contains(&interface)
    }

    pub fn match_device(&self, device: &DeviceDescriptor) -> ManifestMatch {
        if !self.supports_interface(device.interface) {
            return ManifestMatch::unsupported(format!(
                "interface '{}' not supported by manifest '{}'",
                device.interface, self.id
            ));
        }

        let mut reasons = vec![format!("interface '{}' matched", device.interface)];
        let mut score = self.priority.base_score();

        if !self.kinds.is_empty() {
            if !self.kinds.contains(&device.kind) {
                return ManifestMatch::unsupported(format!(
                    "device kind '{}' not supported by manifest '{}'",
                    device.kind, self.id
                ));
            }
            score += 25;
            reasons.push(format!("device kind '{}' matched", device.kind));
        }

        if self.rules.is_empty() {
            return ManifestMatch {
                matched: true,
                priority: self.priority,
                score,
                reasons,
                matched_rule: None,
            };
        }

        let mut best_rule = None;
        let mut best_score = 0;
        for (index, rule) in self.rules.iter().enumerate() {
            if rule.matches(device) && (best_rule.is_none() || rule.score > best_score) {
                best_rule = Some(index);
                best_score = rule.score;
            }
        }

        match best_rule {
            Some(index) => {
                let rule = &self.rules[index];
                score += rule.score;
                reasons.push(
                    rule.description
                        .clone()
                        .unwrap_or_else(|| format!("match rule #{index} matched")),
                );
                ManifestMatch {
                    matched: true,
                    priority: self.priority,
                    score,
                    reasons,
                    matched_rule: Some(index),
                }
            }
            None => ManifestMatch::unsupported(format!(
                "no manifest rules matched device '{}'",
                device.id
            )),
        }
    }
}
