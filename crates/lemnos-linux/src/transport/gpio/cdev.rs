//! GPIO sessions over the GPIO character device, uAPI v2 (`crate::hal`).

use super::{gpio_line_address, resolve_chip_devnode};
use crate::LinuxPaths;
use crate::backend::BACKEND_NAME;
use crate::hal::{
    EdgeKind, GpioChip, GpioLine, GpioLineInfo, LineBias, LineDirection, LineDrive, LineEdge,
    LineSettings,
};
use lemnos_bus::{
    BusError, BusResult, BusSession, GpioEdgeEvent, GpioEdgeStreamSession, GpioSession,
    SessionAccess, SessionMetadata, SessionState, StreamSession,
};
use lemnos_core::{
    DeviceDescriptor, GpioBias, GpioDirection, GpioDrive, GpioEdge, GpioLevel,
    GpioLineConfiguration, InterfaceKind, TimestampMs,
};
use std::fs;
use std::os::unix::fs::FileTypeExt;
use std::time::Duration;

const CONSUMER: &str = "lemnos";

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
    LinuxCdevGpioSession::open(paths, device, access, false)
        .map(|session| Box::new(session) as Box<dyn GpioSession>)
}

pub(super) fn open_edge_stream(
    paths: &LinuxPaths,
    device: &DeviceDescriptor,
    access: SessionAccess,
) -> BusResult<Box<dyn GpioEdgeStreamSession>> {
    LinuxCdevGpioSession::open(paths, device, access, true)
        .map(|session| Box::new(session) as Box<dyn GpioEdgeStreamSession>)
}

struct LinuxCdevGpioSession {
    device: DeviceDescriptor,
    metadata: SessionMetadata,
    configuration: GpioLineConfiguration,
    line: GpioLine,
}

impl LinuxCdevGpioSession {
    fn open(
        paths: &LinuxPaths,
        device: &DeviceDescriptor,
        access: SessionAccess,
        edges: bool,
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

        let chip = GpioChip::open(&chip_devnode).map_err(|error| {
            open_failure(
                device,
                format!("failed to open GPIO chip '{chip_devnode}': {error}"),
                &error,
            )
        })?;
        let info = chip.line_info(offset).map_err(|error| {
            open_failure(
                device,
                format!("failed to query GPIO line {offset} via '{chip_devnode}': {error}"),
                &error,
            )
        })?;
        let mut configuration = configuration_from_info(&info);
        // Take the line over without changing it (no glitch on outputs), or
        // as an input with events on both edges for a stream.
        let settings = if edges {
            configuration.direction = GpioDirection::Input;
            configuration.drive = None;
            configuration.edge = Some(GpioEdge::Both);
            settings_from_configuration(&configuration)
        } else {
            LineSettings {
                direction: LineDirection::AsIs,
                active_low: info.settings.active_low,
                ..LineSettings::default()
            }
        };
        let line = chip
            .request_line(CONSUMER, offset, settings)
            .map_err(|error| {
                open_failure(
                    device,
                    format!("failed to request GPIO line {offset} on '{chip_devnode}': {error}"),
                    &error,
                )
            })?;

        Ok(Self {
            device: device.clone(),
            metadata: SessionMetadata::new(BACKEND_NAME, access).with_state(SessionState::Idle),
            configuration,
            line,
        })
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
            let high = session.line.get().map_err(|error| {
                session.transport_failure("gpio.read", format!("GPIO cdev read failed: {error}"))
            })?;
            Ok(if high {
                GpioLevel::High
            } else {
                GpioLevel::Low
            })
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
            session.line.set(level == GpioLevel::High).map_err(|error| {
                session.transport_failure("gpio.write", format!("GPIO cdev write failed: {error}"))
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
        let settings = settings_from_configuration(configuration);
        settings
            .flags()
            .map_err(|error| self.invalid_configuration(error.to_string()))?;

        self.run_call(|session| {
            session.line.reconfigure(settings).map_err(|error| {
                session.transport_failure(
                    "gpio.configure",
                    format!("GPIO cdev reconfiguration failed: {error}"),
                )
            })?;
            session.configuration = configuration.clone();
            Ok(())
        })
    }

    fn configuration(&self) -> BusResult<GpioLineConfiguration> {
        self.ensure_open("gpio.get_configuration")?;
        Ok(self.configuration.clone())
    }
}

impl StreamSession for LinuxCdevGpioSession {
    type Event = GpioEdgeEvent;

    fn poll_events(
        &mut self,
        max_events: u32,
        timeout_ms: Option<u32>,
    ) -> BusResult<Vec<GpioEdgeEvent>> {
        self.ensure_open("gpio.poll_events")?;
        if self.configuration.edge.is_none() {
            return Err(self.invalid_configuration("line is not configured for edge events"));
        }
        let mut events = Vec::new();
        let mut timeout = timeout_ms.map(|ms| Duration::from_millis(u64::from(ms)));
        while events.len() < max_events as usize {
            let next = self.line.wait_event(timeout).map_err(|error| {
                self.transport_failure(
                    "gpio.poll_events",
                    format!("GPIO cdev event read failed: {error}"),
                )
            })?;
            let Some(event) = next else { break };
            events.push(GpioEdgeEvent {
                edge: match event.kind {
                    EdgeKind::Rising => GpioEdge::Rising,
                    EdgeKind::Falling => GpioEdge::Falling,
                },
                level: Some(match event.kind {
                    EdgeKind::Rising => GpioLevel::High,
                    EdgeKind::Falling => GpioLevel::Low,
                }),
                sequence: u64::from(event.line_seqno),
                observed_at: Some(TimestampMs::new(event.timestamp_ns / 1_000_000)),
            });
            // After the first event, only drain what is already queued.
            timeout = Some(Duration::ZERO);
        }
        self.metadata.touch_now();
        Ok(events)
    }
}

fn settings_from_configuration(configuration: &GpioLineConfiguration) -> LineSettings {
    let output = configuration.direction == GpioDirection::Output;
    LineSettings {
        direction: if output {
            LineDirection::Output
        } else {
            LineDirection::Input
        },
        active_low: configuration.active_low,
        bias: match configuration.bias {
            None => LineBias::AsIs,
            Some(GpioBias::Disabled) => LineBias::Disabled,
            Some(GpioBias::PullUp) => LineBias::PullUp,
            Some(GpioBias::PullDown) => LineBias::PullDown,
        },
        drive: match configuration.drive {
            Some(GpioDrive::OpenDrain) if output => LineDrive::OpenDrain,
            Some(GpioDrive::OpenSource) if output => LineDrive::OpenSource,
            _ => LineDrive::PushPull,
        },
        edge: match configuration.edge {
            None => LineEdge::None,
            Some(GpioEdge::Rising) => LineEdge::Rising,
            Some(GpioEdge::Falling) => LineEdge::Falling,
            Some(GpioEdge::Both) => LineEdge::Both,
        },
        debounce_us: configuration.debounce_us,
        output_value: configuration.initial_level == Some(GpioLevel::High),
    }
}

fn configuration_from_info(info: &GpioLineInfo) -> GpioLineConfiguration {
    let s = info.settings;
    let direction = match s.direction {
        LineDirection::Output => GpioDirection::Output,
        LineDirection::Input | LineDirection::AsIs => GpioDirection::Input,
    };
    GpioLineConfiguration {
        direction,
        active_low: s.active_low,
        bias: match s.bias {
            LineBias::AsIs => None,
            LineBias::Disabled => Some(GpioBias::Disabled),
            LineBias::PullUp => Some(GpioBias::PullUp),
            LineBias::PullDown => Some(GpioBias::PullDown),
        },
        drive: (direction == GpioDirection::Output).then_some(match s.drive {
            LineDrive::PushPull => GpioDrive::PushPull,
            LineDrive::OpenDrain => GpioDrive::OpenDrain,
            LineDrive::OpenSource => GpioDrive::OpenSource,
        }),
        edge: match s.edge {
            LineEdge::None => None,
            LineEdge::Rising => Some(GpioEdge::Rising),
            LineEdge::Falling => Some(GpioEdge::Falling),
            LineEdge::Both => Some(GpioEdge::Both),
        },
        debounce_us: s.debounce_us,
        initial_level: None,
    }
}

fn open_failure(device: &DeviceDescriptor, reason: String, error: &std::io::Error) -> BusError {
    match error.raw_os_error() {
        Some(1 | 13) => BusError::PermissionDenied {
            device_id: device.id.clone(),
            operation: "gpio.open",
            reason,
        },
        Some(16) => BusError::AccessConflict {
            device_id: device.id.clone(),
            reason,
        },
        _ => BusError::TransportFailure {
            device_id: device.id.clone(),
            operation: "gpio.open",
            reason,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configurations_round_trip_through_line_settings() {
        let configuration = GpioLineConfiguration {
            direction: GpioDirection::Input,
            active_low: true,
            bias: Some(GpioBias::PullUp),
            drive: None,
            edge: Some(GpioEdge::Falling),
            debounce_us: Some(2000),
            initial_level: None,
        };
        let settings = settings_from_configuration(&configuration);
        assert!(settings.flags().is_ok());
        let info = GpioLineInfo {
            offset: 3,
            name: String::new(),
            consumer: String::new(),
            used: false,
            settings,
        };
        assert_eq!(configuration_from_info(&info), configuration);
        let output = GpioLineConfiguration {
            direction: GpioDirection::Output,
            active_low: false,
            bias: None,
            drive: Some(GpioDrive::OpenDrain),
            edge: None,
            debounce_us: None,
            initial_level: Some(GpioLevel::High),
        };
        let settings = settings_from_configuration(&output);
        assert!(settings.output_value);
        assert_eq!(settings.drive, LineDrive::OpenDrain);
    }
}
