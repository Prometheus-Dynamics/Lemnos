use crate::LinuxPaths;
use crate::backend::BACKEND_NAME;
use crate::backend::LinuxTransportConfig;
use crate::transport;
use crate::util::{read_trimmed, wait_for_path};
use lemnos_bus::{
    BusError, BusResult, BusSession, PwmSession, SessionAccess, SessionMetadata, SessionState,
};
use lemnos_core::{DeviceDescriptor, DeviceKind, InterfaceKind, PwmConfiguration, PwmPolarity};
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) fn supports_descriptor(device: &DeviceDescriptor) -> bool {
    device.interface == InterfaceKind::Pwm
        && device.kind == DeviceKind::PwmChannel
        && transport::pwm_channel_address(device).is_some()
}

pub(crate) fn open_session(
    paths: &LinuxPaths,
    transport_config: &LinuxTransportConfig,
    device: &DeviceDescriptor,
    access: SessionAccess,
) -> BusResult<Box<dyn PwmSession>> {
    LinuxSysfsPwmSession::open(paths, transport_config, device, access)
        .map(|session| Box::new(session) as Box<dyn PwmSession>)
}

struct LinuxSysfsPwmSession {
    device: DeviceDescriptor,
    metadata: SessionMetadata,
    paths: LinuxPaths,
    chip_name: String,
    channel: u32,
    exported_by_session: bool,
}

impl LinuxSysfsPwmSession {
    fn open(
        paths: &LinuxPaths,
        transport_config: &LinuxTransportConfig,
        device: &DeviceDescriptor,
        access: SessionAccess,
    ) -> BusResult<Self> {
        let (chip_name, channel) =
            transport::pwm_channel_address(device).ok_or_else(|| BusError::UnsupportedDevice {
                backend: BACKEND_NAME.to_string(),
                device_id: device.id.clone(),
            })?;
        let channel_root = paths.pwm_channel_root(&chip_name, channel);
        let exported_by_session = ensure_exported(
            paths,
            transport_config,
            &chip_name,
            channel,
            &channel_root,
            device,
        )?;

        Ok(Self {
            device: device.clone(),
            metadata: SessionMetadata::new(BACKEND_NAME, access).with_state(SessionState::Idle),
            paths: paths.clone(),
            chip_name,
            channel,
            exported_by_session,
        })
    }

    fn channel_root(&self) -> PathBuf {
        self.paths.pwm_channel_root(&self.chip_name, self.channel)
    }

    fn file_path(&self, name: &str) -> PathBuf {
        self.channel_root().join(name)
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
                reason: format!("cannot perform '{operation}' on a closed PWM session"),
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
                    format!("required PWM attribute '{}' is missing", path.display()),
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

    fn write_enable(&self, enabled: bool, operation: &'static str) -> BusResult<()> {
        self.write_required_file(
            &self.file_path("enable"),
            if enabled { "1" } else { "0" },
            operation,
        )
    }

    fn write_period(&self, period_ns: u64, operation: &'static str) -> BusResult<()> {
        self.write_required_file(&self.file_path("period"), &period_ns.to_string(), operation)
    }

    fn write_duty_cycle(&self, duty_cycle_ns: u64, operation: &'static str) -> BusResult<()> {
        self.write_required_file(
            &self.file_path("duty_cycle"),
            &duty_cycle_ns.to_string(),
            operation,
        )
    }

    fn write_polarity(&self, polarity: PwmPolarity, operation: &'static str) -> BusResult<()> {
        self.write_required_file(
            &self.file_path("polarity"),
            match polarity {
                PwmPolarity::Normal => "normal",
                PwmPolarity::Inversed => "inversed",
            },
            operation,
        )
    }

    fn current_configuration(&self) -> BusResult<PwmConfiguration> {
        let period_ns = parse_u64(
            &self.read_required_file(&self.file_path("period"), "pwm.get_configuration")?,
            "pwm.get_configuration",
            self,
            "period",
        )?;
        let duty_cycle_ns = parse_u64(
            &self.read_required_file(&self.file_path("duty_cycle"), "pwm.get_configuration")?,
            "pwm.get_configuration",
            self,
            "duty_cycle",
        )?;
        let enabled = parse_enabled(
            &self.read_required_file(&self.file_path("enable"), "pwm.get_configuration")?,
            "pwm.get_configuration",
            self,
        )?;
        let polarity = parse_polarity(
            &self.read_required_file(&self.file_path("polarity"), "pwm.get_configuration")?,
            "pwm.get_configuration",
            self,
        )?;

        Ok(PwmConfiguration {
            period_ns,
            duty_cycle_ns,
            enabled,
            polarity,
        })
    }

    fn apply_timing(
        &mut self,
        current: &PwmConfiguration,
        target: &PwmConfiguration,
        operation: &'static str,
    ) -> BusResult<()> {
        if current.period_ns == target.period_ns && current.duty_cycle_ns == target.duty_cycle_ns {
            return Ok(());
        }

        let expand_period_first =
            target.period_ns > current.period_ns || target.duty_cycle_ns > current.period_ns;

        if expand_period_first {
            if current.period_ns != target.period_ns {
                self.write_period(target.period_ns, operation)?;
            }
            if current.duty_cycle_ns != target.duty_cycle_ns {
                self.write_duty_cycle(target.duty_cycle_ns, operation)?;
            }
        } else {
            if current.duty_cycle_ns != target.duty_cycle_ns {
                self.write_duty_cycle(target.duty_cycle_ns, operation)?;
            }
            if current.period_ns != target.period_ns {
                self.write_period(target.period_ns, operation)?;
            }
        }

        Ok(())
    }

    fn apply_configuration(
        &mut self,
        target: &PwmConfiguration,
        operation: &'static str,
    ) -> BusResult<()> {
        validate_configuration(self, target)?;
        let current = self.current_configuration()?;
        let needs_disable = current.enabled
            && (current.period_ns != target.period_ns
                || current.duty_cycle_ns != target.duty_cycle_ns
                || current.polarity != target.polarity
                || !target.enabled);

        if needs_disable {
            self.write_enable(false, operation)?;
        }

        self.apply_timing(&current, target, operation)?;

        if current.polarity != target.polarity {
            self.write_polarity(target.polarity, operation)?;
        }

        if needs_disable || current.enabled != target.enabled {
            self.write_enable(target.enabled, operation)?;
        }

        Ok(())
    }
}

impl BusSession for LinuxSysfsPwmSession {
    fn interface(&self) -> InterfaceKind {
        InterfaceKind::Pwm
    }

    fn device(&self) -> &DeviceDescriptor {
        &self.device
    }

    fn metadata(&self) -> &SessionMetadata {
        &self.metadata
    }

    fn close(&mut self) -> BusResult<()> {
        if self.exported_by_session {
            let unexport_path = self.paths.pwm_unexport_path(&self.chip_name);
            if unexport_path.exists() {
                fs::write(&unexport_path, self.channel.to_string()).map_err(|error| {
                    self.transport_failure(
                        "pwm.close",
                        format!(
                            "failed to unexport channel at '{}': {error}",
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

impl PwmSession for LinuxSysfsPwmSession {
    fn set_enabled(&mut self, enabled: bool) -> BusResult<()> {
        self.ensure_open("pwm.enable")?;
        if !self.metadata.access.can_write() {
            return Err(self.permission_denied("pwm.enable", "session access is read-only"));
        }
        self.run_call(|session| session.write_enable(enabled, "pwm.enable"))
    }

    fn set_period_ns(&mut self, period_ns: u64) -> BusResult<()> {
        self.ensure_open("pwm.set_period")?;
        if !self.metadata.access.can_write() {
            return Err(self.permission_denied("pwm.set_period", "session access is read-only"));
        }

        self.run_call(|session| {
            let mut target = session.current_configuration()?;
            target.period_ns = period_ns;
            session.apply_configuration(&target, "pwm.set_period")
        })
    }

    fn set_duty_cycle_ns(&mut self, duty_cycle_ns: u64) -> BusResult<()> {
        self.ensure_open("pwm.set_duty_cycle")?;
        if !self.metadata.access.can_write() {
            return Err(self.permission_denied("pwm.set_duty_cycle", "session access is read-only"));
        }

        self.run_call(|session| {
            let mut target = session.current_configuration()?;
            target.duty_cycle_ns = duty_cycle_ns;
            session.apply_configuration(&target, "pwm.set_duty_cycle")
        })
    }

    fn configure(&mut self, configuration: &PwmConfiguration) -> BusResult<()> {
        self.ensure_open("pwm.configure")?;
        if !self.metadata.access.can_write() {
            return Err(self.permission_denied(
                "pwm.configure",
                "session access does not allow configuration changes",
            ));
        }
        self.run_call(|session| session.apply_configuration(configuration, "pwm.configure"))
    }

    fn configuration(&self) -> BusResult<PwmConfiguration> {
        self.ensure_open("pwm.get_configuration")?;
        self.current_configuration()
    }
}

fn validate_configuration(
    session: &LinuxSysfsPwmSession,
    configuration: &PwmConfiguration,
) -> BusResult<()> {
    if configuration.period_ns == 0 {
        return Err(session.invalid_configuration("PWM period must be greater than zero"));
    }
    if configuration.duty_cycle_ns > configuration.period_ns {
        return Err(session.invalid_configuration("PWM duty cycle must not exceed the period"));
    }
    Ok(())
}

fn ensure_exported(
    paths: &LinuxPaths,
    transport_config: &LinuxTransportConfig,
    chip_name: &str,
    channel: u32,
    channel_root: &Path,
    device: &DeviceDescriptor,
) -> BusResult<bool> {
    if channel_root.exists() {
        return Ok(false);
    }

    let export_path = paths.pwm_export_path(chip_name);
    if !export_path.exists() {
        return Err(BusError::TransportFailure {
            device_id: device.id.clone(),
            operation: "open",
            reason: format!(
                "PWM export path '{}' is not present and channel '{}' is not exported",
                export_path.display(),
                channel_root.display()
            ),
        });
    }

    fs::write(&export_path, channel.to_string()).map_err(|error| BusError::TransportFailure {
        device_id: device.id.clone(),
        operation: "open",
        reason: format!(
            "failed to export PWM channel {channel} using '{}': {error}",
            export_path.display()
        ),
    })?;

    if wait_for_path(
        channel_root,
        transport_config.sysfs_export_retries,
        transport_config.sysfs_export_delay_ms,
    ) {
        return Ok(true);
    }

    Err(BusError::TransportFailure {
        device_id: device.id.clone(),
        operation: "open",
        reason: format!(
            "PWM channel {channel} did not appear at '{}' after export",
            channel_root.display()
        ),
    })
}

fn parse_u64(
    raw: &str,
    operation: &'static str,
    session: &LinuxSysfsPwmSession,
    field: &str,
) -> BusResult<u64> {
    raw.parse::<u64>().map_err(|error| {
        session.transport_failure(
            operation,
            format!("failed to parse PWM {field} value '{raw}': {error}"),
        )
    })
}

fn parse_enabled(
    raw: &str,
    operation: &'static str,
    session: &LinuxSysfsPwmSession,
) -> BusResult<bool> {
    match raw {
        "0" => Ok(false),
        "1" => Ok(true),
        other => {
            Err(session
                .transport_failure(operation, format!("unexpected PWM enable value '{other}'")))
        }
    }
}

fn parse_polarity(
    raw: &str,
    operation: &'static str,
    session: &LinuxSysfsPwmSession,
) -> BusResult<PwmPolarity> {
    match raw {
        "normal" => Ok(PwmPolarity::Normal),
        "inversed" | "inverse" | "inverted" => Ok(PwmPolarity::Inversed),
        other => Err(session.transport_failure(
            operation,
            format!("unexpected PWM polarity value '{other}'"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::supports_descriptor;
    use lemnos_core::{DeviceAddress, DeviceDescriptor, DeviceKind, InterfaceKind};

    #[test]
    fn pwm_supports_descriptor_uses_typed_address_without_property_fallbacks() {
        let with_address =
            DeviceDescriptor::builder_for_kind("pwmchip2-pwm1", DeviceKind::PwmChannel)
                .expect("builder")
                .address(DeviceAddress::PwmChannel {
                    chip_name: "pwmchip2".into(),
                    channel: 1,
                })
                .build()
                .expect("descriptor");
        assert!(supports_descriptor(&with_address));

        let property_only = DeviceDescriptor::builder("pwmchip2-pwm1", InterfaceKind::Pwm)
            .expect("builder")
            .kind(DeviceKind::PwmChannel)
            .property("chip_name", "pwmchip2")
            .property("channel", 1_u64)
            .build()
            .expect("descriptor");
        assert!(!supports_descriptor(&property_only));
    }
}
