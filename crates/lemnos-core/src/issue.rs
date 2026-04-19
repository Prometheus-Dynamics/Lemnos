use crate::{IssueCode, TimestampMs, ValueMap};
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum IssueSeverity {
    Info,
    Warning,
    Error,
    Critical,
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum IssueCategory {
    Discovery,
    Inventory,
    Binding,
    Transport,
    Capability,
    Configuration,
    Conflict,
    Permissions,
    Timeout,
    Protocol,
    Validation,
    Hotplug,
    Other,
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceIssue {
    pub code: IssueCode,
    pub category: IssueCategory,
    pub severity: IssueSeverity,
    pub message: String,
    pub first_seen_at: Option<TimestampMs>,
    pub last_seen_at: Option<TimestampMs>,
    pub attributes: ValueMap,
}

impl DeviceIssue {
    pub fn new(
        code: impl Into<String>,
        category: IssueCategory,
        severity: IssueSeverity,
        message: impl Into<String>,
    ) -> crate::CoreResult<Self> {
        Ok(Self {
            code: IssueCode::new(code)?,
            category,
            severity,
            message: message.into(),
            first_seen_at: None,
            last_seen_at: None,
            attributes: ValueMap::new(),
        })
    }

    pub fn with_first_seen_at(mut self, timestamp: TimestampMs) -> Self {
        self.first_seen_at = Some(timestamp);
        self
    }

    pub fn with_last_seen_at(mut self, timestamp: TimestampMs) -> Self {
        self.last_seen_at = Some(timestamp);
        self
    }

    pub fn with_attribute(
        mut self,
        key: impl Into<String>,
        value: impl Into<crate::Value>,
    ) -> Self {
        self.attributes.insert(key.into(), value.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_builder_collects_metadata() {
        let issue = DeviceIssue::new(
            "gpio.permissions",
            IssueCategory::Permissions,
            IssueSeverity::Error,
            "permission denied",
        )
        .expect("issue")
        .with_first_seen_at(TimestampMs::new(10))
        .with_attribute("path", "/dev/gpiochip0");

        assert_eq!(issue.code.as_str(), "gpio.permissions");
        assert_eq!(issue.first_seen_at, Some(TimestampMs::new(10)));
        assert_eq!(
            issue.attributes.get("path"),
            Some(&crate::Value::from("/dev/gpiochip0"))
        );
    }
}
