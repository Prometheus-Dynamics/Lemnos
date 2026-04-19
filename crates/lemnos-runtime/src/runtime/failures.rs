use super::Runtime;
use crate::{
    DriverId, RuntimeError, RuntimeFailureCategory, RuntimeFailureOperation, RuntimeFailureRecord,
    RuntimeResult,
};
use lemnos_core::{DeviceId, TimestampMs};
use lemnos_driver_sdk::DriverError;
use std::time::{SystemTime, UNIX_EPOCH};

impl Runtime {
    pub(crate) fn complete_operation<T>(
        &mut self,
        device_id: DeviceId,
        operation: RuntimeFailureOperation,
        result: &RuntimeResult<T>,
    ) {
        match result {
            Ok(_) => {
                if self.failures.remove(&device_id).is_some() {
                    super::runtime_debug!(
                        device_id = ?device_id,
                        operation = ?operation,
                        "runtime failure cleared"
                    );
                }
            }
            Err(error) => {
                let category = classify_failure(error);
                let driver_id = driver_id_from_error(error);
                let message = error.to_string();
                let occurred_at = current_timestamp_ms();

                if let Some(failure) = self.failures.get_mut(&device_id) {
                    failure.record_repeat(operation, category, driver_id, message, occurred_at);
                    super::runtime_warn!(
                        device_id = ?device_id,
                        operation = ?operation,
                        category = ?category,
                        occurrence_count = failure.occurrence_count,
                        driver_id = ?failure.driver_id,
                        message = %failure.message,
                        "runtime operation failed"
                    );
                } else {
                    super::runtime_warn!(
                        device_id = ?device_id,
                        operation = ?operation,
                        category = ?category,
                        occurrence_count = 1_u64,
                        driver_id = ?driver_id,
                        message = %message,
                        "runtime operation failed"
                    );
                    self.failures.insert(
                        device_id.clone(),
                        RuntimeFailureRecord::new(
                            device_id,
                            operation,
                            category,
                            driver_id,
                            message,
                            occurred_at,
                        ),
                    );
                }
            }
        }
    }
}

fn classify_failure(error: &RuntimeError) -> RuntimeFailureCategory {
    match error {
        RuntimeError::NotRunning | RuntimeError::DeviceNotBound { .. } => {
            RuntimeFailureCategory::Runtime
        }
        RuntimeError::UnknownDevice { .. } => RuntimeFailureCategory::UnknownDevice,
        RuntimeError::InvalidRequest { .. } => RuntimeFailureCategory::InvalidRequest,
        RuntimeError::Discovery(_) | RuntimeError::Registry(_) => RuntimeFailureCategory::Registry,
        RuntimeError::Driver { .. } => RuntimeFailureCategory::Driver,
    }
}

fn driver_id_from_error(error: &RuntimeError) -> Option<DriverId> {
    match error {
        RuntimeError::Driver { source, .. } => Some(driver_id_from_driver_error(source)),
        _ => None,
    }
}

fn driver_id_from_driver_error(error: &DriverError) -> DriverId {
    match error {
        DriverError::MissingBackend { driver_id, .. }
        | DriverError::BindRejected { driver_id, .. }
        | DriverError::BindFailed { driver_id, .. }
        | DriverError::InvalidRequest { driver_id, .. }
        | DriverError::Transport { driver_id, .. }
        | DriverError::HostIo { driver_id, .. }
        | DriverError::InvariantViolation { driver_id, .. }
        | DriverError::UnsupportedAction { driver_id, .. }
        | DriverError::NotImplemented { driver_id, .. } => DriverId::from(driver_id),
    }
}

fn current_timestamp_ms() -> Option<TimestampMs> {
    let duration = SystemTime::now().duration_since(UNIX_EPOCH).ok()?;
    let millis = duration.as_millis();
    Some(TimestampMs::new(u64::try_from(millis).ok()?))
}
