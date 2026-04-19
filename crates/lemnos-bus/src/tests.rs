use crate::contract::assert_stream_poll_contract;
use crate::*;
use lemnos_core::{
    DeviceDescriptor, DeviceId, GpioDirection, GpioEdge, GpioLevel, GpioLineConfiguration,
    I2cOperation, InterfaceKind, TimestampMs, UartConfiguration, UartDataBits, UartFlowControl,
    UartParity, UartStopBits,
};

struct FakeGpioSession {
    device: DeviceDescriptor,
    metadata: SessionMetadata,
    level: GpioLevel,
    configuration: GpioLineConfiguration,
}

impl FakeGpioSession {
    fn new() -> Self {
        Self {
            device: DeviceDescriptor::new("gpio.line0", InterfaceKind::Gpio)
                .expect("device descriptor"),
            metadata: SessionMetadata::new("fake", SessionAccess::Exclusive),
            level: GpioLevel::Low,
            configuration: GpioLineConfiguration {
                direction: GpioDirection::Output,
                active_low: false,
                bias: None,
                drive: None,
                edge: None,
                debounce_us: None,
                initial_level: Some(GpioLevel::Low),
            },
        }
    }
}

impl BusSession for FakeGpioSession {
    fn interface(&self) -> InterfaceKind {
        InterfaceKind::Gpio
    }

    fn device(&self) -> &DeviceDescriptor {
        &self.device
    }

    fn metadata(&self) -> &SessionMetadata {
        &self.metadata
    }

    fn close(&mut self) -> BusResult<()> {
        self.metadata.state = SessionState::Closed;
        Ok(())
    }
}

impl GpioSession for FakeGpioSession {
    fn read_level(&mut self) -> BusResult<GpioLevel> {
        Ok(self.level)
    }

    fn write_level(&mut self, level: GpioLevel) -> BusResult<()> {
        self.level = level;
        Ok(())
    }

    fn configure_line(&mut self, configuration: &GpioLineConfiguration) -> BusResult<()> {
        self.configuration = configuration.clone();
        Ok(())
    }

    fn configuration(&self) -> BusResult<GpioLineConfiguration> {
        Ok(self.configuration.clone())
    }
}

struct FakeGpioEdgeSession {
    inner: FakeGpioSession,
    events: Vec<GpioEdgeEvent>,
}

impl BusSession for FakeGpioEdgeSession {
    fn interface(&self) -> InterfaceKind {
        self.inner.interface()
    }

    fn device(&self) -> &DeviceDescriptor {
        self.inner.device()
    }

    fn metadata(&self) -> &SessionMetadata {
        self.inner.metadata()
    }

    fn close(&mut self) -> BusResult<()> {
        self.inner.close()
    }
}

impl GpioSession for FakeGpioEdgeSession {
    fn read_level(&mut self) -> BusResult<GpioLevel> {
        self.inner.read_level()
    }

    fn write_level(&mut self, level: GpioLevel) -> BusResult<()> {
        self.inner.write_level(level)
    }

    fn configure_line(&mut self, configuration: &GpioLineConfiguration) -> BusResult<()> {
        self.inner.configure_line(configuration)
    }

    fn configuration(&self) -> BusResult<GpioLineConfiguration> {
        self.inner.configuration()
    }
}

impl StreamSession for FakeGpioEdgeSession {
    type Event = GpioEdgeEvent;

    fn poll_events(
        &mut self,
        max_events: u32,
        _timeout_ms: Option<u32>,
    ) -> BusResult<Vec<Self::Event>> {
        Ok(self
            .events
            .iter()
            .take(max_events as usize)
            .cloned()
            .collect())
    }
}

struct FakeI2cControllerSession {
    owner: DeviceDescriptor,
    metadata: SessionMetadata,
    bus: u32,
    last_address: Option<u16>,
}

impl BusSession for FakeI2cControllerSession {
    fn interface(&self) -> InterfaceKind {
        InterfaceKind::I2c
    }

    fn device(&self) -> &DeviceDescriptor {
        &self.owner
    }

    fn metadata(&self) -> &SessionMetadata {
        &self.metadata
    }

    fn close(&mut self) -> BusResult<()> {
        self.metadata.state = SessionState::Closed;
        Ok(())
    }
}

impl I2cControllerSession for FakeI2cControllerSession {
    fn bus(&self) -> u32 {
        self.bus
    }

    fn read(&mut self, address: u16, length: u32) -> BusResult<Vec<u8>> {
        self.last_address = Some(address);
        Ok(vec![0; length as usize])
    }

    fn write(&mut self, address: u16, _bytes: &[u8]) -> BusResult<()> {
        self.last_address = Some(address);
        Ok(())
    }

    fn write_read(&mut self, address: u16, _write: &[u8], read_length: u32) -> BusResult<Vec<u8>> {
        self.last_address = Some(address);
        Ok(vec![0; read_length as usize])
    }

    fn transaction(
        &mut self,
        address: u16,
        _operations: &[I2cOperation],
    ) -> BusResult<Vec<Vec<u8>>> {
        self.last_address = Some(address);
        Ok(Vec::new())
    }
}

struct FakeUartStreamSession {
    device: DeviceDescriptor,
    metadata: SessionMetadata,
    configuration: UartConfiguration,
    events: Vec<UartReadChunk>,
}

impl BusSession for FakeUartStreamSession {
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
        self.metadata.state = SessionState::Closed;
        Ok(())
    }
}

impl UartSession for FakeUartStreamSession {
    fn read(&mut self, _max_bytes: u32) -> BusResult<Vec<u8>> {
        Ok(Vec::new())
    }

    fn write(&mut self, _bytes: &[u8]) -> BusResult<()> {
        Ok(())
    }

    fn flush(&mut self) -> BusResult<()> {
        Ok(())
    }

    fn configure(&mut self, configuration: &UartConfiguration) -> BusResult<()> {
        self.configuration = configuration.clone();
        Ok(())
    }

    fn configuration(&self) -> BusResult<UartConfiguration> {
        Ok(self.configuration.clone())
    }
}

impl StreamSession for FakeUartStreamSession {
    type Event = UartReadChunk;

    fn poll_events(
        &mut self,
        max_events: u32,
        _timeout_ms: Option<u32>,
    ) -> BusResult<Vec<Self::Event>> {
        Ok(self
            .events
            .iter()
            .take(max_events as usize)
            .cloned()
            .collect())
    }
}

#[test]
fn session_access_reports_expected_permissions() {
    assert!(SessionAccess::SharedReadOnly.can_read());
    assert!(!SessionAccess::SharedReadOnly.can_write());
    assert!(SessionAccess::Exclusive.is_exclusive());
}

#[test]
fn session_metadata_tracks_open_activity_and_fault_transitions() {
    let mut metadata = SessionMetadata::new("fake", SessionAccess::Exclusive);
    let opened_at = metadata.opened_at.expect("opened_at");
    let first_active_at = metadata.last_active_at.expect("last_active_at");
    assert_eq!(metadata.state, SessionState::Open);

    metadata.mark_idle();
    metadata.begin_call();
    assert_eq!(metadata.state, SessionState::Busy);
    assert!(metadata.last_active_at >= Some(first_active_at));

    let ok_result: BusResult<()> = Ok(());
    metadata.finish_call(&ok_result);
    assert_eq!(metadata.state, SessionState::Idle);
    assert_eq!(metadata.opened_at, Some(opened_at));

    metadata.begin_call();
    let err_result: BusResult<()> = Err(BusError::Disconnected {
        device_id: DeviceId::new("gpio.line0").expect("device id"),
    });
    metadata.finish_call(&err_result);
    assert_eq!(metadata.state, SessionState::Faulted);

    metadata.mark_closed();
    assert_eq!(metadata.state, SessionState::Closed);
    assert_eq!(metadata.opened_at, Some(opened_at));
}

#[test]
fn fake_gpio_session_round_trips_level_and_configuration() {
    let mut session = FakeGpioSession::new();
    session.write_level(GpioLevel::High).expect("write level");
    assert_eq!(session.read_level().expect("read level"), GpioLevel::High);
    assert_eq!(
        session.configuration().expect("configuration").direction,
        GpioDirection::Output
    );
}

#[test]
fn uart_configuration_is_usable_from_core_types() {
    let config = UartConfiguration {
        baud_rate: 115_200,
        data_bits: UartDataBits::Eight,
        parity: UartParity::None,
        stop_bits: UartStopBits::One,
        flow_control: UartFlowControl::None,
    };

    assert_eq!(config.baud_rate, 115_200);
}

#[test]
fn i2c_controller_session_targets_bus_and_address() {
    let owner =
        DeviceDescriptor::new("i2c.controller0", InterfaceKind::I2c).expect("owner descriptor");
    let mut session = FakeI2cControllerSession {
        owner,
        metadata: SessionMetadata::new("fake", SessionAccess::ExclusiveController),
        bus: 4,
        last_address: None,
    };

    let bytes = session
        .write_read(0x68, &[0x00], 2)
        .expect("controller write_read");

    assert_eq!(session.bus(), 4);
    assert_eq!(session.last_address, Some(0x68));
    assert_eq!(bytes, vec![0, 0]);
}

#[test]
fn gpio_edge_stream_sessions_use_typed_polling_contract() {
    let mut session = FakeGpioEdgeSession {
        inner: FakeGpioSession::new(),
        events: vec![
            GpioEdgeEvent {
                edge: GpioEdge::Rising,
                level: Some(GpioLevel::High),
                sequence: 1,
                observed_at: Some(TimestampMs::new(10)),
            },
            GpioEdgeEvent {
                edge: GpioEdge::Falling,
                level: Some(GpioLevel::Low),
                sequence: 2,
                observed_at: Some(TimestampMs::new(20)),
            },
        ],
    };

    assert_stream_poll_contract(
        &mut session,
        2,
        Some(50),
        &[
            GpioEdgeEvent {
                edge: GpioEdge::Rising,
                level: Some(GpioLevel::High),
                sequence: 1,
                observed_at: Some(TimestampMs::new(10)),
            },
            GpioEdgeEvent {
                edge: GpioEdge::Falling,
                level: Some(GpioLevel::Low),
                sequence: 2,
                observed_at: Some(TimestampMs::new(20)),
            },
        ],
    );
}

#[test]
fn uart_stream_sessions_use_typed_polling_contract() {
    let device = DeviceDescriptor::new("uart.port0", InterfaceKind::Uart).expect("uart descriptor");
    let mut session = FakeUartStreamSession {
        device,
        metadata: SessionMetadata::new("fake", SessionAccess::Shared),
        configuration: UartConfiguration {
            baud_rate: 115_200,
            data_bits: UartDataBits::Eight,
            parity: UartParity::None,
            stop_bits: UartStopBits::One,
            flow_control: UartFlowControl::None,
        },
        events: vec![UartReadChunk {
            bytes: vec![0xaa, 0xbb, 0xcc],
            sequence: 7,
            observed_at: Some(TimestampMs::new(44)),
        }],
    };

    assert_stream_poll_contract(
        &mut session,
        1,
        Some(25),
        &[UartReadChunk {
            bytes: vec![0xaa, 0xbb, 0xcc],
            sequence: 7,
            observed_at: Some(TimestampMs::new(44)),
        }],
    );
}
