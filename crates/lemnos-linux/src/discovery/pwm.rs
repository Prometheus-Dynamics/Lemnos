use crate::LinuxPaths;
use crate::util::{file_name, parse_prefixed_u32, read_dir_sorted, read_u32};
use lemnos_core::{
    CapabilityAccess, CapabilityDescriptor, DeviceAddress, DeviceDescriptor, DeviceId, DeviceKind,
    DeviceLink, DeviceRelation, InterfaceKind,
};
use lemnos_discovery::{DiscoveryContext, DiscoveryError, DiscoveryProbe, ProbeDiscovery};
use std::path::Path;

const INTERFACES: [InterfaceKind; 1] = [InterfaceKind::Pwm];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PwmDiscoveryProbe {
    paths: LinuxPaths,
}

impl PwmDiscoveryProbe {
    pub fn new(paths: LinuxPaths) -> Self {
        Self { paths }
    }
}

impl DiscoveryProbe for PwmDiscoveryProbe {
    fn name(&self) -> &'static str {
        "linux-pwm"
    }

    fn interfaces(&self) -> &'static [InterfaceKind] {
        &INTERFACES
    }

    fn discover(&self, _context: &DiscoveryContext) -> Result<ProbeDiscovery, DiscoveryError> {
        let pwm_root = self.paths.pwm_class_root();
        let mut discovery = ProbeDiscovery::default();

        if !pwm_root.exists() {
            discovery.notes.push(format!(
                "PWM sysfs root '{}' is not present",
                pwm_root.display()
            ));
            return Ok(discovery);
        }

        let entries = read_dir_sorted(&pwm_root)
            .map_err(|error| probe_failed(self.name(), &pwm_root, "enumerate PWM chips", error))?;

        for chip_path in entries {
            let Some(chip_name) = file_name(&chip_path) else {
                continue;
            };
            if !chip_name.starts_with("pwmchip") {
                continue;
            }

            let chip_data = load_chip_data(self.name(), &chip_path, chip_name)?;
            let chip = match build_chip_descriptor(&chip_data) {
                Ok(device) => device,
                Err(note) => {
                    discovery.notes.push(note);
                    continue;
                }
            };
            let chip_id = chip.id.clone();
            discovery.devices.push(chip);

            for channel in 0..chip_data.channel_count {
                match build_channel_descriptor(&chip_data, &chip_id, channel) {
                    Ok(device) => discovery.devices.push(device),
                    Err(note) => discovery.notes.push(note),
                }
            }
        }

        Ok(discovery)
    }
}

#[derive(Debug, Clone)]
struct PwmChipData {
    chip_name: String,
    chip_index: Option<u32>,
    chip_path: String,
    channel_count: u32,
}

fn load_chip_data(
    probe: &str,
    chip_path: &Path,
    chip_name: &str,
) -> Result<PwmChipData, DiscoveryError> {
    let channel_count = read_u32(&chip_path.join("npwm"))
        .map_err(|error| probe_failed(probe, chip_path, "read PWM channel count", error))?
        .unwrap_or(0);

    Ok(PwmChipData {
        chip_name: chip_name.to_string(),
        chip_index: parse_prefixed_u32(chip_name, "pwmchip"),
        chip_path: chip_path.display().to_string(),
        channel_count,
    })
}

fn build_chip_descriptor(data: &PwmChipData) -> Result<DeviceDescriptor, String> {
    let mut builder = DeviceDescriptor::builder_for_kind(
        format!("linux.pwm.chip.{}", data.chip_name),
        DeviceKind::PwmChip,
    )
    .map_err(|error| {
        format!(
            "failed to start PWM chip descriptor '{}': {error}",
            data.chip_name
        )
    })?
    .display_name(data.chip_name.clone())
    .summary("Linux PWM controller")
    .address(DeviceAddress::PwmChip {
        chip_name: data.chip_name.clone(),
    })
    .label("backend", "linux")
    .label("chip_name", data.chip_name.clone())
    .property("chip_name", data.chip_name.clone())
    .property("sysfs_path", data.chip_path.clone())
    .property("channel_count", u64::from(data.channel_count));

    if let Some(chip_index) = data.chip_index {
        builder = builder.property("chip_index", u64::from(chip_index));
    }

    builder.build().map_err(|error| {
        format!(
            "failed to build PWM chip descriptor '{}': {error}",
            data.chip_name
        )
    })
}

fn build_channel_descriptor(
    data: &PwmChipData,
    chip_id: &DeviceId,
    channel: u32,
) -> Result<DeviceDescriptor, String> {
    let channel_path = Path::new(&data.chip_path).join(format!("pwm{channel}"));
    let mut builder = DeviceDescriptor::builder_for_kind(
        format!("linux.pwm.channel.{}.{}", data.chip_name, channel),
        DeviceKind::PwmChannel,
    )
    .map_err(|error| {
        format!(
            "failed to start PWM channel descriptor '{}:{}': {error}",
            data.chip_name, channel
        )
    })?
    .display_name(format!("{}:{channel}", data.chip_name))
    .summary("Linux PWM channel")
    .address(DeviceAddress::PwmChannel {
        chip_name: data.chip_name.clone(),
        channel,
    })
    .driver_hint("lemnos.pwm.generic")
    .label("backend", "linux")
    .label("chip_name", data.chip_name.clone())
    .property("chip_name", data.chip_name.clone())
    .property("channel", u64::from(channel))
    .property("sysfs_path", data.chip_path.clone())
    .property("exported", channel_path.exists())
    .link(DeviceLink::new(chip_id.clone(), DeviceRelation::Parent))
    .capability(pwm_capability("pwm.enable", CapabilityAccess::WRITE))
    .capability(pwm_capability("pwm.configure", CapabilityAccess::CONFIGURE))
    .capability(pwm_capability(
        "pwm.set_period",
        CapabilityAccess::CONFIGURE,
    ))
    .capability(pwm_capability(
        "pwm.set_duty_cycle",
        CapabilityAccess::CONFIGURE,
    ))
    .capability(pwm_capability(
        "pwm.get_configuration",
        CapabilityAccess::READ,
    ));

    if let Some(chip_index) = data.chip_index {
        builder = builder.property("chip_index", u64::from(chip_index));
    }

    builder.build().map_err(|error| {
        format!(
            "failed to build PWM channel descriptor '{}:{}': {error}",
            data.chip_name, channel
        )
    })
}

fn pwm_capability(id: &str, access: CapabilityAccess) -> CapabilityDescriptor {
    CapabilityDescriptor::new(id, access).expect("static PWM capability identifiers are valid")
}

fn probe_failed(probe: &str, path: &Path, action: &str, error: std::io::Error) -> DiscoveryError {
    DiscoveryError::ProbeFailed {
        probe: probe.to_string(),
        message: format!("{action} at '{}': {error}", path.display()),
    }
}
