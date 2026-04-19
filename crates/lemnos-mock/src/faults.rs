use lemnos_bus::BusError;
use lemnos_core::DeviceId;
use std::collections::{BTreeMap, VecDeque};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MockFaultStep {
    Timeout {
        operation: &'static str,
    },
    Disconnect {
        operation: &'static str,
    },
    TransportFailure {
        operation: &'static str,
        reason: String,
    },
    Error {
        operation: String,
        error: BusError,
    },
}

impl MockFaultStep {
    pub(crate) fn into_entry(self, device_id: &DeviceId) -> (String, BusError) {
        match self {
            Self::Timeout { operation } => (
                operation.to_string(),
                BusError::Timeout {
                    device_id: device_id.clone(),
                    operation,
                },
            ),
            Self::Disconnect { operation } => (
                operation.to_string(),
                BusError::Disconnected {
                    device_id: device_id.clone(),
                },
            ),
            Self::TransportFailure { operation, reason } => (
                operation.to_string(),
                BusError::TransportFailure {
                    device_id: device_id.clone(),
                    operation,
                    reason,
                },
            ),
            Self::Error { operation, error } => (operation, error),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MockFaultScript {
    steps: Vec<MockFaultStep>,
}

impl MockFaultScript {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn timeout(mut self, operation: &'static str) -> Self {
        self.steps.push(MockFaultStep::Timeout { operation });
        self
    }

    pub fn disconnect(mut self, operation: &'static str) -> Self {
        self.steps.push(MockFaultStep::Disconnect { operation });
        self
    }

    pub fn transport_failure(mut self, operation: &'static str, reason: impl Into<String>) -> Self {
        self.steps.push(MockFaultStep::TransportFailure {
            operation,
            reason: reason.into(),
        });
        self
    }

    pub fn error(mut self, operation: impl Into<String>, error: BusError) -> Self {
        self.steps.push(MockFaultStep::Error {
            operation: operation.into(),
            error,
        });
        self
    }

    pub(crate) fn into_entries(self, device_id: &DeviceId) -> Vec<(String, BusError)> {
        self.steps
            .into_iter()
            .map(|step| step.into_entry(device_id))
            .collect()
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct MockFaultRegistry {
    faults: BTreeMap<DeviceId, BTreeMap<String, VecDeque<BusError>>>,
}

impl MockFaultRegistry {
    pub fn push(&mut self, device_id: DeviceId, operation: impl Into<String>, error: BusError) {
        self.faults
            .entry(device_id)
            .or_default()
            .entry(operation.into())
            .or_default()
            .push_back(error);
    }

    pub fn take(&mut self, device_id: &DeviceId, operation: &str) -> Option<BusError> {
        let operations = self.faults.get_mut(device_id)?;
        let queue = operations.get_mut(operation)?;
        let error = queue.pop_front();
        if queue.is_empty() {
            operations.remove(operation);
        }
        if operations.is_empty() {
            self.faults.remove(device_id);
        }
        error
    }

    pub fn clear_device(&mut self, device_id: &DeviceId) {
        self.faults.remove(device_id);
    }
}
