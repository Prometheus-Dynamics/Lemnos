use lemnos_core::OperationRecord;
use lemnos_driver_sdk::{
    record_bytes_read_operation, record_bytes_written_slice_operation, record_output_operation, usb,
};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default)]
pub(crate) struct UsbStats {
    pub control_ops: u64,
    pub bulk_read_ops: u64,
    pub bulk_write_ops: u64,
    pub interrupt_read_ops: u64,
    pub interrupt_write_ops: u64,
    pub claim_ops: u64,
    pub release_ops: u64,
    pub bytes_read: u64,
    pub bytes_written: u64,
    pub claimed_interfaces: BTreeMap<u8, Option<u8>>,
    pub last_operation: Option<OperationRecord>,
}

impl UsbStats {
    pub fn record_control_read(&mut self, bytes: &[u8], summary: impl Into<String>) {
        self.control_ops += 1;
        record_bytes_read_operation(
            &mut self.last_operation,
            &mut self.bytes_read,
            usb::CONTROL_TRANSFER_INTERACTION,
            summary,
            bytes,
        );
    }

    pub fn record_control_write(&mut self, bytes: &[u8], summary: impl Into<String>) {
        self.control_ops += 1;
        record_bytes_written_slice_operation(
            &mut self.last_operation,
            &mut self.bytes_written,
            usb::CONTROL_TRANSFER_INTERACTION,
            summary,
            bytes,
        );
    }

    pub fn record_bulk_read(&mut self, bytes: &[u8], summary: impl Into<String>) {
        self.bulk_read_ops += 1;
        record_bytes_read_operation(
            &mut self.last_operation,
            &mut self.bytes_read,
            usb::BULK_READ_INTERACTION,
            summary,
            bytes,
        );
    }

    pub fn record_bulk_write(&mut self, bytes: &[u8], summary: impl Into<String>) {
        self.bulk_write_ops += 1;
        record_bytes_written_slice_operation(
            &mut self.last_operation,
            &mut self.bytes_written,
            usb::BULK_WRITE_INTERACTION,
            summary,
            bytes,
        );
    }

    pub fn record_interrupt_read(&mut self, bytes: &[u8], summary: impl Into<String>) {
        self.interrupt_read_ops += 1;
        record_bytes_read_operation(
            &mut self.last_operation,
            &mut self.bytes_read,
            usb::INTERRUPT_READ_INTERACTION,
            summary,
            bytes,
        );
    }

    pub fn record_interrupt_write(&mut self, bytes: &[u8], summary: impl Into<String>) {
        self.interrupt_write_ops += 1;
        record_bytes_written_slice_operation(
            &mut self.last_operation,
            &mut self.bytes_written,
            usb::INTERRUPT_WRITE_INTERACTION,
            summary,
            bytes,
        );
    }

    pub fn record_claim(
        &mut self,
        interface_number: u8,
        alternate_setting: Option<u8>,
        summary: impl Into<String>,
    ) {
        self.claim_ops += 1;
        self.claimed_interfaces
            .insert(interface_number, alternate_setting);
        record_output_operation(
            &mut self.last_operation,
            usb::CLAIM_INTERFACE_INTERACTION,
            summary,
            u64::from(interface_number),
        );
    }

    pub fn record_release(&mut self, interface_number: u8, summary: impl Into<String>) {
        self.release_ops += 1;
        self.claimed_interfaces.remove(&interface_number);
        record_output_operation(
            &mut self.last_operation,
            usb::RELEASE_INTERFACE_INTERACTION,
            summary,
            u64::from(interface_number),
        );
    }
}
