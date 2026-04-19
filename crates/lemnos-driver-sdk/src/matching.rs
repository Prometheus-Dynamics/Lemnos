use lemnos_driver_manifest::{DriverPriority, ManifestMatch};
use std::cmp::Ordering;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum DriverMatchLevel {
    #[default]
    Unsupported,
    Fallback,
    Generic,
    Preferred,
    Exact,
}

impl From<DriverPriority> for DriverMatchLevel {
    fn from(value: DriverPriority) -> Self {
        match value {
            DriverPriority::Fallback => Self::Fallback,
            DriverPriority::Generic => Self::Generic,
            DriverPriority::Preferred => Self::Preferred,
            DriverPriority::Exact => Self::Exact,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriverMatch {
    pub level: DriverMatchLevel,
    pub score: u32,
    pub reasons: Vec<String>,
    pub matched_rule: Option<usize>,
}

impl DriverMatch {
    pub fn unsupported(reason: impl Into<String>) -> Self {
        Self {
            level: DriverMatchLevel::Unsupported,
            score: 0,
            reasons: vec![reason.into()],
            matched_rule: None,
        }
    }

    pub fn is_supported(&self) -> bool {
        self.level != DriverMatchLevel::Unsupported
    }

    pub fn compare_rank(&self, other: &Self) -> Ordering {
        self.score
            .cmp(&other.score)
            .then(self.level.cmp(&other.level))
    }
}

impl From<ManifestMatch> for DriverMatch {
    fn from(value: ManifestMatch) -> Self {
        if !value.matched {
            return Self {
                level: DriverMatchLevel::Unsupported,
                score: 0,
                reasons: value.reasons,
                matched_rule: value.matched_rule,
            };
        }

        Self {
            level: value.priority.into(),
            score: value.score,
            reasons: value.reasons,
            matched_rule: value.matched_rule,
        }
    }
}
