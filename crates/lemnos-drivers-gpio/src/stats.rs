use lemnos_core::OperationRecord;
use lemnos_driver_sdk::{gpio, record_operation, record_output_operation};

#[derive(Debug, Clone, Default)]
pub(crate) struct GpioStats {
    pub read_ops: u64,
    pub write_ops: u64,
    pub configure_ops: u64,
    pub last_operation: Option<OperationRecord>,
}

impl GpioStats {
    pub fn record_read(&mut self, summary: impl Into<String>, level: &str) {
        self.read_ops += 1;
        record_output_operation(
            &mut self.last_operation,
            gpio::READ_INTERACTION,
            summary,
            level.to_string(),
        );
    }

    pub fn record_write(&mut self, summary: impl Into<String>, level: &str) {
        self.write_ops += 1;
        record_output_operation(
            &mut self.last_operation,
            gpio::WRITE_INTERACTION,
            summary,
            level.to_string(),
        );
    }

    pub fn record_configure(&mut self, summary: impl Into<String>) {
        self.configure_ops += 1;
        record_operation(
            &mut self.last_operation,
            gpio::CONFIGURE_INTERACTION,
            summary,
        );
    }
}
