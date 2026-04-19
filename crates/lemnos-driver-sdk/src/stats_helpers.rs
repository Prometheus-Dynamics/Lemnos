use lemnos_core::OperationRecord;

use crate::{succeeded_operation, succeeded_operation_with_output};

pub fn record_operation(
    last_operation: &mut Option<OperationRecord>,
    interaction: &'static str,
    summary: impl Into<String>,
) {
    *last_operation = Some(succeeded_operation(interaction, summary));
}

pub fn record_output_operation(
    last_operation: &mut Option<OperationRecord>,
    interaction: &'static str,
    summary: impl Into<String>,
    output: impl Into<lemnos_core::Value>,
) {
    *last_operation = Some(succeeded_operation_with_output(
        interaction,
        summary,
        output,
    ));
}

pub fn record_bytes_read_operation(
    last_operation: &mut Option<OperationRecord>,
    bytes_read: &mut u64,
    interaction: &'static str,
    summary: impl Into<String>,
    bytes: &[u8],
) {
    *bytes_read += bytes.len() as u64;
    record_output_operation(
        last_operation,
        interaction,
        summary,
        crate::bounded_bytes_output(bytes),
    );
}

pub fn record_bytes_written_count_operation(
    last_operation: &mut Option<OperationRecord>,
    bytes_written: &mut u64,
    interaction: &'static str,
    summary: impl Into<String>,
    bytes_written_count: usize,
) {
    *bytes_written += bytes_written_count as u64;
    record_output_operation(
        last_operation,
        interaction,
        summary,
        bytes_written_count as u64,
    );
}

pub fn record_bytes_written_slice_operation(
    last_operation: &mut Option<OperationRecord>,
    bytes_written: &mut u64,
    interaction: &'static str,
    summary: impl Into<String>,
    bytes: &[u8],
) {
    record_bytes_written_count_operation(
        last_operation,
        bytes_written,
        interaction,
        summary,
        bytes.len(),
    );
}
