use super::UartTransport;
use super::sessions::LinuxUartSession;
use crate::metadata::descriptor_devnode;
use lemnos_bus::{BusError, BusSession, SessionAccess, SessionState, UartSession};
use lemnos_core::{
    DeviceAddress, DeviceDescriptor, DeviceKind, InterfaceKind, UartConfiguration, UartDataBits,
    UartFlowControl, UartParity, UartStopBits,
};

struct MockTransport {
    configuration: UartConfiguration,
    rx: Vec<u8>,
    tx: Vec<u8>,
}

impl MockTransport {
    fn new() -> Self {
        Self {
            configuration: UartConfiguration {
                baud_rate: 115_200,
                data_bits: UartDataBits::Eight,
                parity: UartParity::None,
                stop_bits: UartStopBits::One,
                flow_control: UartFlowControl::None,
            },
            rx: vec![0x48, 0x69],
            tx: Vec::new(),
        }
    }
}

impl UartTransport for MockTransport {
    fn read_into(&mut self, buffer: &mut [u8]) -> lemnos_bus::BusResult<usize> {
        let count = usize::min(buffer.len(), self.rx.len());
        buffer[..count].copy_from_slice(&self.rx[..count]);
        self.rx.drain(..count);
        Ok(count)
    }

    fn read(&mut self, max_bytes: u32) -> lemnos_bus::BusResult<Vec<u8>> {
        let count = usize::min(max_bytes as usize, self.rx.len());
        Ok(self.rx.drain(..count).collect())
    }

    fn write(&mut self, bytes: &[u8]) -> lemnos_bus::BusResult<()> {
        self.tx.extend_from_slice(bytes);
        Ok(())
    }

    fn flush(&mut self) -> lemnos_bus::BusResult<()> {
        Ok(())
    }

    fn configure(&mut self, configuration: &UartConfiguration) -> lemnos_bus::BusResult<()> {
        self.configuration = configuration.clone();
        Ok(())
    }

    fn configuration(&self) -> lemnos_bus::BusResult<UartConfiguration> {
        Ok(self.configuration.clone())
    }
}

fn test_device() -> DeviceDescriptor {
    DeviceDescriptor::builder_for_kind("linux.uart.port.ttyUSB0", DeviceKind::UartPort)
        .expect("builder")
        .address(DeviceAddress::UartPort {
            port: "ttyUSB0".into(),
        })
        .property("devnode", "/dev/ttyUSB0")
        .build()
        .expect("descriptor")
}

#[test]
fn uart_session_round_trips_through_transport() {
    let device = test_device();
    let mut session = LinuxUartSession::with_transport(
        device.clone(),
        SessionAccess::Exclusive,
        Box::new(MockTransport::new()),
    );

    let bytes = session.read(2).expect("read");
    assert_eq!(bytes, vec![0x48, 0x69]);

    session.write(&[0xAA, 0x55]).expect("write");
    session
        .configure(&UartConfiguration {
            baud_rate: 57_600,
            data_bits: UartDataBits::Seven,
            parity: UartParity::Even,
            stop_bits: UartStopBits::Two,
            flow_control: UartFlowControl::Hardware,
        })
        .expect("configure");

    assert_eq!(
        session.configuration().expect("configuration"),
        UartConfiguration {
            baud_rate: 57_600,
            data_bits: UartDataBits::Seven,
            parity: UartParity::Even,
            stop_bits: UartStopBits::Two,
            flow_control: UartFlowControl::Hardware,
        }
    );
    assert_eq!(descriptor_devnode(session.device()), Some("/dev/ttyUSB0"));
}

#[test]
fn uart_session_rejects_operations_after_close_and_new_session_can_reopen() {
    let device = test_device();
    let mut session = LinuxUartSession::with_transport(
        device.clone(),
        SessionAccess::Exclusive,
        Box::new(MockTransport::new()),
    );
    session.close().expect("close");

    assert!(matches!(
        session.read(1),
        Err(BusError::SessionUnavailable { .. })
    ));
    assert!(matches!(
        session.write(&[0xAA]),
        Err(BusError::SessionUnavailable { .. })
    ));

    let reopened = LinuxUartSession::with_transport(
        device,
        SessionAccess::Exclusive,
        Box::new(MockTransport::new()),
    );
    assert_eq!(reopened.metadata().state, SessionState::Idle);
}

#[test]
fn uart_supports_descriptor_requires_typed_address() {
    let property_only = DeviceDescriptor::builder("linux.uart.port.ttyUSB0", InterfaceKind::Uart)
        .expect("builder")
        .kind(DeviceKind::UartPort)
        .property("port", "ttyUSB0")
        .property("devnode", "/dev/ttyUSB0")
        .build()
        .expect("descriptor");
    assert!(!super::supports_descriptor(&property_only));
}
