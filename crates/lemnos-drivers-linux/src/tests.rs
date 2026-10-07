use super::*;
use lemnos_device::{Control, DeviceRef, NO_VALUE, Sensor};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

/// A throwaway sysfs tree.
struct Tree(PathBuf);

impl Tree {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let root = std::env::temp_dir().join(format!(
            "lemnos-drivers-linux-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        Self(root)
    }

    fn file(&self, path: &str, contents: &str) -> &Self {
        let path = self.0.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, format!("{contents}\n")).unwrap();
        self
    }

    fn link(&self, link: &str, target: &str) -> &Self {
        let link = self.0.join(link);
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(self.0.join(target), link).unwrap();
        self
    }

    fn read(&self, path: &str) -> String {
        fs::read_to_string(self.0.join(path)).unwrap().trim().into()
    }

    fn path(&self, path: &str) -> PathBuf {
        self.0.join(path)
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct NoDelay;

impl embedded_hal::delay::DelayNs for NoDelay {
    fn delay_ns(&mut self, _ns: u32) {}
}

#[test]
fn hwmon_fan_reads_and_controls() {
    let tree = Tree::new();
    tree.file("class/hwmon/hwmon0/name", "cpu_thermal")
        .file("class/hwmon/hwmon3/name", "pwm-fan")
        .file("class/hwmon/hwmon3/pwm1", "128")
        .file("class/hwmon/hwmon3/pwm1_enable", "2")
        .file("class/hwmon/hwmon3/fan1_input", "3000");
    let mut fan = HwmonFan::find(&tree.path("class/hwmon"), Some("pwm-fan"))
        .unwrap()
        .expect("fan");
    assert!(
        HwmonFan::find(&tree.path("class/hwmon"), Some("other"))
            .unwrap()
            .is_none()
    );
    let mut device = DeviceRef::both(&mut fan);
    device.init(&mut NoDelay).unwrap();
    let mut out = [0; 3];
    device.read(&mut out).unwrap();
    assert_eq!(out, [3000, 502, MODE_AUTOMATIC]);

    assert_eq!(device.set(CONTROL_MODE, MODE_MANUAL), Ok(MODE_MANUAL));
    // 70 % is pwm 179, which reads back as 70.2 %.
    assert_eq!(device.set(CONTROL_DUTY, 700), Ok(702));
    assert_eq!(tree.read("class/hwmon/hwmon3/pwm1"), "179");
    assert_eq!(
        device.set(CONTROL_DUTY, 1001),
        Err(lemnos_hal::ErrorKind::InvalidInput)
    );
    fan.restore_automatic().unwrap();
    assert_eq!(tree.read("class/hwmon/hwmon3/pwm1_enable"), "2");

    fs::remove_file(tree.path("class/hwmon/hwmon3/fan1_input")).unwrap();
    Sensor::read(&mut fan, &mut out).unwrap();
    assert_eq!(out[SPEED], NO_VALUE);
    assert_eq!((pwm_to_duty(255), duty_to_pwm(1000)), (1000, 255));
    assert_eq!(Control::get(&mut fan, CONTROL_DUTY), Ok(702));
}

#[test]
fn thermal_zone_reads_millidegrees() {
    let tree = Tree::new();
    tree.file("class/thermal/thermal_zone0/type", "cpu-thermal")
        .file("class/thermal/thermal_zone0/temp", "48250")
        .file("class/thermal/cooling_device0/type", "pwm-fan");
    let mut zone = ThermalZone::find(&tree.path("class/thermal"), "cpu-thermal")
        .unwrap()
        .expect("zone");
    assert_eq!(
        ThermalZone::all(&tree.path("class/thermal")).unwrap().len(),
        1
    );
    let mut out = [0; 1];
    let mut device = DeviceRef::sensor(&mut zone);
    device.init(&mut NoDelay).unwrap();
    device.read(&mut out).unwrap();
    assert_eq!(out, [48_250]);
    assert_eq!(zone.zone_type().unwrap(), "cpu-thermal");
    fs::remove_file(tree.path("class/thermal/thermal_zone0/temp")).unwrap();
    assert_eq!(
        DeviceRef::sensor(&mut zone).read(&mut out),
        Err(lemnos_hal::ErrorKind::NotFound)
    );
}

fn imu_tree(tree: &Tree) {
    let dev = "devices/platform/soc/i2c-1";
    tree.file(&format!("{dev}/1-0018/iio:device0/name"), "bmi088-accel")
        .file(&format!("{dev}/1-0018/iio:device0/in_accel_x_raw"), "1000")
        .file(&format!("{dev}/1-0018/iio:device0/in_accel_y_raw"), "-1000")
        .file(&format!("{dev}/1-0018/iio:device0/in_accel_z_raw"), "0")
        .file(
            &format!("{dev}/1-0018/iio:device0/in_accel_scale"),
            "0.001796",
        )
        .file(&format!("{dev}/1-0068/iio:device1/name"), "bmi088_gyro")
        .file(&format!("{dev}/1-0068/iio:device1/in_anglvel_x_raw"), "100")
        .file(&format!("{dev}/1-0068/iio:device1/in_anglvel_y_raw"), "0")
        .file(
            &format!("{dev}/1-0068/iio:device1/in_anglvel_z_raw"),
            "-100",
        )
        .file(
            &format!("{dev}/1-0068/iio:device1/in_anglvel_scale"),
            "0.001065264",
        )
        .link(
            "bus/iio/devices/iio:device0",
            &format!("{dev}/1-0018/iio:device0"),
        )
        .link(
            "bus/iio/devices/iio:device1",
            &format!("{dev}/1-0068/iio:device1"),
        );
}

#[test]
fn kernel_binding_serves_the_userspace_channels_from_iio() {
    let tree = Tree::new();
    imu_tree(&tree);
    let sys = SysRoot::new(&tree.0);
    let info = &lemnos_drivers_bmi088::INFO;
    let binding = &lemnos_drivers_bmi088::KERNEL;
    let at = |bus| I2cLocation {
        bus,
        addresses: &[0x18, 0x68],
    };
    assert!(
        KernelDevice::find(info, binding, &sys, Some(at(2)))
            .unwrap()
            .is_none()
    );
    let mut imu = KernelDevice::find(info, binding, &sys, Some(at(1)))
        .unwrap()
        .expect("both parts");
    let mut out = [0; 6];
    let mut device = DeviceRef::sensor(&mut imu);
    device.init(&mut NoDelay).unwrap();
    device.read(&mut out).unwrap();
    assert_eq!(device.info().model, "BMI088");
    // 1000 × 0.001796 m/s² and 100 × 0.001065264 rad/s.
    assert_eq!(out, [1_796, -1_796, 0, 106_526, 0, -106_526]);
}

#[test]
fn kernel_binding_reads_hwmon_power_monitors() {
    let tree = Tree::new();
    tree.file("class/hwmon/hwmon1/name", "ina238")
        .file("class/hwmon/hwmon1/in0_input", "10")
        .file("class/hwmon/hwmon1/in1_input", "12000")
        .file("class/hwmon/hwmon1/curr1_input", "1500")
        .file("class/hwmon/hwmon1/power1_input", "18000000")
        .file("class/hwmon/hwmon1/temp1_input", "25000");
    let model = lemnos_drivers_ina2xx::Model::Ina238;
    let mut ina = KernelDevice::find(model.info(), model.kernel(), &SysRoot::new(&tree.0), None)
        .unwrap()
        .expect("ina238");
    let mut out = [0; 5];
    Sensor::read(&mut ina, &mut out).unwrap();
    assert_eq!(out, [12_000_000, 10_000_000, 1_500_000, 18_000_000, 25_000]);
    let model = lemnos_drivers_ina2xx::Model::Ina226;
    assert!(
        KernelDevice::find(model.info(), model.kernel(), &SysRoot::new(&tree.0), None)
            .unwrap()
            .is_none()
    );
    assert!(KernelDevice::new(model.info(), model.kernel(), vec![]).is_err());
}
