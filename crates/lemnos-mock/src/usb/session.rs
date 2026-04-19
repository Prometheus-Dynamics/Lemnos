use super::device::{MockUsbControlKey, MockUsbDeviceState};
use crate::hardware::{MOCK_BACKEND_NAME, MockHardwareState, take_injected_error};
use lemnos_bus::{
    BusError, BusResult, BusSession, SessionAccess, SessionMetadata, SessionState, UsbSession,
};
use lemnos_core::{
    DeviceDescriptor, InterfaceKind, UsbControlTransfer, UsbDirection, UsbInterruptTransfer,
};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};

pub(crate) struct MockUsbSession {
    state: Arc<Mutex<MockHardwareState>>,
    device: DeviceDescriptor,
    owner_id: lemnos_core::DeviceId,
    metadata: SessionMetadata,
}

impl MockUsbSession {
    pub(crate) fn new(
        state: Arc<Mutex<MockHardwareState>>,
        device: DeviceDescriptor,
        owner_id: lemnos_core::DeviceId,
        access: SessionAccess,
    ) -> Self {
        Self {
            state,
            device,
            owner_id,
            metadata: SessionMetadata::new(MOCK_BACKEND_NAME, access)
                .with_state(SessionState::Idle),
        }
    }

    fn invalid_request(&self, operation: &'static str, reason: impl Into<String>) -> BusError {
        BusError::InvalidRequest {
            device_id: self.device.id.clone(),
            operation,
            reason: reason.into(),
        }
    }

    fn permission_denied(&self, operation: &'static str, reason: impl Into<String>) -> BusError {
        BusError::PermissionDenied {
            device_id: self.device.id.clone(),
            operation,
            reason: reason.into(),
        }
    }

    fn device_state_mut(&self) -> BusResult<MutexGuard<'_, MockHardwareState>> {
        let guard = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !guard.usb_devices.contains_key(&self.owner_id) {
            return Err(BusError::Disconnected {
                device_id: self.device.id.clone(),
            });
        }
        Ok(guard)
    }

    fn run_call<T>(&mut self, call: impl FnOnce(&mut Self) -> BusResult<T>) -> BusResult<T> {
        self.metadata.begin_call();
        let result = call(self);
        self.metadata.finish_call(&result);
        result
    }

    fn ensure_writable(&self, operation: &'static str, reason: impl Into<String>) -> BusResult<()> {
        if self.metadata.access.can_write() {
            Ok(())
        } else {
            Err(self.permission_denied(operation, reason))
        }
    }

    fn with_device_mut<T>(
        &mut self,
        operation: &'static str,
        call: impl FnOnce(&DeviceDescriptor, &mut MockUsbDeviceState) -> BusResult<T>,
    ) -> BusResult<T> {
        self.run_call(|session| {
            take_injected_error(&session.state, &session.device.id, operation)?;
            let mut state = session.device_state_mut()?;
            let device = state
                .usb_devices
                .get_mut(&session.owner_id)
                .expect("device existence checked before mutation");
            call(&session.device, device)
        })
    }
}

impl BusSession for MockUsbSession {
    fn interface(&self) -> InterfaceKind {
        InterfaceKind::Usb
    }

    fn device(&self) -> &DeviceDescriptor {
        &self.device
    }

    fn metadata(&self) -> &SessionMetadata {
        &self.metadata
    }

    fn close(&mut self) -> BusResult<()> {
        self.metadata.mark_closed();
        Ok(())
    }
}

impl UsbSession for MockUsbSession {
    fn control_transfer(&mut self, transfer: &UsbControlTransfer) -> BusResult<Vec<u8>> {
        if transfer.setup.direction == UsbDirection::Out {
            self.ensure_writable(
                "usb.control_transfer",
                "session access is read-only for outbound control transfers",
            )?;
        }

        self.with_device_mut("usb.control_transfer", |_device, device| {
            let key = MockUsbControlKey::from(transfer);
            match transfer.setup.direction {
                UsbDirection::In => Ok(device
                    .control_responses
                    .get(&key)
                    .cloned()
                    .unwrap_or_default()),
                UsbDirection::Out => {
                    device.last_control_out = Some(transfer.clone());
                    Ok(device
                        .control_responses
                        .get(&key)
                        .cloned()
                        .unwrap_or_default())
                }
            }
        })
    }

    fn bulk_read(
        &mut self,
        endpoint: u8,
        length: u32,
        _timeout_ms: Option<u32>,
    ) -> BusResult<Vec<u8>> {
        if endpoint == 0 {
            return Err(self.invalid_request(
                "usb.bulk_read",
                "bulk read endpoint must be greater than zero",
            ));
        }
        if length == 0 {
            return Err(self.invalid_request(
                "usb.bulk_read",
                "bulk read length must be greater than zero",
            ));
        }

        self.with_device_mut("usb.bulk_read", |_device, device| {
            Ok(take_queued_usb_read(
                device.bulk_in_responses.entry(endpoint).or_default(),
                length,
            ))
        })
    }

    fn bulk_write(
        &mut self,
        endpoint: u8,
        bytes: &[u8],
        _timeout_ms: Option<u32>,
    ) -> BusResult<()> {
        self.ensure_writable("usb.bulk_write", "session access is read-only")?;
        if endpoint == 0 {
            return Err(self.invalid_request(
                "usb.bulk_write",
                "bulk write endpoint must be greater than zero",
            ));
        }
        if bytes.is_empty() {
            return Err(
                self.invalid_request("usb.bulk_write", "bulk write payload must not be empty")
            );
        }

        self.with_device_mut("usb.bulk_write", |_device, device| {
            device.last_bulk_writes.insert(endpoint, bytes.to_vec());
            Ok(())
        })
    }

    fn interrupt_read(
        &mut self,
        endpoint: u8,
        length: u32,
        _timeout_ms: Option<u32>,
    ) -> BusResult<Vec<u8>> {
        if endpoint == 0 {
            return Err(self.invalid_request(
                "usb.interrupt_read",
                "interrupt read endpoint must be greater than zero",
            ));
        }
        if length == 0 {
            return Err(self.invalid_request(
                "usb.interrupt_read",
                "interrupt read length must be greater than zero",
            ));
        }

        self.with_device_mut("usb.interrupt_read", |_device, device| {
            Ok(take_queued_usb_read(
                device.interrupt_in_responses.entry(endpoint).or_default(),
                length,
            ))
        })
    }

    fn interrupt_write(&mut self, transfer: &UsbInterruptTransfer) -> BusResult<()> {
        self.ensure_writable("usb.interrupt_write", "session access is read-only")?;
        if transfer.endpoint == 0 {
            return Err(self.invalid_request(
                "usb.interrupt_write",
                "interrupt write endpoint must be greater than zero",
            ));
        }
        if transfer.bytes.is_empty() {
            return Err(self.invalid_request(
                "usb.interrupt_write",
                "interrupt write payload must not be empty",
            ));
        }

        self.with_device_mut("usb.interrupt_write", |_device, device| {
            device
                .last_interrupt_writes
                .insert(transfer.endpoint, transfer.bytes.clone());
            Ok(())
        })
    }

    fn claim_interface(
        &mut self,
        interface_number: u8,
        alternate_setting: Option<u8>,
    ) -> BusResult<()> {
        self.ensure_writable(
            "usb.claim_interface",
            "session access does not allow interface claims",
        )?;

        self.with_device_mut("usb.claim_interface", |device_descriptor, device| {
            if !device.interface_numbers.contains(&interface_number) {
                return Err(BusError::InvalidRequest {
                    device_id: device_descriptor.id.clone(),
                    operation: "usb.claim_interface",
                    reason: format!("USB interface {interface_number} is not present on device"),
                });
            }
            device
                .claimed_interfaces
                .insert(interface_number, alternate_setting);
            Ok(())
        })
    }

    fn release_interface(&mut self, interface_number: u8) -> BusResult<()> {
        self.ensure_writable(
            "usb.release_interface",
            "session access does not allow interface release",
        )?;

        self.with_device_mut("usb.release_interface", |_device, device| {
            device.claimed_interfaces.remove(&interface_number);
            Ok(())
        })
    }
}

fn take_queued_usb_read(queue: &mut VecDeque<Vec<u8>>, length: u32) -> Vec<u8> {
    let mut bytes = queue
        .pop_front()
        .unwrap_or_else(|| vec![0; length as usize]);
    bytes.truncate(length as usize);
    bytes
}
