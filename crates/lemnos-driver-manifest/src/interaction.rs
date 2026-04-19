use crate::{ManifestError, ManifestResult};
use lemnos_core::{InteractionId, Value};
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "lowercase"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum InteractionKind {
    Standard,
    Custom,
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InteractionManifest {
    pub id: String,
    pub summary: String,
    pub kind: InteractionKind,
    pub attributes: BTreeMap<String, Value>,
}

impl InteractionManifest {
    pub fn standard(id: impl Into<String>, summary: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            summary: summary.into(),
            kind: InteractionKind::Standard,
            attributes: BTreeMap::new(),
        }
    }

    pub fn custom(id: impl Into<String>, summary: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            summary: summary.into(),
            kind: InteractionKind::Custom,
            attributes: BTreeMap::new(),
        }
    }

    pub fn with_attribute(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.attributes.insert(key.into(), value.into());
        self
    }

    pub(crate) fn validate(&self, driver_id: &str) -> ManifestResult<()> {
        match self.kind {
            InteractionKind::Standard => {
                InteractionId::new(self.id.clone()).map_err(|source| {
                    ManifestError::InvalidStandardInteraction {
                        id: driver_id.to_string(),
                        interaction: self.id.clone(),
                        source,
                    }
                })?;
            }
            InteractionKind::Custom => {
                InteractionId::new(self.id.clone()).map_err(|source| {
                    ManifestError::InvalidCustomInteraction {
                        id: driver_id.to_string(),
                        interaction: self.id.clone(),
                        source,
                    }
                })?;
            }
        }
        Ok(())
    }
}
