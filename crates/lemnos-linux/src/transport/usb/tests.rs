use super::*;
use lemnos_core::{DeviceAddress, DeviceKind, UsbControlSetup, UsbRecipient, UsbRequestType};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

struct MockUsbTransport {
    control_response: Vec<u8>,
    bulk_response: Vec<u8>,
    interrupt_response: Vec<u8>,
    claimed_interfaces: Arc<Mutex<BTreeMap<u8, Option<u8>>>>,
    last_bulk_write: Vec<u8>,
    last_interrupt_write: Vec<u8>,
}

impl Default for MockUsbTransport {
    fn default() -> Self {
        Self {
            control_response: Vec::new(),
            bulk_response: Vec::new(),
            interrupt_response: Vec::new(),
            claimed_interfaces: Arc::new(Mutex::new(BTreeMap::new())),
            last_bulk_write: Vec::new(),
            last_interrupt_write: Vec::new(),
        }
    }
}

impl UsbTransport for MockUsbTransport {
    fn close(&mut self) -> BusResult<()> {
        self.claimed_interfaces
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
        Ok(())
    }

    fn control_transfer(&mut self, _transfer: &UsbControlTransfer) -> BusResult<Vec<u8>> {
        Ok(self.control_response.clone())
    }

    fn bulk_read_into(
        &mut self,
        _endpoint: u8,
        buffer: &mut [u8],
        _timeout_ms: Option<u32>,
    ) -> BusResult<usize> {
        let len = buffer.len().min(self.bulk_response.len());
        buffer[..len].copy_from_slice(&self.bulk_response[..len]);
        Ok(len)
    }

    fn bulk_read(
        &mut self,
        _endpoint: u8,
        _length: u32,
        _timeout_ms: Option<u32>,
    ) -> BusResult<Vec<u8>> {
        Ok(self.bulk_response.clone())
    }

    fn bulk_write(
        &mut self,
        _endpoint: u8,
        bytes: &[u8],
        _timeout_ms: Option<u32>,
    ) -> BusResult<()> {
        self.last_bulk_write = bytes.to_vec();
        Ok(())
    }

    fn interrupt_read_into(
        &mut self,
        _endpoint: u8,
        buffer: &mut [u8],
        _timeout_ms: Option<u32>,
    ) -> BusResult<usize> {
        let len = buffer.len().min(self.interrupt_response.len());
        buffer[..len].copy_from_slice(&self.interrupt_response[..len]);
        Ok(len)
    }

    fn interrupt_read(
        &mut self,
        _endpoint: u8,
        _length: u32,
        _timeout_ms: Option<u32>,
    ) -> BusResult<Vec<u8>> {
        Ok(self.interrupt_response.clone())
    }

    fn interrupt_write(&mut self, transfer: &UsbInterruptTransfer) -> BusResult<()> {
        self.last_interrupt_write = transfer.bytes.clone();
        Ok(())
    }

    fn claim_interface(
        &mut self,
        interface_number: u8,
        alternate_setting: Option<u8>,
    ) -> BusResult<()> {
        self.claimed_interfaces
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(interface_number, alternate_setting);
        Ok(())
    }

    fn release_interface(&mut self, interface_number: u8) -> BusResult<()> {
        self.claimed_interfaces
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&interface_number);
        Ok(())
    }
}

#[test]
fn usb_session_round_trips_through_transport() {
    let device = DeviceDescriptor::builder_for_kind("usb0-if0", DeviceKind::UsbInterface)
        .expect("builder")
        .address(DeviceAddress::UsbInterface {
            bus: 1,
            ports: vec![2],
            interface_number: 0,
            vendor_id: Some(0x1209),
            product_id: Some(0x0001),
        })
        .build()
        .expect("descriptor");
    let transport = MockUsbTransport {
        control_response: vec![0x10, 0x20],
        bulk_response: vec![0xAA, 0xBB],
        interrupt_response: vec![0x33],
        ..Default::default()
    };
    let mut session = LinuxUsbSession::with_transport(
        device,
        SessionAccess::ExclusiveController,
        Box::new(transport),
    );

    assert_eq!(
        session
            .control_transfer(&UsbControlTransfer {
                setup: UsbControlSetup {
                    direction: UsbDirection::In,
                    request_type: UsbRequestType::Vendor,
                    recipient: UsbRecipient::Interface,
                    request: 0x01,
                    value: 0,
                    index: 0,
                },
                data: vec![0; 2],
                timeout_ms: Some(50),
            })
            .expect("control transfer"),
        vec![0x10, 0x20]
    );
    assert_eq!(
        session.bulk_read(0x81, 2, Some(50)).expect("bulk read"),
        vec![0xAA, 0xBB]
    );
    let mut bulk_buffer = [0_u8; 2];
    assert_eq!(
        session
            .bulk_read_into(0x81, &mut bulk_buffer, Some(50))
            .expect("bulk read into"),
        2
    );
    assert_eq!(bulk_buffer, [0xAA, 0xBB]);
    session
        .bulk_write(0x01, &[0x55], Some(50))
        .expect("bulk write");
    assert_eq!(
        session
            .interrupt_read(0x82, 1, Some(50))
            .expect("interrupt read"),
        vec![0x33]
    );
    let mut interrupt_buffer = [0_u8; 1];
    assert_eq!(
        session
            .interrupt_read_into(0x82, &mut interrupt_buffer, Some(50))
            .expect("interrupt read into"),
        1
    );
    assert_eq!(interrupt_buffer, [0x33]);
    session
        .interrupt_write(&UsbInterruptTransfer {
            endpoint: 0x02,
            bytes: vec![0x44],
            timeout_ms: Some(50),
        })
        .expect("interrupt write");
    session
        .claim_interface(0, Some(1))
        .expect("claim interface");
    session.release_interface(0).expect("release interface");
}

#[test]
fn usb_open_error_classification_maps_access_busy_and_missing() {
    let device_id = lemnos_core::DeviceId::new("usb0-if0").expect("device id");

    assert!(matches!(
        libusb_transport::classify_open_error(&device_id, 1, &[2], rusb::Error::Access),
        BusError::PermissionDenied {
            operation: "open",
            ..
        }
    ));
    assert!(matches!(
        libusb_transport::classify_open_error(&device_id, 1, &[2], rusb::Error::Busy),
        BusError::AccessConflict { .. }
    ));
    assert!(matches!(
        libusb_transport::classify_open_error(&device_id, 1, &[2], rusb::Error::NoDevice),
        BusError::SessionUnavailable { .. }
    ));
    assert!(matches!(
        libusb_transport::classify_open_error(&device_id, 1, &[2], rusb::Error::NotFound),
        BusError::SessionUnavailable { .. }
    ));
}

#[test]
fn usb_session_rejects_operations_after_close_and_new_session_can_reopen() {
    let device = DeviceDescriptor::builder_for_kind("usb0-if0", DeviceKind::UsbInterface)
        .expect("builder")
        .address(DeviceAddress::UsbInterface {
            bus: 1,
            ports: vec![2],
            interface_number: 0,
            vendor_id: Some(0x1209),
            product_id: Some(0x0001),
        })
        .build()
        .expect("descriptor");
    let mut session = LinuxUsbSession::with_transport(
        device.clone(),
        SessionAccess::ExclusiveController,
        Box::new(MockUsbTransport::default()),
    );
    session.close().expect("close");

    assert!(matches!(
        session.bulk_read(0x81, 1, Some(10)),
        Err(BusError::SessionUnavailable { .. })
    ));
    assert!(matches!(
        session.bulk_write(0x01, &[0x55], Some(10)),
        Err(BusError::SessionUnavailable { .. })
    ));

    let reopened = LinuxUsbSession::with_transport(
        device,
        SessionAccess::ExclusiveController,
        Box::new(MockUsbTransport::default()),
    );
    assert_eq!(reopened.metadata().state, SessionState::Idle);
}

#[test]
fn usb_session_close_releases_claimed_interfaces() {
    let device = DeviceDescriptor::builder_for_kind("usb0-if0", DeviceKind::UsbInterface)
        .expect("builder")
        .address(DeviceAddress::UsbInterface {
            bus: 1,
            ports: vec![2],
            interface_number: 0,
            vendor_id: Some(0x1209),
            product_id: Some(0x0001),
        })
        .build()
        .expect("descriptor");
    let mut transport = MockUsbTransport::default();
    let claimed_interfaces = Arc::clone(&transport.claimed_interfaces);
    transport
        .claim_interface(0, Some(1))
        .expect("claim interface");
    transport.claim_interface(2, None).expect("claim interface");

    let mut session = LinuxUsbSession::with_transport(
        device,
        SessionAccess::ExclusiveController,
        Box::new(transport),
    );
    session.close().expect("close");

    assert_eq!(session.metadata().state, SessionState::Closed);
    assert!(
        claimed_interfaces
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_empty()
    );
}
