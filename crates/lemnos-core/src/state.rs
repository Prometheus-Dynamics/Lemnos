use crate::{DeviceId, DeviceIssue, TimestampMs, ValueMap};
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum DeviceHealth {
    #[default]
    Healthy,
    Degraded,
    Failed,
    Offline,
    Unknown,
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Availability {
    #[default]
    Present,
    Missing,
    Unknown,
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum DeviceLifecycleState {
    #[default]
    Discovered,
    Binding,
    Bound,
    Idle,
    Active,
    Busy,
    Suspended,
    Unbinding,
    Removed,
    Faulted,
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "kebab-case"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum OperationStatus {
    #[default]
    Succeeded,
    Rejected,
    Failed,
    TimedOut,
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationRecord {
    pub interaction: String,
    pub status: OperationStatus,
    pub observed_at: Option<TimestampMs>,
    pub summary: Option<String>,
    pub output: Option<crate::Value>,
}

impl OperationRecord {
    pub fn new(interaction: impl Into<String>, status: OperationStatus) -> Self {
        Self {
            interaction: interaction.into(),
            status,
            observed_at: None,
            summary: None,
            output: None,
        }
    }

    pub fn with_observed_at(mut self, timestamp: TimestampMs) -> Self {
        self.observed_at = Some(timestamp);
        self
    }

    pub fn with_summary(mut self, summary: impl Into<String>) -> Self {
        self.summary = Some(summary.into());
        self
    }

    pub fn with_output(mut self, output: impl Into<crate::Value>) -> Self {
        self.output = Some(output.into());
        self
    }
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceStateSnapshot {
    pub device_id: DeviceId,
    pub lifecycle: DeviceLifecycleState,
    pub health: DeviceHealth,
    pub availability: Availability,
    pub observed_at: Option<TimestampMs>,
    pub updated_at: Option<TimestampMs>,
    pub issues: Vec<DeviceIssue>,
    /// Driver-realized configuration values.
    ///
    /// This remains string-keyed so drivers can publish interface- or
    /// device-specific settings without needing a global schema update. Reusable
    /// drivers should prefer shared constants for stable keys where possible.
    pub realized_config: ValueMap,
    /// Driver-reported telemetry values.
    ///
    /// This also remains string-keyed for extensibility. Prefer shared
    /// constants for common fields and reserve ad hoc keys for integration- or
    /// driver-specific data.
    pub telemetry: ValueMap,
    pub last_operation: Option<OperationRecord>,
}

impl DeviceStateSnapshot {
    pub fn new(device_id: DeviceId) -> Self {
        Self {
            device_id,
            lifecycle: DeviceLifecycleState::Discovered,
            health: DeviceHealth::Healthy,
            availability: Availability::Present,
            observed_at: None,
            updated_at: None,
            issues: Vec::new(),
            realized_config: ValueMap::new(),
            telemetry: ValueMap::new(),
            last_operation: None,
        }
    }

    pub fn with_lifecycle(mut self, lifecycle: DeviceLifecycleState) -> Self {
        self.lifecycle = lifecycle;
        self
    }

    pub fn with_health(mut self, health: DeviceHealth) -> Self {
        self.health = health;
        self
    }

    pub fn with_issue(mut self, issue: DeviceIssue) -> Self {
        self.issues.push(issue);
        self
    }

    pub fn with_observed_at(mut self, timestamp: TimestampMs) -> Self {
        self.observed_at = Some(timestamp);
        self
    }

    pub fn with_updated_at(mut self, timestamp: TimestampMs) -> Self {
        self.updated_at = Some(timestamp);
        self
    }

    /// Adds a realized configuration value under a stable key.
    ///
    /// Prefer shared constants for reusable keys and use ad hoc names only for
    /// driver-specific metadata that does not merit a shared schema.
    pub fn with_config(mut self, key: impl Into<String>, value: impl Into<crate::Value>) -> Self {
        self.realized_config.insert(key.into(), value.into());
        self
    }

    /// Adds a telemetry value under a stable key.
    ///
    /// Prefer shared constants for reusable keys and keep custom telemetry keys
    /// scoped to the driver or integration that owns them.
    pub fn with_telemetry(
        mut self,
        key: impl Into<String>,
        value: impl Into<crate::Value>,
    ) -> Self {
        self.telemetry.insert(key.into(), value.into());
        self
    }

    pub fn with_last_operation(mut self, record: OperationRecord) -> Self {
        self.last_operation = Some(record);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DeviceId, IssueCategory, IssueSeverity};

    #[test]
    fn state_snapshot_collects_issue_and_operation_data() {
        let issue = DeviceIssue::new(
            "i2c.timeout",
            IssueCategory::Timeout,
            IssueSeverity::Warning,
            "read timed out",
        )
        .expect("issue");
        let operation = OperationRecord::new("i2c.read", OperationStatus::TimedOut)
            .with_summary("read of 16 bytes timed out");

        let state = DeviceStateSnapshot::new(DeviceId::new("i2c.dev0").expect("device id"))
            .with_issue(issue)
            .with_last_operation(operation)
            .with_config("bus", 1_u64)
            .with_telemetry("retries", 3_u64);

        assert_eq!(state.issues.len(), 1);
        assert_eq!(
            state.realized_config.get("bus"),
            Some(&crate::Value::from(1_u64))
        );
        assert_eq!(
            state.telemetry.get("retries"),
            Some(&crate::Value::from(3_u64))
        );
    }
}
