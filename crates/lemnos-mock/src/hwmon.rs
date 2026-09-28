use lemnos_core::{
    DeviceAddress, DeviceControlSurface, DeviceDescriptor, DeviceKind, InterfaceKind,
};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_SYSFS_ID: AtomicU64 = AtomicU64::new(0);

/// A fake Linux hwmon fan, backed by a private temporary sysfs-style directory.
///
/// Its descriptor matches what the Linux backend's hwmon probe reports
/// (`linux.subsystem = "hwmon"` with a `LinuxClass` control surface), so
/// class-device drivers such as `lemnos-drivers-pwm`'s `HwmonFanDriver` bind
/// it exactly as they would a real `pwm-fan`. Writes land in `pwm1` and
/// `pwm1_enable` and can be inspected through [`crate::MockHardware`]. The
/// directory is removed when the fan is removed or the hardware is dropped.
#[derive(Debug, Clone)]
pub struct MockHwmonFan {
    hwmon_name: String,
    name: String,
    pwm: u64,
    pwm_mode: u64,
    rpm: Option<u64>,
}

impl MockHwmonFan {
    /// A fan at `/sys/class/hwmon/<hwmon_name>`, named `pwmfan`, stopped, in
    /// manual mode, with a tachometer reading `0`.
    pub fn new(hwmon_name: impl Into<String>) -> Self {
        Self {
            hwmon_name: hwmon_name.into(),
            name: "pwmfan".into(),
            pwm: 0,
            pwm_mode: 1,
            rpm: Some(0),
        }
    }

    /// Sets the hwmon `name` attribute.
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Sets the raw `pwm1` value (`0..=255`).
    pub fn with_pwm(mut self, pwm: u64) -> Self {
        self.pwm = pwm;
        self
    }

    /// Sets the raw `pwm1_enable` mode.
    pub fn with_mode(mut self, pwm_mode: u64) -> Self {
        self.pwm_mode = pwm_mode;
        self
    }

    /// Sets the `fan1_input` tachometer reading.
    pub fn with_rpm(mut self, rpm: u64) -> Self {
        self.rpm = Some(rpm);
        self
    }

    /// Omits `fan1_input`, like a fan without a tachometer.
    pub fn without_tachometer(mut self) -> Self {
        self.rpm = None;
        self
    }
}

pub(crate) struct MockHwmonFanState {
    pub descriptor: DeviceDescriptor,
    sysfs: MockSysfsDir,
}

impl MockHwmonFanState {
    pub fn root(&self) -> &Path {
        &self.sysfs.0
    }

    pub fn read_u64(&self, attribute: &str) -> Option<u64> {
        fs::read_to_string(self.root().join(attribute))
            .ok()?
            .trim()
            .parse()
            .ok()
    }

    pub fn write_u64(&self, attribute: &str, value: u64) -> bool {
        fs::write(self.root().join(attribute), format!("{value}\n")).is_ok()
    }
}

impl From<MockHwmonFan> for MockHwmonFanState {
    fn from(fan: MockHwmonFan) -> Self {
        let id = NEXT_SYSFS_ID.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "lemnos-mock-hwmon-{}-{id}/{}",
            std::process::id(),
            fan.hwmon_name
        ));
        fs::create_dir_all(&root).expect("create mock hwmon directory");
        let sysfs = MockSysfsDir(root);
        let write = |attribute: &str, value: String| {
            fs::write(sysfs.0.join(attribute), format!("{value}\n"))
                .expect("write mock hwmon attribute");
        };
        write("name", fan.name.clone());
        write("pwm1", fan.pwm.to_string());
        write("pwm1_enable", fan.pwm_mode.to_string());
        if let Some(rpm) = fan.rpm {
            write("fan1_input", rpm.to_string());
        }

        let descriptor = DeviceDescriptor::builder_for_kind(
            format!("mock.pwm.hwmon-fan.{}", fan.hwmon_name),
            DeviceKind::Unspecified(InterfaceKind::Pwm),
        )
        .expect("mock hwmon fan builder")
        .display_name(fan.name.clone())
        .summary("Mock Linux hwmon fan")
        .address(DeviceAddress::Custom {
            interface: InterfaceKind::Pwm,
            scheme: "linux-hwmon-fan".into(),
            value: fan.hwmon_name.clone(),
        })
        .label("backend", "mock")
        .label("subsystem", "hwmon")
        .label("hwmon_name", fan.hwmon_name.clone())
        .control_surface(DeviceControlSurface::LinuxClass {
            root: sysfs.0.display().to_string(),
        })
        .property("linux.subsystem", "hwmon")
        .property("linux.class_path", sysfs.0.display().to_string())
        .property("hwmon.name", fan.name)
        .property("fan.hwmon_name", fan.hwmon_name)
        .build()
        .expect("mock hwmon fan descriptor");

        Self { descriptor, sysfs }
    }
}

/// Owns a temporary sysfs-style directory and removes it (and its per-fan
/// parent) on drop.
struct MockSysfsDir(PathBuf);

impl Drop for MockSysfsDir {
    fn drop(&mut self) {
        let parent = self.0.parent().map(Path::to_path_buf);
        let _ = fs::remove_dir_all(&self.0);
        if let Some(parent) = parent {
            let _ = fs::remove_dir(parent);
        }
    }
}
