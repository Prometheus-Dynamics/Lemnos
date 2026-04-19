use crate::DriverId;
use lemnos_core::{DeviceId, TimestampMs};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeFailureOperation {
    Bind,
    Rebind,
    Request,
    RefreshState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeFailureCategory {
    Runtime,
    UnknownDevice,
    InvalidRequest,
    Registry,
    Driver,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeFailureRecord {
    pub device_id: DeviceId,
    pub operation: RuntimeFailureOperation,
    pub category: RuntimeFailureCategory,
    pub driver_id: Option<DriverId>,
    pub message: String,
    pub occurrence_count: u64,
    pub first_occurred_at: Option<TimestampMs>,
    pub last_occurred_at: Option<TimestampMs>,
}

impl RuntimeFailureRecord {
    pub fn new(
        device_id: DeviceId,
        operation: RuntimeFailureOperation,
        category: RuntimeFailureCategory,
        driver_id: Option<DriverId>,
        message: impl Into<String>,
        occurred_at: Option<TimestampMs>,
    ) -> Self {
        Self {
            device_id,
            operation,
            category,
            driver_id,
            message: message.into(),
            occurrence_count: 1,
            first_occurred_at: occurred_at,
            last_occurred_at: occurred_at,
        }
    }

    pub fn record_repeat(
        &mut self,
        operation: RuntimeFailureOperation,
        category: RuntimeFailureCategory,
        driver_id: Option<DriverId>,
        message: impl Into<String>,
        occurred_at: Option<TimestampMs>,
    ) {
        self.operation = operation;
        self.category = category;
        self.driver_id = driver_id;
        self.message = message.into();
        self.occurrence_count += 1;
        if self.first_occurred_at.is_none() {
            self.first_occurred_at = occurred_at;
        }
        self.last_occurred_at = occurred_at;
    }

    pub fn is_repeated(&self) -> bool {
        self.occurrence_count > 1
    }
}

impl fmt::Display for RuntimeFailureRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "device={} operation={:?} category={:?}",
            self.device_id, self.operation, self.category
        )?;
        if let Some(driver_id) = &self.driver_id {
            write!(f, " driver={driver_id}")?;
        }
        write!(f, " occurrences={}", self.occurrence_count)?;
        if let Some(first) = self.first_occurred_at {
            write!(f, " first_at={first:?}")?;
        }
        if let Some(last) = self.last_occurred_at {
            write!(f, " last_at={last:?}")?;
        }
        write!(f, " message={}", self.message)
    }
}

#[cfg(test)]
mod tests {
    use super::{RuntimeFailureCategory, RuntimeFailureOperation, RuntimeFailureRecord};
    use crate::DriverId;
    use lemnos_core::{DeviceId, TimestampMs};

    #[test]
    fn runtime_failure_record_display_surfaces_key_diagnostics() {
        let record = RuntimeFailureRecord::new(
            DeviceId::new("gpiochip0-line-4").expect("valid id"),
            RuntimeFailureOperation::Request,
            RuntimeFailureCategory::Driver,
            Some(DriverId::from("gpio-driver")),
            "timed out during gpio.read",
            Some(TimestampMs::new(42)),
        );

        let rendered = record.to_string();
        assert!(rendered.contains("device=gpiochip0-line-4"));
        assert!(rendered.contains("operation=Request"));
        assert!(rendered.contains("category=Driver"));
        assert!(rendered.contains("driver=gpio-driver"));
        assert!(rendered.contains("occurrences=1"));
        assert!(rendered.contains("first_at=TimestampMs(42)"));
        assert!(rendered.contains("last_at=TimestampMs(42)"));
        assert!(rendered.contains("message=timed out during gpio.read"));
    }

    #[test]
    fn runtime_failure_record_reports_repeat_status() {
        let mut record = RuntimeFailureRecord::new(
            DeviceId::new("gpiochip0-line-5").expect("valid id"),
            RuntimeFailureOperation::Bind,
            RuntimeFailureCategory::Driver,
            None,
            "initial bind failure",
            Some(TimestampMs::new(1)),
        );
        assert!(!record.is_repeated());

        record.record_repeat(
            RuntimeFailureOperation::Rebind,
            RuntimeFailureCategory::Driver,
            None,
            "rebind failure",
            Some(TimestampMs::new(2)),
        );
        assert!(record.is_repeated());
    }
}
