use crate::hardware::{MOCK_BACKEND_NAME, MockHardwareState, take_injected_error};
use lemnos_bus::{
    BusError, BusResult, BusSession, SessionAccess, SessionMetadata, SessionState, UartSession,
};
use lemnos_core::{
    CapabilityAccess, CapabilityDescriptor, DeviceAddress, DeviceDescriptor, DeviceKind,
    InterfaceKind, UartConfiguration, UartDataBits, UartFlowControl, UartParity, UartStopBits,
};
use std::sync::{Arc, Mutex, MutexGuard};

#[derive(Clone)]
pub struct MockUartPort {
    descriptor: DeviceDescriptor,
    configuration: UartConfiguration,
    rx_buffer: Vec<u8>,
    tx_buffer: Vec<u8>,
}

impl MockUartPort {
    pub fn new(port: impl AsRef<str>) -> Self {
        let port = port.as_ref().to_string();
        let descriptor =
            DeviceDescriptor::builder_for_kind(format!("mock.uart.{port}"), DeviceKind::UartPort)
                .expect("mock uart builder")
                .display_name(port.clone())
                .summary("Mock UART port")
                .address(DeviceAddress::UartPort { port: port.clone() })
                .driver_hint("lemnos.uart.generic")
                .label("port", port.clone())
                .property("port", port.clone())
                .capability(
                    CapabilityDescriptor::new("uart.read", CapabilityAccess::READ)
                        .expect("uart.read capability"),
                )
                .capability(
                    CapabilityDescriptor::new("uart.write", CapabilityAccess::WRITE)
                        .expect("uart.write capability"),
                )
                .capability(
                    CapabilityDescriptor::new("uart.configure", CapabilityAccess::CONFIGURE)
                        .expect("uart.configure capability"),
                )
                .capability(
                    CapabilityDescriptor::new("uart.flush", CapabilityAccess::WRITE)
                        .expect("uart.flush capability"),
                )
                .capability(
                    CapabilityDescriptor::new("uart.get_configuration", CapabilityAccess::READ)
                        .expect("uart.get_configuration capability"),
                )
                .build()
                .expect("mock uart descriptor");

        Self {
            descriptor,
            configuration: UartConfiguration {
                baud_rate: 115_200,
                data_bits: UartDataBits::Eight,
                parity: UartParity::None,
                stop_bits: UartStopBits::One,
                flow_control: UartFlowControl::None,
            },
            rx_buffer: Vec::new(),
            tx_buffer: Vec::new(),
        }
    }

    pub fn with_display_name(mut self, display_name: impl Into<String>) -> Self {
        self.descriptor.display_name = Some(display_name.into());
        self
    }

    pub fn with_configuration(mut self, configuration: UartConfiguration) -> Self {
        validate_configuration(&self.descriptor, &configuration)
            .expect("mock UART configuration must be valid");
        self.configuration = configuration;
        self
    }

    pub fn with_rx_bytes(mut self, bytes: impl AsRef<[u8]>) -> Self {
        self.rx_buffer.extend_from_slice(bytes.as_ref());
        self
    }

    pub fn descriptor(&self) -> &DeviceDescriptor {
        &self.descriptor
    }
}

#[derive(Clone)]
pub(crate) struct MockUartPortState {
    pub descriptor: DeviceDescriptor,
    pub configuration: UartConfiguration,
    pub rx_buffer: Vec<u8>,
    pub tx_buffer: Vec<u8>,
}

impl From<MockUartPort> for MockUartPortState {
    fn from(value: MockUartPort) -> Self {
        Self {
            descriptor: value.descriptor,
            configuration: value.configuration,
            rx_buffer: value.rx_buffer,
            tx_buffer: value.tx_buffer,
        }
    }
}

pub(crate) struct MockUartSession {
    state: Arc<Mutex<MockHardwareState>>,
    device: DeviceDescriptor,
    metadata: SessionMetadata,
}

impl MockUartSession {
    pub(crate) fn new(
        state: Arc<Mutex<MockHardwareState>>,
        device: DeviceDescriptor,
        access: SessionAccess,
    ) -> Self {
        Self {
            state,
            device,
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

    fn port_state(&self) -> BusResult<MockUartPortState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .uart_ports
            .get(&self.device.id)
            .cloned()
            .ok_or_else(|| BusError::Disconnected {
                device_id: self.device.id.clone(),
            })
    }

    fn port_state_mut(&self) -> BusResult<MutexGuard<'_, MockHardwareState>> {
        let guard = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !guard.uart_ports.contains_key(&self.device.id) {
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
}

impl BusSession for MockUartSession {
    fn interface(&self) -> InterfaceKind {
        InterfaceKind::Uart
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

impl UartSession for MockUartSession {
    fn read(&mut self, max_bytes: u32) -> BusResult<Vec<u8>> {
        if max_bytes == 0 {
            return Err(self.invalid_request("uart.read", "max_bytes must be greater than zero"));
        }

        self.run_call(|session| {
            take_injected_error(&session.state, &session.device.id, "uart.read")?;
            let mut state = session.port_state_mut()?;
            let port = state
                .uart_ports
                .get_mut(&session.device.id)
                .expect("port existence checked before mutation");
            let count = usize::min(max_bytes as usize, port.rx_buffer.len());
            Ok(port.rx_buffer.drain(..count).collect())
        })
    }

    fn write(&mut self, bytes: &[u8]) -> BusResult<()> {
        if !self.metadata.access.can_write() {
            return Err(self.permission_denied("uart.write", "session access is read-only"));
        }
        if bytes.is_empty() {
            return Err(self.invalid_request("uart.write", "write payload must not be empty"));
        }

        self.run_call(|session| {
            take_injected_error(&session.state, &session.device.id, "uart.write")?;
            let mut state = session.port_state_mut()?;
            let port = state
                .uart_ports
                .get_mut(&session.device.id)
                .expect("port existence checked before mutation");
            port.tx_buffer.extend_from_slice(bytes);
            Ok(())
        })
    }

    fn flush(&mut self) -> BusResult<()> {
        if !self.metadata.access.can_write() {
            return Err(self.permission_denied("uart.flush", "session access is read-only"));
        }
        self.run_call(|session| {
            take_injected_error(&session.state, &session.device.id, "uart.flush")?;
            Ok(())
        })
    }

    fn configure(&mut self, configuration: &UartConfiguration) -> BusResult<()> {
        if !self.metadata.access.can_write() {
            return Err(self.permission_denied(
                "uart.configure",
                "session access does not allow configuration changes",
            ));
        }
        validate_configuration(&self.device, configuration)?;

        self.run_call(|session| {
            take_injected_error(&session.state, &session.device.id, "uart.configure")?;
            let mut state = session.port_state_mut()?;
            let port = state
                .uart_ports
                .get_mut(&session.device.id)
                .expect("port existence checked before mutation");
            port.configuration = configuration.clone();
            Ok(())
        })
    }

    fn configuration(&self) -> BusResult<UartConfiguration> {
        take_injected_error(&self.state, &self.device.id, "uart.get_configuration")?;
        Ok(self.port_state()?.configuration)
    }
}

fn validate_configuration(
    device: &DeviceDescriptor,
    configuration: &UartConfiguration,
) -> BusResult<()> {
    if configuration.baud_rate == 0 {
        return Err(BusError::InvalidConfiguration {
            device_id: device.id.clone(),
            reason: "UART baud rate must be greater than zero".into(),
        });
    }
    Ok(())
}
