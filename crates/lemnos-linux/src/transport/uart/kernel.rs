use super::UartTransport;
use crate::backend::LinuxTransportConfig;
use lemnos_bus::{BusError, BusResult};
use lemnos_core::{
    DeviceId, UartConfiguration, UartDataBits, UartFlowControl, UartParity, UartStopBits,
};
use serialport::{DataBits, FlowControl, Parity, SerialPort, StopBits, TTYPort};
use std::io::{ErrorKind, Read, Write};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

pub(super) struct LinuxKernelUartTransport {
    device_id: DeviceId,
    port: Mutex<TTYPort>,
    configuration: Mutex<UartConfiguration>,
    default_timeout: Duration,
}

impl LinuxKernelUartTransport {
    pub(super) fn new(
        device_id: DeviceId,
        devnode: &str,
        transport_config: &LinuxTransportConfig,
    ) -> BusResult<Self> {
        let port = serialport::new(devnode, transport_config.uart_default_baud_rate)
            .timeout(Duration::from_millis(transport_config.uart_timeout_ms))
            .open_native()
            .map_err(|error| classify_open_error(&device_id, devnode, &error))?;
        let configuration =
            read_port_configuration(&device_id, &port).map_err(|error| match error {
                BusError::TransportFailure { reason, .. } => BusError::TransportFailure {
                    device_id: device_id.clone(),
                    operation: "open",
                    reason,
                },
                other => other,
            })?;

        Ok(Self {
            device_id,
            port: Mutex::new(port),
            configuration: Mutex::new(configuration),
            default_timeout: Duration::from_millis(transport_config.uart_timeout_ms),
        })
    }

    fn invalid_request(&self, operation: &'static str, reason: impl Into<String>) -> BusError {
        BusError::InvalidRequest {
            device_id: self.device_id.clone(),
            operation,
            reason: reason.into(),
        }
    }

    fn transport_failure(&self, operation: &'static str, reason: impl Into<String>) -> BusError {
        BusError::TransportFailure {
            device_id: self.device_id.clone(),
            operation,
            reason: reason.into(),
        }
    }

    fn lock_port(&self) -> MutexGuard<'_, TTYPort> {
        self.port
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn lock_configuration(&self) -> MutexGuard<'_, UartConfiguration> {
        self.configuration
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

fn classify_open_error(device_id: &DeviceId, devnode: &str, error: &serialport::Error) -> BusError {
    match error.kind() {
        serialport::ErrorKind::NoDevice => BusError::SessionUnavailable {
            device_id: device_id.clone(),
            reason: format!("Linux UART device '{devnode}' is not currently available: {error}"),
        },
        serialport::ErrorKind::InvalidInput => BusError::InvalidConfiguration {
            device_id: device_id.clone(),
            reason: format!("invalid Linux UART target '{devnode}': {error}"),
        },
        serialport::ErrorKind::Io(kind)
            if kind == ErrorKind::PermissionDenied || kind == ErrorKind::NotFound =>
        {
            if kind == ErrorKind::PermissionDenied {
                BusError::PermissionDenied {
                    device_id: device_id.clone(),
                    operation: "open",
                    reason: format!("failed to open Linux UART device '{devnode}': {error}"),
                }
            } else {
                BusError::SessionUnavailable {
                    device_id: device_id.clone(),
                    reason: format!(
                        "Linux UART device '{devnode}' is not currently available: {error}"
                    ),
                }
            }
        }
        serialport::ErrorKind::Io(ErrorKind::WouldBlock) => BusError::AccessConflict {
            device_id: device_id.clone(),
            reason: format!("Linux UART device '{devnode}' is already in use"),
        },
        _ => BusError::TransportFailure {
            device_id: device_id.clone(),
            operation: "open",
            reason: format!("failed to open Linux UART device '{devnode}': {error}"),
        },
    }
}

impl UartTransport for LinuxKernelUartTransport {
    fn read_into(&mut self, buffer: &mut [u8]) -> BusResult<usize> {
        if buffer.is_empty() {
            return Err(self.invalid_request("uart.read", "max_bytes must be greater than zero"));
        }

        let mut port = self.lock_port();

        match port.read(buffer) {
            Ok(bytes_read) => Ok(bytes_read),
            Err(error) if matches!(error.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock) => {
                Ok(0)
            }
            Err(error) => {
                Err(self.transport_failure("uart.read", format!("Linux UART read failed: {error}")))
            }
        }
    }

    fn read(&mut self, max_bytes: u32) -> BusResult<Vec<u8>> {
        if max_bytes == 0 {
            return Err(self.invalid_request("uart.read", "max_bytes must be greater than zero"));
        }

        let mut buffer = vec![0; max_bytes as usize];
        let bytes_read = self.read_into(&mut buffer)?;
        buffer.truncate(bytes_read);
        Ok(buffer)
    }

    fn write(&mut self, bytes: &[u8]) -> BusResult<()> {
        if bytes.is_empty() {
            return Err(self.invalid_request("uart.write", "write payload must not be empty"));
        }

        let mut port = self.lock_port();
        port.write_all(bytes).map_err(|error| {
            self.transport_failure("uart.write", format!("Linux UART write failed: {error}"))
        })
    }

    fn flush(&mut self) -> BusResult<()> {
        let mut port = self.lock_port();
        port.flush().map_err(|error| {
            self.transport_failure("uart.flush", format!("Linux UART flush failed: {error}"))
        })
    }

    fn configure(&mut self, configuration: &UartConfiguration) -> BusResult<()> {
        validate_configuration(&self.device_id, configuration)?;

        let mut port = self.lock_port();
        port.set_baud_rate(configuration.baud_rate)
            .map_err(|error| {
                self.transport_failure(
                    "uart.configure",
                    format!("failed to set UART baud rate: {error}"),
                )
            })?;
        port.set_data_bits(to_serial_data_bits(configuration.data_bits))
            .map_err(|error| {
                self.transport_failure(
                    "uart.configure",
                    format!("failed to set UART data bits: {error}"),
                )
            })?;
        port.set_parity(to_serial_parity(configuration.parity))
            .map_err(|error| {
                self.transport_failure(
                    "uart.configure",
                    format!("failed to set UART parity: {error}"),
                )
            })?;
        port.set_stop_bits(to_serial_stop_bits(configuration.stop_bits))
            .map_err(|error| {
                self.transport_failure(
                    "uart.configure",
                    format!("failed to set UART stop bits: {error}"),
                )
            })?;
        port.set_flow_control(to_serial_flow_control(configuration.flow_control))
            .map_err(|error| {
                self.transport_failure(
                    "uart.configure",
                    format!("failed to set UART flow control: {error}"),
                )
            })?;
        port.set_timeout(self.default_timeout).map_err(|error| {
            self.transport_failure(
                "uart.configure",
                format!("failed to set UART timeout: {error}"),
            )
        })?;
        let updated_configuration = read_port_configuration(&self.device_id, &port)?;
        drop(port);
        *self.lock_configuration() = updated_configuration;
        Ok(())
    }

    fn configuration(&self) -> BusResult<UartConfiguration> {
        Ok(self.lock_configuration().clone())
    }
}

fn validate_configuration(
    device_id: &DeviceId,
    configuration: &UartConfiguration,
) -> BusResult<()> {
    if configuration.baud_rate == 0 {
        return Err(BusError::InvalidConfiguration {
            device_id: device_id.clone(),
            reason: "UART baud rate must be greater than zero".into(),
        });
    }
    Ok(())
}

fn read_port_configuration(device_id: &DeviceId, port: &TTYPort) -> BusResult<UartConfiguration> {
    Ok(UartConfiguration {
        baud_rate: port
            .baud_rate()
            .map_err(|error| BusError::TransportFailure {
                device_id: device_id.clone(),
                operation: "uart.get_configuration",
                reason: format!("failed to read UART baud rate: {error}"),
            })?,
        data_bits: from_serial_data_bits(port.data_bits().map_err(|error| {
            BusError::TransportFailure {
                device_id: device_id.clone(),
                operation: "uart.get_configuration",
                reason: format!("failed to read UART data bits: {error}"),
            }
        })?),
        parity: from_serial_parity(port.parity().map_err(|error| BusError::TransportFailure {
            device_id: device_id.clone(),
            operation: "uart.get_configuration",
            reason: format!("failed to read UART parity: {error}"),
        })?),
        stop_bits: from_serial_stop_bits(port.stop_bits().map_err(|error| {
            BusError::TransportFailure {
                device_id: device_id.clone(),
                operation: "uart.get_configuration",
                reason: format!("failed to read UART stop bits: {error}"),
            }
        })?),
        flow_control: from_serial_flow_control(port.flow_control().map_err(|error| {
            BusError::TransportFailure {
                device_id: device_id.clone(),
                operation: "uart.get_configuration",
                reason: format!("failed to read UART flow control: {error}"),
            }
        })?),
    })
}

fn to_serial_data_bits(data_bits: UartDataBits) -> DataBits {
    match data_bits {
        UartDataBits::Five => DataBits::Five,
        UartDataBits::Six => DataBits::Six,
        UartDataBits::Seven => DataBits::Seven,
        UartDataBits::Eight => DataBits::Eight,
    }
}

fn from_serial_data_bits(data_bits: DataBits) -> UartDataBits {
    match data_bits {
        DataBits::Five => UartDataBits::Five,
        DataBits::Six => UartDataBits::Six,
        DataBits::Seven => UartDataBits::Seven,
        DataBits::Eight => UartDataBits::Eight,
    }
}

fn to_serial_parity(parity: UartParity) -> Parity {
    match parity {
        UartParity::None => Parity::None,
        UartParity::Even => Parity::Even,
        UartParity::Odd => Parity::Odd,
    }
}

fn from_serial_parity(parity: Parity) -> UartParity {
    match parity {
        Parity::None => UartParity::None,
        Parity::Even => UartParity::Even,
        Parity::Odd => UartParity::Odd,
    }
}

fn to_serial_stop_bits(stop_bits: UartStopBits) -> StopBits {
    match stop_bits {
        UartStopBits::One => StopBits::One,
        UartStopBits::Two => StopBits::Two,
    }
}

fn from_serial_stop_bits(stop_bits: StopBits) -> UartStopBits {
    match stop_bits {
        StopBits::One => UartStopBits::One,
        StopBits::Two => UartStopBits::Two,
    }
}

fn to_serial_flow_control(flow_control: UartFlowControl) -> FlowControl {
    match flow_control {
        UartFlowControl::None => FlowControl::None,
        UartFlowControl::Software => FlowControl::Software,
        UartFlowControl::Hardware => FlowControl::Hardware,
    }
}

fn from_serial_flow_control(flow_control: FlowControl) -> UartFlowControl {
    match flow_control {
        FlowControl::None => UartFlowControl::None,
        FlowControl::Software => UartFlowControl::Software,
        FlowControl::Hardware => UartFlowControl::Hardware,
    }
}
