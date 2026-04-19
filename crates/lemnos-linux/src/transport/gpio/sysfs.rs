use super::{gpio_line_address, resolve_global_line};
use crate::LinuxPaths;
use crate::backend::BACKEND_NAME;
use crate::backend::LinuxTransportConfig;
use crate::util::{read_trimmed, wait_for_path};
use lemnos_bus::{
    BusError, BusResult, BusSession, GpioSession, SessionAccess, SessionMetadata, SessionState,
};
use lemnos_core::{
    DeviceDescriptor, GpioDirection, GpioDrive, GpioEdge, GpioLevel, GpioLineConfiguration,
    InterfaceKind,
};
use std::fs;
use std::path::{Path, PathBuf};

pub(super) fn supports_descriptor(device: &DeviceDescriptor) -> bool {
    gpio_line_address(device).is_some()
}

pub(super) fn can_use_transport(paths: &LinuxPaths, device: &DeviceDescriptor) -> bool {
    resolve_global_line(paths, device).is_some()
}

pub(super) fn open_session(
    paths: &LinuxPaths,
    transport_config: &LinuxTransportConfig,
    device: &DeviceDescriptor,
    access: SessionAccess,
) -> BusResult<Box<dyn GpioSession>> {
    LinuxSysfsGpioSession::open(paths, transport_config, device, access)
        .map(|session| Box::new(session) as Box<dyn GpioSession>)
}

struct LinuxSysfsGpioSession {
    device: DeviceDescriptor,
    metadata: SessionMetadata,
    paths: LinuxPaths,
    global_line: u32,
    exported_by_session: bool,
}

impl LinuxSysfsGpioSession {
    fn open(
        paths: &LinuxPaths,
        transport_config: &LinuxTransportConfig,
        device: &DeviceDescriptor,
        access: SessionAccess,
    ) -> BusResult<Self> {
        let global_line =
            resolve_global_line(paths, device).ok_or_else(|| BusError::UnsupportedDevice {
                backend: BACKEND_NAME.to_string(),
                device_id: device.id.clone(),
            })?;
        let line_root = paths.gpio_line_root(global_line);
        let exported_by_session =
            ensure_exported(paths, transport_config, global_line, &line_root, device)?;

        Ok(Self {
            device: device.clone(),
            metadata: SessionMetadata::new(BACKEND_NAME, access).with_state(SessionState::Idle),
            paths: paths.clone(),
            global_line,
            exported_by_session,
        })
    }

    fn line_root(&self) -> PathBuf {
        self.paths.gpio_line_root(self.global_line)
    }

    fn file_path(&self, name: &str) -> PathBuf {
        self.line_root().join(name)
    }

    fn transport_failure(&self, operation: &'static str, reason: impl Into<String>) -> BusError {
        BusError::TransportFailure {
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

    fn read_required_file(&self, path: &Path, operation: &'static str) -> BusResult<String> {
        read_trimmed(path)
            .map_err(|error| {
                self.transport_failure(
                    operation,
                    format!("failed to read '{}': {error}", path.display()),
                )
            })?
            .ok_or_else(|| {
                self.transport_failure(
                    operation,
                    format!("required GPIO attribute '{}' is missing", path.display()),
                )
            })
    }

    fn write_required_file(
        &self,
        path: &Path,
        value: &str,
        operation: &'static str,
    ) -> BusResult<()> {
        fs::write(path, value).map_err(|error| {
            self.transport_failure(
                operation,
                format!(
                    "failed to write '{}' with value '{value}': {error}",
                    path.display()
                ),
            )
        })
    }

    fn current_configuration(&self) -> BusResult<GpioLineConfiguration> {
        let direction = parse_direction(
            &self.read_required_file(&self.file_path("direction"), "gpio.get_configuration")?,
        )
        .map_err(|reason| self.transport_failure("gpio.get_configuration", reason))?;

        let active_low = match read_trimmed(&self.file_path("active_low")).map_err(|error| {
            self.transport_failure(
                "gpio.get_configuration",
                format!(
                    "failed to read '{}': {error}",
                    self.file_path("active_low").display()
                ),
            )
        })? {
            Some(value) => parse_active_low(&value)
                .map_err(|reason| self.transport_failure("gpio.get_configuration", reason))?,
            None => false,
        };

        let edge = match read_trimmed(&self.file_path("edge")).map_err(|error| {
            self.transport_failure(
                "gpio.get_configuration",
                format!(
                    "failed to read '{}': {error}",
                    self.file_path("edge").display()
                ),
            )
        })? {
            Some(value) => parse_edge(&value)
                .map_err(|reason| self.transport_failure("gpio.get_configuration", reason))?,
            None => None,
        };

        Ok(GpioLineConfiguration {
            direction,
            active_low,
            bias: None,
            drive: None,
            edge,
            debounce_us: None,
            initial_level: None,
        })
    }
}

impl BusSession for LinuxSysfsGpioSession {
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
        if self.exported_by_session {
            let unexport_path = self.paths.gpio_unexport_path();
            if unexport_path.exists() {
                fs::write(&unexport_path, self.global_line.to_string()).map_err(|error| {
                    self.transport_failure(
                        "gpio.close",
                        format!(
                            "failed to unexport line at '{}': {error}",
                            unexport_path.display()
                        ),
                    )
                })?;
            }
        }

        self.metadata.mark_closed();
        Ok(())
    }
}

impl GpioSession for LinuxSysfsGpioSession {
    fn read_level(&mut self) -> BusResult<GpioLevel> {
        self.ensure_open("gpio.read")?;
        self.run_call(|session| {
            let raw = session.read_required_file(&session.file_path("value"), "gpio.read")?;
            match raw.as_str() {
                "0" => Ok(GpioLevel::Low),
                "1" => Ok(GpioLevel::High),
                other => Err(session
                    .transport_failure("gpio.read", format!("unexpected GPIO value '{other}'"))),
            }
        })
    }

    fn write_level(&mut self, level: GpioLevel) -> BusResult<()> {
        self.ensure_open("gpio.write")?;
        if !self.metadata.access.can_write() {
            return Err(self.permission_denied("gpio.write", "session access is read-only"));
        }

        self.run_call(|session| {
            if session.current_configuration()?.direction != GpioDirection::Output {
                return Err(
                    session.permission_denied("gpio.write", "line is not configured for output")
                );
            }

            session.write_required_file(
                &session.file_path("value"),
                match level {
                    GpioLevel::Low => "0",
                    GpioLevel::High => "1",
                },
                "gpio.write",
            )
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
                "linux GPIO sysfs transport does not support bias configuration",
            ));
        }

        if configuration.debounce_us.is_some() {
            return Err(self.invalid_configuration(
                "linux GPIO sysfs transport does not support debounce configuration",
            ));
        }

        if matches!(
            configuration.drive,
            Some(GpioDrive::OpenDrain | GpioDrive::OpenSource)
        ) {
            return Err(self.invalid_configuration(
                "linux GPIO sysfs transport does not support open-drain/open-source drive modes",
            ));
        }

        if configuration.direction == GpioDirection::Output && configuration.edge.is_some() {
            return Err(self.invalid_configuration("edge detection is only valid for input lines"));
        }

        self.run_call(|session| {
            let active_low_path = session.file_path("active_low");
            if active_low_path.exists() {
                session.write_required_file(
                    &active_low_path,
                    if configuration.active_low { "1" } else { "0" },
                    "gpio.configure",
                )?;
            } else if configuration.active_low {
                return Err(session.invalid_configuration(
                    "linux GPIO sysfs transport does not expose active_low for this line",
                ));
            }

            let direction_value = match configuration.direction {
                GpioDirection::Input => "in",
                GpioDirection::Output => match configuration.initial_level {
                    Some(GpioLevel::Low) => "low",
                    Some(GpioLevel::High) => "high",
                    None => "out",
                },
            };
            session.write_required_file(
                &session.file_path("direction"),
                direction_value,
                "gpio.configure",
            )?;

            let edge_path = session.file_path("edge");
            if edge_path.exists() {
                let edge = match configuration.edge {
                    None => "none",
                    Some(GpioEdge::Rising) => "rising",
                    Some(GpioEdge::Falling) => "falling",
                    Some(GpioEdge::Both) => "both",
                };
                session.write_required_file(&edge_path, edge, "gpio.configure")?;
            } else if configuration.edge.is_some() {
                return Err(session.invalid_configuration(
                    "linux GPIO sysfs transport does not expose edge configuration for this line",
                ));
            }

            Ok(())
        })
    }

    fn configuration(&self) -> BusResult<GpioLineConfiguration> {
        self.ensure_open("gpio.get_configuration")?;
        self.current_configuration()
    }
}

fn ensure_exported(
    paths: &LinuxPaths,
    transport_config: &LinuxTransportConfig,
    global_line: u32,
    line_root: &Path,
    device: &DeviceDescriptor,
) -> BusResult<bool> {
    if line_root.exists() {
        return Ok(false);
    }

    let export_path = paths.gpio_export_path();
    if !export_path.exists() {
        return Err(BusError::SessionUnavailable {
            device_id: device.id.clone(),
            reason: format!(
                "GPIO line '{}' is not exported and '{}' is unavailable",
                line_root.display(),
                export_path.display()
            ),
        });
    }

    fs::write(&export_path, global_line.to_string()).map_err(|error| {
        BusError::TransportFailure {
            device_id: device.id.clone(),
            operation: "gpio.open",
            reason: format!(
                "failed to export GPIO line {global_line} via '{}': {error}",
                export_path.display()
            ),
        }
    })?;

    if wait_for_path(
        line_root,
        transport_config.sysfs_export_retries,
        transport_config.sysfs_export_delay_ms,
    ) {
        return Ok(true);
    }

    Err(BusError::SessionUnavailable {
        device_id: device.id.clone(),
        reason: format!(
            "GPIO line {global_line} did not appear at '{}' after export",
            line_root.display()
        ),
    })
}

fn parse_direction(value: &str) -> Result<GpioDirection, String> {
    match value {
        "in" => Ok(GpioDirection::Input),
        "out" | "low" | "high" => Ok(GpioDirection::Output),
        other => Err(format!("unexpected GPIO direction '{other}'")),
    }
}

fn parse_active_low(value: &str) -> Result<bool, String> {
    match value {
        "0" => Ok(false),
        "1" => Ok(true),
        other => Err(format!("unexpected GPIO active_low value '{other}'")),
    }
}

fn parse_edge(value: &str) -> Result<Option<GpioEdge>, String> {
    match value {
        "none" => Ok(None),
        "rising" => Ok(Some(GpioEdge::Rising)),
        "falling" => Ok(Some(GpioEdge::Falling)),
        "both" => Ok(Some(GpioEdge::Both)),
        other => Err(format!("unexpected GPIO edge value '{other}'")),
    }
}
