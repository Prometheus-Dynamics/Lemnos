use lemnos_core::{DeviceStateSnapshot, OperationRecord, OperationStatus, Value};

pub fn succeeded_operation(
    interaction: &'static str,
    summary: impl Into<String>,
) -> OperationRecord {
    OperationRecord::new(interaction, OperationStatus::Succeeded).with_summary(summary)
}

pub fn succeeded_operation_with_output(
    interaction: &'static str,
    summary: impl Into<String>,
    output: impl Into<Value>,
) -> OperationRecord {
    succeeded_operation(interaction, summary).with_output(output)
}

pub fn with_last_operation(
    state: DeviceStateSnapshot,
    last_operation: Option<&OperationRecord>,
) -> DeviceStateSnapshot {
    match last_operation {
        Some(last_operation) => state.with_last_operation(last_operation.clone()),
        None => state,
    }
}
