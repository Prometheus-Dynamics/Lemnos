use lemnos_core::OperationRecord;
use lemnos_driver_sdk::{
    record_bytes_read_operation, record_bytes_written_slice_operation, record_operation, uart,
};

#[derive(Debug, Clone, Default)]
pub(crate) struct UartStats {
    pub read_ops: u64,
    pub write_ops: u64,
    pub flush_ops: u64,
    pub bytes_read: u64,
    pub bytes_written: u64,
    pub last_operation: Option<OperationRecord>,
}

impl UartStats {
    pub fn record_read(&mut self, bytes: &[u8], summary: impl Into<String>) {
        self.read_ops += 1;
        record_bytes_read_operation(
            &mut self.last_operation,
            &mut self.bytes_read,
            uart::READ_INTERACTION,
            summary,
            bytes,
        );
    }

    pub fn record_write(&mut self, bytes: &[u8], summary: impl Into<String>) {
        self.write_ops += 1;
        record_bytes_written_slice_operation(
            &mut self.last_operation,
            &mut self.bytes_written,
            uart::WRITE_INTERACTION,
            summary,
            bytes,
        );
    }

    pub fn record_flush(&mut self, summary: impl Into<String>) {
        self.flush_ops += 1;
        record_operation(&mut self.last_operation, uart::FLUSH_INTERACTION, summary);
    }
}
