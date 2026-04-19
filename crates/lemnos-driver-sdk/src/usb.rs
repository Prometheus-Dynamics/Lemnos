use crate::DriverResult;
use crate::transport::transport_error;
use lemnos_bus::UsbSession;
use lemnos_core::{DeviceDescriptor, DeviceId, UsbControlTransfer, UsbInterruptTransfer};

pub const CONTROL_TRANSFER_INTERACTION: &str = "usb.control_transfer";
pub const BULK_READ_INTERACTION: &str = "usb.bulk_read";
pub const BULK_WRITE_INTERACTION: &str = "usb.bulk_write";
pub const INTERRUPT_READ_INTERACTION: &str = "usb.interrupt_read";
pub const INTERRUPT_WRITE_INTERACTION: &str = "usb.interrupt_write";
pub const CLAIM_INTERFACE_INTERACTION: &str = "usb.claim_interface";
pub const RELEASE_INTERFACE_INTERACTION: &str = "usb.release_interface";

pub struct UsbDeviceIo<'a> {
    driver_id: &'a str,
    device_id: DeviceId,
    session: &'a mut dyn UsbSession,
}

impl<'a> UsbDeviceIo<'a> {
    pub fn new(
        session: &'a mut dyn UsbSession,
        driver_id: &'a str,
        device: &DeviceDescriptor,
    ) -> Self {
        Self::with_device_id(session, driver_id, device.id.clone())
    }

    pub fn with_device_id(
        session: &'a mut dyn UsbSession,
        driver_id: &'a str,
        device_id: DeviceId,
    ) -> Self {
        Self {
            driver_id,
            device_id,
            session,
        }
    }

    pub fn control_transfer(&mut self, transfer: &UsbControlTransfer) -> DriverResult<Vec<u8>> {
        self.session
            .control_transfer(transfer)
            .map_err(|source| transport_error(self.driver_id, &self.device_id, source))
    }

    pub fn bulk_read(
        &mut self,
        endpoint: u8,
        length: u32,
        timeout_ms: Option<u32>,
    ) -> DriverResult<Vec<u8>> {
        self.session
            .bulk_read(endpoint, length, timeout_ms)
            .map_err(|source| transport_error(self.driver_id, &self.device_id, source))
    }

    pub fn bulk_read_into(
        &mut self,
        endpoint: u8,
        buffer: &mut [u8],
        timeout_ms: Option<u32>,
    ) -> DriverResult<usize> {
        self.session
            .bulk_read_into(endpoint, buffer, timeout_ms)
            .map_err(|source| transport_error(self.driver_id, &self.device_id, source))
    }

    pub fn bulk_write(
        &mut self,
        endpoint: u8,
        bytes: &[u8],
        timeout_ms: Option<u32>,
    ) -> DriverResult<()> {
        self.session
            .bulk_write(endpoint, bytes, timeout_ms)
            .map_err(|source| transport_error(self.driver_id, &self.device_id, source))
    }

    pub fn interrupt_read(
        &mut self,
        endpoint: u8,
        length: u32,
        timeout_ms: Option<u32>,
    ) -> DriverResult<Vec<u8>> {
        self.session
            .interrupt_read(endpoint, length, timeout_ms)
            .map_err(|source| transport_error(self.driver_id, &self.device_id, source))
    }

    pub fn interrupt_read_into(
        &mut self,
        endpoint: u8,
        buffer: &mut [u8],
        timeout_ms: Option<u32>,
    ) -> DriverResult<usize> {
        self.session
            .interrupt_read_into(endpoint, buffer, timeout_ms)
            .map_err(|source| transport_error(self.driver_id, &self.device_id, source))
    }

    pub fn interrupt_write(&mut self, transfer: &UsbInterruptTransfer) -> DriverResult<()> {
        self.session
            .interrupt_write(transfer)
            .map_err(|source| transport_error(self.driver_id, &self.device_id, source))
    }

    pub fn claim_interface(
        &mut self,
        interface_number: u8,
        alternate_setting: Option<u8>,
    ) -> DriverResult<()> {
        self.session
            .claim_interface(interface_number, alternate_setting)
            .map_err(|source| transport_error(self.driver_id, &self.device_id, source))
    }

    pub fn release_interface(&mut self, interface_number: u8) -> DriverResult<()> {
        self.session
            .release_interface(interface_number)
            .map_err(|source| transport_error(self.driver_id, &self.device_id, source))
    }
}
