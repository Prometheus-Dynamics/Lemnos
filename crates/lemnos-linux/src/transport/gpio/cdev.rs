use super::{gpio_line_address, resolve_chip_devnode};
use crate::LinuxPaths;
use crate::backend::BACKEND_NAME;
use gpio_cdev::{Chip, LineDirection, LineHandle, LineInfo, LineRequestFlags};
use lemnos_bus::{
    BusError, BusResult, BusSession, GpioSession, SessionAccess, SessionMetadata, SessionState,
};
use lemnos_core::{
    DeviceDescriptor, GpioDirection, GpioDrive, GpioLevel, GpioLineConfiguration, InterfaceKind,
};
use std::fs;
use std::os::unix::fs::FileTypeExt;

pub(super) fn supports_descriptor(device: &DeviceDescriptor) -> bool {
    gpio_line_address(device).is_some()
}

pub(super) fn can_use_transport(paths: &LinuxPaths, device: &DeviceDescriptor) -> bool {
    let Some(devnode) = resolve_chip_devnode(paths, device) else {
        return false;
    };

    fs::metadata(devnode)
        .map(|metadata| metadata.file_type().is_char_device())
        .unwrap_or(false)
}

pub(super) fn open_session(
    paths: &LinuxPaths,
    device: &DeviceDescriptor,
    access: SessionAccess,
) -> BusResult<Box<dyn GpioSession>> {
    LinuxCdevGpioSession::open(paths, device, access)
        .map(|session| Box::new(session) as Box<dyn GpioSession>)
}

struct LinuxCdevGpioSession {
    device: DeviceDescriptor,
    metadata: SessionMetadata,
    chip_devnode: String,
    offset: u32,
    configuration: GpioLineConfiguration,
    handle: LineHandle,
}

impl LinuxCdevGpioSession {
    fn open(
        paths: &LinuxPaths,
        device: &DeviceDescriptor,
        access: SessionAccess,
    ) -> BusResult<Self> {
        let chip_devnode =
            resolve_chip_devnode(paths, device).ok_or_else(|| BusError::UnsupportedDevice {
                backend: BACKEND_NAME.to_string(),
                device_id: device.id.clone(),
            })?;
        let (_, offset) = gpio_line_address(device).ok_or_else(|| BusError::UnsupportedDevice {
            backend: BACKEND_NAME.to_string(),
            device_id: device.id.clone(),
        })?;

        let line = open_line(device, &chip_devnode, offset, "gpio.open")?;
        let configuration = configuration_from_info(
            device,
            &line.info().map_err(|error| {
                transport_failure(
                    device,
                    "gpio.open",
                    format!("failed to query GPIO line info via '{chip_devnode}': {error}"),
                )
            })?,
        )?;
        let handle = request_handle(device, &line, &configuration, "gpio.open")?;

        Ok(Self {
            device: device.clone(),
            metadata: SessionMetadata::new(BACKEND_NAME, access).with_state(SessionState::Idle),
            chip_devnode,
            offset,
            configuration,
            handle,
        })
    }

    fn transport_failure(&self, operation: &'static str, reason: impl Into<String>) -> BusError {
        transport_failure(&self.device, operation, reason)
    }

    fn permission_denied(&self, operation: &'static str, reason: impl Into<String>) -> BusError {
        BusError::PermissionDenied {
            device_id: self.device.id.clone(),
            operation,
            reason: reason.into(),
        }
    }

    fn invalid_configuration(&self, reason: impl Into<String>) -> BusError {
        BusError::InvalidConfiguration {
            device_id: self.device.id.clone(),
            reason: reason.into(),
        }
    }

    fn ensure_open(&self, operation: &'static str) -> BusResult<()> {
        if self.metadata.state == SessionState::Closed {
            return Err(BusError::SessionUnavailable {
                device_id: self.device.id.clone(),
                reason: format!("cannot perform '{operation}' on a closed GPIO session"),
            });
        }
        Ok(())
    }

    fn run_call<T>(&mut self, call: impl FnOnce(&mut Self) -> BusResult<T>) -> BusResult<T> {
        self.metadata.begin_call();
        let result = call(self);
        self.metadata.finish_call(&result);
        result
    }

    fn line(&self, operation: &'static str) -> BusResult<gpio_cdev::Line> {
        open_line(&self.device, &self.chip_devnode, self.offset, operation)
    }
}

impl BusSession for LinuxCdevGpioSession {
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
        self.metadata.mark_closed();
        Ok(())
    }
}

impl GpioSession for LinuxCdevGpioSession {
    fn read_level(&mut self) -> BusResult<GpioLevel> {
        self.ensure_open("gpio.read")?;
        self.run_call(|session| {
            match session.handle.get_value().map_err(|error| {
                session.transport_failure("gpio.read", format!("GPIO cdev read failed: {error}"))
            })? {
                0 => Ok(GpioLevel::Low),
                1 => Ok(GpioLevel::High),
                other => Err(session.transport_failure(
                    "gpio.read",
                    format!("unexpected GPIO cdev value '{other}'"),
                )),
            }
        })
    }

    fn write_level(&mut self, level: GpioLevel) -> BusResult<()> {
        self.ensure_open("gpio.write")?;
        if !self.metadata.access.can_write() {
            return Err(self.permission_denied("gpio.write", "session access is read-only"));
        }

        if self.configuration.direction != GpioDirection::Output {
            return Err(self.permission_denied("gpio.write", "line is not configured for output"));
        }

        self.run_call(|session| {
            session
                .handle
                .set_value(match level {
                    GpioLevel::Low => 0,
                    GpioLevel::High => 1,
                })
                .map_err(|error| {
                    session
                        .transport_failure("gpio.write", format!("GPIO cdev write failed: {error}"))
                })
        })
    }

    fn configure_line(&mut self, configuration: &GpioLineConfiguration) -> BusResult<()> {
        self.ensure_open("gpio.configure")?;
        if !self.metadata.access.can_write() {
            return Err(self.permission_denied(
                "gpio.configure",
                "session access does not allow configuration changes",
            ));
        }

        if configuration.direction == GpioDirection::Input && configuration.initial_level.is_some()
        {
            return Err(
                self.invalid_configuration("input lines cannot set an initial output level")
            );
        }

        if configuration.bias.is_some() {
            return Err(self.invalid_configuration(
                "linux GPIO cdev transport does not yet support bias configuration",
            ));
        }

        if configuration.debounce_us.is_some() {
            return Err(self.invalid_configuration(
                "linux GPIO cdev transport does not yet support debounce configuration",
            ));
        }

        if configuration.edge.is_some() {
            return Err(self.invalid_configuration(
                "linux GPIO cdev transport does not yet support edge configuration",
            ));
        }

        self.run_call(|session| {
            let line = session.line("gpio.configure")?;
            let handle = request_handle(&session.device, &line, configuration, "gpio.configure")?;
            session.handle = handle;
            session.configuration = configuration.clone();
            Ok(())
        })
    }

    fn configuration(&self) -> BusResult<GpioLineConfiguration> {
        self.ensure_open("gpio.get_configuration")?;
        Ok(self.configuration.clone())
    }
}

fn open_line(
    device: &DeviceDescriptor,
    chip_devnode: &str,
    offset: u32,
    operation: &'static str,
) -> BusResult<gpio_cdev::Line> {
    let mut chip = Chip::new(chip_devnode).map_err(|error| {
        transport_failure(
            device,
            operation,
            format!("failed to open GPIO chip '{chip_devnode}': {error}"),
        )
    })?;

    chip.get_line(offset).map_err(|error| {
        transport_failure(
            device,
            operation,
            format!("failed to access line offset {offset} on '{chip_devnode}': {error}"),
        )
    })
}

fn request_handle(
    device: &DeviceDescriptor,
    line: &gpio_cdev::Line,
    configuration: &GpioLineConfiguration,
    operation: &'static str,
) -> BusResult<LineHandle> {
    let mut flags = match configuration.direction {
        GpioDirection::Input => LineRequestFlags::INPUT,
        GpioDirection::Output => LineRequestFlags::OUTPUT,
    };

    if configuration.active_low {
        flags |= LineRequestFlags::ACTIVE_LOW;
    }

    match configuration.drive {
        Some(GpioDrive::OpenDrain) => flags |= LineRequestFlags::OPEN_DRAIN,
        Some(GpioDrive::OpenSource) => flags |= LineRequestFlags::OPEN_SOURCE,
        Some(GpioDrive::PushPull) | None => {}
    }

    let initial = match configuration.initial_level {
        Some(GpioLevel::High) => 1,
        Some(GpioLevel::Low) | None => 0,
    };

    line.request(flags, initial, "lemnos-linux")
        .map_err(|error| {
            transport_failure(
                device,
                operation,
                format!("failed to request GPIO line handle: {error}"),
            )
        })
}

fn configuration_from_info(
    device: &DeviceDescriptor,
    info: &LineInfo,
) -> BusResult<GpioLineConfiguration> {
    let direction = match info.direction() {
        LineDirection::In => GpioDirection::Input,
        LineDirection::Out => GpioDirection::Output,
    };

    let drive = if direction == GpioDirection::Output {
        if info.is_open_drain() {
            Some(GpioDrive::OpenDrain)
        } else if info.is_open_source() {
            Some(GpioDrive::OpenSource)
        } else {
            Some(GpioDrive::PushPull)
        }
    } else {
        None
    };

    if info.is_open_drain() && info.is_open_source() {
        return Err(BusError::InvalidConfiguration {
            device_id: device.id.clone(),
            reason: "kernel reported both open-drain and open-source on one GPIO line".into(),
        });
    }

    Ok(GpioLineConfiguration {
        direction,
        active_low: info.is_active_low(),
        bias: None,
        drive,
        edge: None,
        debounce_us: None,
        initial_level: None,
    })
}

fn transport_failure(
    device: &DeviceDescriptor,
    operation: &'static str,
    reason: impl Into<String>,
) -> BusError {
    BusError::TransportFailure {
        device_id: device.id.clone(),
        operation,
        reason: reason.into(),
    }
}
