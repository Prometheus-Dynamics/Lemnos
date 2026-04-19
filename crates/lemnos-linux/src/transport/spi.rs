use crate::LinuxPaths;
use crate::backend::BACKEND_NAME;
use crate::metadata::descriptor_devnode;
use crate::transport;
use crate::transport::session;
use lemnos_bus::{
    BusError, BusResult, BusSession, SessionAccess, SessionMetadata, SessionState, SpiSession,
};
#[cfg(test)]
use lemnos_core::DeviceAddress;
use lemnos_core::{
    DeviceDescriptor, DeviceKind, InterfaceKind, SpiBitOrder, SpiConfiguration, SpiMode,
};
use spidev::spidevioctl::{get_bits_per_word, get_lsb_first, get_max_speed_hz, get_mode};
use spidev::{SpiModeFlags, Spidev, SpidevOptions, SpidevTransfer};
use std::os::fd::AsRawFd;

pub(crate) fn supports_descriptor(device: &DeviceDescriptor) -> bool {
    device.interface == InterfaceKind::Spi
        && device.kind == DeviceKind::SpiDevice
        && transport::spi_bus_chip_select(device).is_some()
}

pub(crate) fn open_session(
    paths: &LinuxPaths,
    device: &DeviceDescriptor,
    access: SessionAccess,
) -> BusResult<Box<dyn SpiSession>> {
    LinuxSpiSession::open(paths, device, access)
        .map(|session| Box::new(session) as Box<dyn SpiSession>)
}

trait SpiTransport: Send + Sync {
    fn transfer(&mut self, write: &[u8]) -> BusResult<Vec<u8>>;
    fn write(&mut self, bytes: &[u8]) -> BusResult<()>;
    fn configure(&mut self, configuration: &SpiConfiguration) -> BusResult<()>;
    fn configuration(&self) -> BusResult<SpiConfiguration>;
}

pub(crate) struct LinuxSpiSession {
    device: DeviceDescriptor,
    metadata: SessionMetadata,
    transport: Box<dyn SpiTransport>,
}

impl LinuxSpiSession {
    fn open(
        paths: &LinuxPaths,
        device: &DeviceDescriptor,
        access: SessionAccess,
    ) -> BusResult<Self> {
        if !supports_descriptor(device) {
            return Err(BusError::UnsupportedDevice {
                backend: BACKEND_NAME.to_string(),
                device_id: device.id.clone(),
            });
        }

        let (bus, chip_select) =
            transport::spi_bus_chip_select(device).ok_or_else(|| BusError::UnsupportedDevice {
                backend: BACKEND_NAME.to_string(),
                device_id: device.id.clone(),
            })?;
        let devnode = descriptor_devnode(device)
            .map(str::to_owned)
            .unwrap_or_else(|| resolve_devnode(paths, bus, chip_select));
        let transport = LinuxKernelSpiTransport::new(device.id.clone(), &devnode)?;

        Ok(Self {
            device: device.clone(),
            metadata: SessionMetadata::new(BACKEND_NAME, access).with_state(SessionState::Idle),
            transport: Box::new(transport),
        })
    }

    #[cfg(test)]
    fn with_transport(
        device: DeviceDescriptor,
        access: SessionAccess,
        transport: Box<dyn SpiTransport>,
    ) -> Self {
        Self {
            device,
            metadata: SessionMetadata::new(BACKEND_NAME, access).with_state(SessionState::Idle),
            transport,
        }
    }

    fn ensure_open(&self, operation: &'static str) -> BusResult<()> {
        session::ensure_open(&self.metadata, &self.device.id, "SPI", operation)
    }

    fn ensure_writable(&self, operation: &'static str, reason: &'static str) -> BusResult<()> {
        if self.metadata.access.can_write() {
            Ok(())
        } else {
            Err(session::permission_denied(
                &self.device.id,
                operation,
                reason,
            ))
        }
    }

    fn run_transport_call<T>(
        &mut self,
        operation: &'static str,
        call: impl FnOnce(&mut dyn SpiTransport) -> BusResult<T>,
    ) -> BusResult<T> {
        self.ensure_open(operation)?;
        session::run_call(&mut self.metadata, &mut self.transport, |transport| {
            call(transport.as_mut())
        })
    }
}

impl BusSession for LinuxSpiSession {
    fn interface(&self) -> InterfaceKind {
        InterfaceKind::Spi
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

impl SpiSession for LinuxSpiSession {
    fn transfer(&mut self, write: &[u8]) -> BusResult<Vec<u8>> {
        self.ensure_writable("spi.transfer", "session access is read-only")?;
        self.run_transport_call("spi.transfer", |transport| transport.transfer(write))
    }

    fn write(&mut self, bytes: &[u8]) -> BusResult<()> {
        self.ensure_writable("spi.write", "session access is read-only")?;
        self.run_transport_call("spi.write", |transport| transport.write(bytes))
    }

    fn configure(&mut self, configuration: &SpiConfiguration) -> BusResult<()> {
        self.ensure_writable(
            "spi.configure",
            "session access does not allow configuration changes",
        )?;
        self.run_transport_call("spi.configure", |transport| {
            transport.configure(configuration)
        })
    }

    fn configuration(&self) -> BusResult<SpiConfiguration> {
        self.ensure_open("spi.get_configuration")?;
        self.transport.configuration()
    }
}

struct LinuxKernelSpiTransport {
    device_id: lemnos_core::DeviceId,
    spi: Spidev,
    configuration: SpiConfiguration,
}

impl LinuxKernelSpiTransport {
    fn new(device_id: lemnos_core::DeviceId, devnode: &str) -> BusResult<Self> {
        let spi = Spidev::open(devnode)
            .map_err(|error| classify_open_error(&device_id, devnode, &error))?;
        let configuration =
            read_kernel_configuration(&device_id, &spi).map_err(|error| match error {
                BusError::TransportFailure { reason, .. } => BusError::TransportFailure {
                    device_id: device_id.clone(),
                    operation: "open",
                    reason,
                },
                other => other,
            })?;

        Ok(Self {
            device_id,
            spi,
            configuration,
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
}

fn classify_open_error(
    device_id: &lemnos_core::DeviceId,
    devnode: &str,
    error: &std::io::Error,
) -> BusError {
    let kind = error.kind();
    let raw_os_error = error.raw_os_error();

    if kind == std::io::ErrorKind::PermissionDenied || matches!(raw_os_error, Some(1 | 13)) {
        return BusError::PermissionDenied {
            device_id: device_id.clone(),
            operation: "open",
            reason: format!("failed to open Linux SPI device '{devnode}': {error}"),
        };
    }

    if matches!(raw_os_error, Some(16)) {
        return BusError::AccessConflict {
            device_id: device_id.clone(),
            reason: format!("Linux SPI device '{devnode}' is already in use"),
        };
    }

    if kind == std::io::ErrorKind::NotFound || matches!(raw_os_error, Some(2 | 6 | 19)) {
        return BusError::SessionUnavailable {
            device_id: device_id.clone(),
            reason: format!("Linux SPI device '{devnode}' is not currently available: {error}"),
        };
    }

    BusError::TransportFailure {
        device_id: device_id.clone(),
        operation: "open",
        reason: format!("failed to open Linux SPI device '{devnode}': {error}"),
    }
}

impl SpiTransport for LinuxKernelSpiTransport {
    fn transfer(&mut self, write: &[u8]) -> BusResult<Vec<u8>> {
        if write.is_empty() {
            return Err(self.invalid_request("spi.transfer", "transfer payload must not be empty"));
        }

        let mut read = vec![0; write.len()];
        let mut transfer = SpidevTransfer::read_write(write, &mut read);
        self.spi.transfer(&mut transfer).map_err(|error| {
            self.transport_failure(
                "spi.transfer",
                format!("Linux SPI transfer failed: {error}"),
            )
        })?;
        Ok(read)
    }

    fn write(&mut self, bytes: &[u8]) -> BusResult<()> {
        if bytes.is_empty() {
            return Err(self.invalid_request("spi.write", "write payload must not be empty"));
        }

        std::io::Write::write_all(&mut self.spi, bytes).map_err(|error| {
            self.transport_failure("spi.write", format!("Linux SPI write failed: {error}"))
        })
    }

    fn configure(&mut self, configuration: &SpiConfiguration) -> BusResult<()> {
        validate_configuration(&self.device_id, configuration)?;

        let options = to_spidev_options(configuration);
        self.spi.configure(&options).map_err(|error| {
            self.transport_failure(
                "spi.configure",
                format!("Linux SPI configure failed: {error}"),
            )
        })?;
        self.configuration = read_kernel_configuration(&self.device_id, &self.spi)?;
        Ok(())
    }

    fn configuration(&self) -> BusResult<SpiConfiguration> {
        Ok(self.configuration.clone())
    }
}

fn resolve_devnode(paths: &LinuxPaths, bus: u32, chip_select: u16) -> String {
    paths.spi_devnode(bus, chip_select).display().to_string()
}

fn validate_configuration(
    device_id: &lemnos_core::DeviceId,
    configuration: &SpiConfiguration,
) -> BusResult<()> {
    if configuration.max_frequency_hz == Some(0) {
        return Err(BusError::InvalidConfiguration {
            device_id: device_id.clone(),
            reason: "SPI max frequency must be greater than zero".into(),
        });
    }
    if configuration.bits_per_word == Some(0) {
        return Err(BusError::InvalidConfiguration {
            device_id: device_id.clone(),
            reason: "SPI bits per word must be greater than zero".into(),
        });
    }
    Ok(())
}

fn read_kernel_configuration(
    device_id: &lemnos_core::DeviceId,
    spi: &Spidev,
) -> BusResult<SpiConfiguration> {
    let fd = spi.inner().as_raw_fd();
    let mode_bits = get_mode(fd).map_err(|error| BusError::TransportFailure {
        device_id: device_id.clone(),
        operation: "spi.get_configuration",
        reason: format!("failed to read Linux SPI mode: {error}"),
    })?;
    let bits_per_word = get_bits_per_word(fd).map_err(|error| BusError::TransportFailure {
        device_id: device_id.clone(),
        operation: "spi.get_configuration",
        reason: format!("failed to read Linux SPI bits per word: {error}"),
    })?;
    let max_speed_hz = get_max_speed_hz(fd).map_err(|error| BusError::TransportFailure {
        device_id: device_id.clone(),
        operation: "spi.get_configuration",
        reason: format!("failed to read Linux SPI max speed: {error}"),
    })?;
    let lsb_first = get_lsb_first(fd).map_err(|error| BusError::TransportFailure {
        device_id: device_id.clone(),
        operation: "spi.get_configuration",
        reason: format!("failed to read Linux SPI bit order: {error}"),
    })?;

    Ok(SpiConfiguration {
        mode: mode_from_bits(mode_bits),
        max_frequency_hz: (max_speed_hz != 0).then_some(max_speed_hz),
        bits_per_word: Some(if bits_per_word == 0 { 8 } else { bits_per_word }),
        bit_order: if lsb_first == 0 {
            SpiBitOrder::MsbFirst
        } else {
            SpiBitOrder::LsbFirst
        },
    })
}

fn to_spidev_options(configuration: &SpiConfiguration) -> SpidevOptions {
    let mut options = SpidevOptions::new();
    if let Some(bits_per_word) = configuration.bits_per_word {
        options.bits_per_word(bits_per_word);
    }
    if let Some(max_frequency_hz) = configuration.max_frequency_hz {
        options.max_speed_hz(max_frequency_hz);
    }
    options
        .lsb_first(matches!(configuration.bit_order, SpiBitOrder::LsbFirst))
        .mode(mode_to_flags(configuration.mode));
    options.build()
}

fn mode_to_flags(mode: SpiMode) -> SpiModeFlags {
    match mode {
        SpiMode::Mode0 => SpiModeFlags::SPI_MODE_0,
        SpiMode::Mode1 => SpiModeFlags::SPI_MODE_1,
        SpiMode::Mode2 => SpiModeFlags::SPI_MODE_2,
        SpiMode::Mode3 => SpiModeFlags::SPI_MODE_3,
    }
}

fn mode_from_bits(bits: u8) -> SpiMode {
    match bits & 0x03 {
        0x00 => SpiMode::Mode0,
        0x01 => SpiMode::Mode1,
        0x02 => SpiMode::Mode2,
        _ => SpiMode::Mode3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::descriptor_devnode;
    use lemnos_bus::SessionAccess;
    use lemnos_core::DeviceDescriptor;

    struct MockTransport {
        configuration: SpiConfiguration,
        last_write: Vec<u8>,
    }

    impl MockTransport {
        fn new() -> Self {
            Self {
                configuration: SpiConfiguration {
                    mode: SpiMode::Mode0,
                    max_frequency_hz: Some(1_000_000),
                    bits_per_word: Some(8),
                    bit_order: SpiBitOrder::MsbFirst,
                },
                last_write: Vec::new(),
            }
        }
    }

    impl SpiTransport for MockTransport {
        fn transfer(&mut self, write: &[u8]) -> BusResult<Vec<u8>> {
            self.last_write = write.to_vec();
            Ok(vec![0xAA; write.len()])
        }

        fn write(&mut self, bytes: &[u8]) -> BusResult<()> {
            self.last_write = bytes.to_vec();
            Ok(())
        }

        fn configure(&mut self, configuration: &SpiConfiguration) -> BusResult<()> {
            self.configuration = configuration.clone();
            Ok(())
        }

        fn configuration(&self) -> BusResult<SpiConfiguration> {
            Ok(self.configuration.clone())
        }
    }

    fn test_device() -> DeviceDescriptor {
        DeviceDescriptor::builder_for_kind("linux.spi.bus0.cs1", DeviceKind::SpiDevice)
            .expect("builder")
            .address(DeviceAddress::SpiDevice {
                bus: 0,
                chip_select: 1,
            })
            .property("devnode", "/dev/spidev0.1")
            .build()
            .expect("descriptor")
    }

    #[test]
    fn spi_session_round_trips_through_transport() {
        let device = test_device();
        let mut session = LinuxSpiSession::with_transport(
            device.clone(),
            SessionAccess::Exclusive,
            Box::new(MockTransport::new()),
        );

        let bytes = session.transfer(&[0x9F, 0x00]).expect("transfer");
        assert_eq!(bytes, vec![0xAA, 0xAA]);

        session.write(&[0xAB, 0xCD]).expect("write");
        session
            .configure(&SpiConfiguration {
                mode: SpiMode::Mode3,
                max_frequency_hz: Some(8_000_000),
                bits_per_word: Some(16),
                bit_order: SpiBitOrder::LsbFirst,
            })
            .expect("configure");

        assert_eq!(
            session.configuration().expect("configuration"),
            SpiConfiguration {
                mode: SpiMode::Mode3,
                max_frequency_hz: Some(8_000_000),
                bits_per_word: Some(16),
                bit_order: SpiBitOrder::LsbFirst,
            }
        );
        assert_eq!(descriptor_devnode(&session.device), Some("/dev/spidev0.1"));
    }

    #[test]
    fn spi_session_rejects_operations_after_close_and_new_session_can_reopen() {
        let device = test_device();
        let mut session = LinuxSpiSession::with_transport(
            device.clone(),
            SessionAccess::Exclusive,
            Box::new(MockTransport::new()),
        );
        session.close().expect("close");

        assert!(matches!(
            session.transfer(&[0x9F]),
            Err(BusError::SessionUnavailable { .. })
        ));
        assert!(matches!(
            session.write(&[0xAA]),
            Err(BusError::SessionUnavailable { .. })
        ));

        let reopened = LinuxSpiSession::with_transport(
            device,
            SessionAccess::Exclusive,
            Box::new(MockTransport::new()),
        );
        assert_eq!(reopened.metadata().state, SessionState::Idle);
    }

    #[test]
    fn spi_supports_descriptor_requires_typed_address() {
        let property_only = DeviceDescriptor::builder("linux.spi.bus0.cs1", InterfaceKind::Spi)
            .expect("builder")
            .kind(DeviceKind::SpiDevice)
            .property("devnode", "/dev/spidev0.1")
            .build()
            .expect("descriptor");
        assert!(!supports_descriptor(&property_only));
    }
}
