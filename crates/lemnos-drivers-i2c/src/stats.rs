use lemnos_core::OperationRecord;
use lemnos_driver_sdk::{
    i2c, record_bytes_read_operation, record_bytes_written_count_operation, record_output_operation,
};

#[derive(Debug, Clone, Default)]
pub(crate) struct I2cStats {
    pub read_ops: u64,
    pub write_ops: u64,
    pub write_read_ops: u64,
    pub transaction_ops: u64,
    pub bytes_read: u64,
    pub bytes_written: u64,
    pub last_operation: Option<OperationRecord>,
}

impl I2cStats {
    pub fn record_read(&mut self, bytes: &[u8], summary: impl Into<String>) {
        self.read_ops += 1;
        record_bytes_read_operation(
            &mut self.last_operation,
            &mut self.bytes_read,
            i2c::READ_INTERACTION,
            summary,
            bytes,
        );
    }

    pub fn record_write(&mut self, bytes_written: usize, summary: impl Into<String>) {
        self.write_ops += 1;
        record_bytes_written_count_operation(
            &mut self.last_operation,
            &mut self.bytes_written,
            i2c::WRITE_INTERACTION,
            summary,
            bytes_written,
        );
    }

    pub fn record_write_read(
        &mut self,
        bytes_written: usize,
        bytes_read: &[u8],
        summary: impl Into<String>,
    ) {
        self.write_read_ops += 1;
        self.bytes_written += bytes_written as u64;
        record_bytes_read_operation(
            &mut self.last_operation,
            &mut self.bytes_read,
            i2c::WRITE_READ_INTERACTION,
            summary,
            bytes_read,
        );
    }

    pub fn record_transaction(
        &mut self,
        operations: usize,
        bytes_written: usize,
        bytes_read: usize,
        summary: impl Into<String>,
    ) {
        self.transaction_ops += 1;
        self.bytes_written += bytes_written as u64;
        self.bytes_read += bytes_read as u64;
        record_output_operation(
            &mut self.last_operation,
            i2c::TRANSACTION_INTERACTION,
            summary,
            operations as u64,
        );
    }
}
