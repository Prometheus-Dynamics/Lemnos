use super::*;
use std::path::PathBuf;

impl MockHardware {
    pub fn attach_hwmon_fan(&self, fan: MockHwmonFan) -> DeviceId {
        let fan = MockHwmonFanState::from(fan);
        let device_id = fan.descriptor.id.clone();
        self.state().hwmon_fans.insert(device_id.clone(), fan);
        device_id
    }

    /// The fan's current raw `pwm1` value, as last written by a driver.
    pub fn hwmon_fan_pwm(&self, device_id: &DeviceId) -> Option<u64> {
        self.state()
            .hwmon_fans
            .get(device_id)
            .and_then(|fan| fan.read_u64("pwm1"))
    }

    /// The fan's current raw `pwm1_enable` mode, as last written by a driver.
    pub fn hwmon_fan_mode(&self, device_id: &DeviceId) -> Option<u64> {
        self.state()
            .hwmon_fans
            .get(device_id)
            .and_then(|fan| fan.read_u64("pwm1_enable"))
    }

    /// Simulates a new tachometer reading. Returns `false` for unknown fans.
    pub fn set_hwmon_fan_rpm(&self, device_id: &DeviceId, rpm: u64) -> bool {
        self.state()
            .hwmon_fans
            .get(device_id)
            .is_some_and(|fan| fan.write_u64("fan1_input", rpm))
    }

    /// The fan's private sysfs-style directory.
    pub fn hwmon_fan_root(&self, device_id: &DeviceId) -> Option<PathBuf> {
        self.state()
            .hwmon_fans
            .get(device_id)
            .map(|fan| fan.root().to_path_buf())
    }
}
