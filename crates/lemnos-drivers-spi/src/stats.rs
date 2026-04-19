use lemnos_core::OperationRecord;
use lemnos_driver_sdk::{record_bytes_read_operation, record_bytes_written_slice_operation, spi};

#[derive(Debug, Clone, Default)]
pub(crate) struct SpiStats {
    pub transfer_ops: u64,
    pub write_ops: u64,
    pub bytes_read: u64,
    pub bytes_written: u64,
    pub last_operation: Option<OperationRecord>,
}

impl SpiStats {
    pub fn record_transfer(&mut self, written: &[u8], read: &[u8], summary: impl Into<String>) {
        self.transfer_ops += 1;
        self.bytes_written += written.len() as u64;
        record_bytes_read_operation(
            &mut self.last_operation,
            &mut self.bytes_read,
            spi::TRANSFER_INTERACTION,
            summary,
            read,
        );
    }

    pub fn record_write(&mut self, bytes: &[u8], summary: impl Into<String>) {
        self.write_ops += 1;
        record_bytes_written_slice_operation(
            &mut self.last_operation,
            &mut self.bytes_written,
            spi::WRITE_INTERACTION,
            summary,
            bytes,
        );
    }
}
