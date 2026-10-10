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
    let mut out = [0; 12];
    let mut device = DeviceRef::sensor(&mut imu);
    device.init(&mut NoDelay).unwrap();
    device.read(&mut out).unwrap();
    assert_eq!(device.info().model, "BMI088");
    // 1000 × 0.001796 m/s² and 100 × 0.001065264 rad/s.
    assert_eq!(out[..6], [1_796, -1_796, 0, 106_526, 0, -106_526]);
    // The calibrated channels come from the driver, not the kernel.
    assert!(out[6..].iter().all(|v| *v == lemnos_device::NO_VALUE));
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

#[test]
fn ws2812_writes_the_rp1_layout_and_controls_apply() {
    use lemnos_device::{Pixels, Rgbw};
    use lemnos_drivers_ws2812::{StripConfig, Wire};
    let tree = Tree::new();
    tree.file("dev/leds0", "");
    let mut strip = Ws2812Pio::new(
        tree.path("dev/leds0"),
        StripConfig::new(4, Wire::Rgbw).with_offset(1),
    );
    let mut device = DeviceRef::light(&mut strip);
    device.init(&mut NoDelay).unwrap();
    assert_eq!(device.pixel_count(), 4);
    device
        .show(&[Rgbw::new(1, 2, 3, 4), Rgbw::rgb(0x102030)])
        .unwrap();
    let bytes = fs::read(tree.path("dev/leds0")).unwrap();
    assert_eq!(
        bytes,
        [0, 0, 0, 0, 1, 2, 3, 4, 0x10, 0x20, 0x30, 0, 0, 0, 0, 0]
    );
    assert_eq!(device.set(CONTROL_BRIGHTNESS, 500), Ok(500));
    assert_eq!(device.set(CONTROL_COLOR, 0xff0000), Ok(0xff0000));
    let bytes = fs::read(tree.path("dev/leds0")).unwrap();
    // Half brightness red on every LED.
    assert_eq!(&bytes[..4], &[128, 0, 0, 0]);
    assert_eq!(device.get(CONTROL_BRIGHTNESS), Ok(502));
    assert_eq!(Pixels::pixel_count(&strip), 4);
}

#[test]
fn userspace_regulator_switches_through_state() {
    use lemnos_hal::Regulator;
    let tree = Tree::new();
    tree.file("devices/platform/cam-supply/state", "disabled")
        .file("class/regulator/regulator.5/microvolts", "2800000");
    let mut supply = UserspaceRegulator::new(tree.path("devices/platform/cam-supply"))
        .with_regulator(tree.path("class/regulator/regulator.5"));
    assert!(!supply.is_enabled().unwrap());
    supply.enable().unwrap();
    assert_eq!(tree.read("devices/platform/cam-supply/state"), "enabled");
    assert!(supply.is_enabled().unwrap());
    assert_eq!(supply.voltage_uv().unwrap(), Some(2_800_000));
    assert_eq!(
        lemnos_hal::HalError::kind(&supply.set_voltage_uv(1, 2).unwrap_err()),
        lemnos_hal::ErrorKind::Unsupported
    );
    supply.set_enabled(false).unwrap();
    assert_eq!(tree.read("devices/platform/cam-supply/state"), "disabled");
}

#[test]
fn debugfs_clock_reports_its_rate_and_refuses_gating() {
    use lemnos_hal::ClockOutput;
    let tree = Tree::new();
    tree.file("debug/clk/cam0_clk/clk_rate", "24000000")
        .file("debug/clk/cam0_clk/clk_enable_count", "1");
    let mut clock = DebugfsClock::named(&tree.path("debug"), "cam0_clk");
    assert_eq!(clock.rate_hz().unwrap(), 24_000_000);
    clock.enable().unwrap();
    assert!(clock.disable().is_err());
    clock.set(Some(24_000_000)).unwrap();
    assert!(clock.set(Some(19_200_000)).is_err());
}

/// A `pwm-fan` fan on a platform device, with the cooling device the thermal
/// governor drives it through.
fn pwm_fan_tree(tree: &Tree, linked: bool) {
    tree.file("devices/platform/cooling_fan/hwmon/hwmon2/name", "pwmfan")
        .file("devices/platform/cooling_fan/hwmon/hwmon2/pwm1", "255")
        .file("devices/platform/cooling_fan/hwmon/hwmon2/pwm1_enable", "1")
        .file("bus/platform/drivers/pwm-fan/bind", "")
        .link(
            "devices/platform/cooling_fan/driver",
            "bus/platform/drivers/pwm-fan",
        )
        .link(
            "devices/platform/cooling_fan/hwmon/hwmon2/device",
            "devices/platform/cooling_fan",
        )
        .link(
            "class/hwmon/hwmon2",
            "devices/platform/cooling_fan/hwmon/hwmon2",
        )
        .file("devices/virtual/thermal/cooling_device0/type", "pwm-fan")
        .file("devices/virtual/thermal/cooling_device0/cur_state", "2")
        .file("devices/virtual/thermal/cooling_device0/max_state", "4")
        .link(
            "class/thermal/cooling_device0",
            "devices/virtual/thermal/cooling_device0",
        )
        .file("devices/virtual/thermal/cooling_device1/type", "Processor")
        .file("devices/virtual/thermal/cooling_device1/cur_state", "0")
        .link(
            "class/thermal/cooling_device1",
            "devices/virtual/thermal/cooling_device1",
        )
        // cpu-thermal drives the fan; another zone drives something else.
        .file("devices/virtual/thermal/thermal_zone0/policy", "step_wise")
        .file(
            "devices/virtual/thermal/thermal_zone0/cdev0_trip_point",
            "1",
        )
        .link(
            "devices/virtual/thermal/thermal_zone0/cdev0",
            "devices/virtual/thermal/cooling_device0",
        )
        .link(
            "class/thermal/thermal_zone0",
            "devices/virtual/thermal/thermal_zone0",
        )
        .file("devices/virtual/thermal/thermal_zone1/policy", "untouched")
        .link(
            "class/thermal/thermal_zone1",
            "devices/virtual/thermal/thermal_zone1",
        );
    if linked {
        tree.link(
            "devices/virtual/thermal/cooling_device1/device",
            "devices/platform/cooling_fan",
        );
    }
}

#[test]
fn pwm_fan_restores_the_governors_state_and_kicks_its_zone() {
    let tree = Tree::new();
    pwm_fan_tree(&tree, false);
    let fan = HwmonFan::find(&tree.path("class/hwmon"), Some("pwmfan"))
        .unwrap()
        .expect("fan");
    assert_eq!(fan.driver().as_deref(), Some(PWM_FAN_DRIVER));
    // Bound while the kernel had it enabled (1) at governor state 2; the
    // board's restore_mode 2 does not apply to a cooling-device fan.
    let mut plan = fan.restore_plan(&tree.path("class/thermal"), 2).unwrap();
    let cdev = tree.path("class/thermal/cooling_device0");
    assert_eq!(
        plan.kind,
        RestoreKind::CoolingDevice {
            enable: 1,
            devices: vec![CoolingRecord {
                device: cdev.clone(),
                state: Some(2),
            }],
        }
    );
    // The governor stepped up before the controller's first write: that is
    // the state to restore.
    tree.file("devices/virtual/thermal/cooling_device0/cur_state", "3");
    plan.record_states().unwrap();
    assert!(plan.to_line().ends_with("cooling_device0=3"));
    assert_eq!(FanRestore::from_line(&plan.to_line()), Some(plan.clone()));

    // The controller took over; pwm-fan moved cur_state to match pwm1.
    fan.set_mode(MODE_FULL_SPEED).unwrap();
    fan.set_pwm(255).unwrap();
    tree.file("devices/virtual/thermal/cooling_device0/cur_state", "4");
    let nudges = plan.apply().unwrap();
    assert_eq!(
        nudges,
        vec![CoolingNudge {
            state: Some(3),
            via: None,
            zones: 1
        }]
    );
    assert_eq!(
        tree.read("class/hwmon/hwmon2/pwm1_enable"),
        MODE_MANUAL.to_string()
    );
    assert_eq!(tree.read("class/thermal/cooling_device0/cur_state"), "3");
    // The zone bound to the device got its policy written back; the other
    // zone was left alone.
    assert_eq!(tree.read("class/thermal/thermal_zone0/policy"), "step_wise");
    assert_eq!(tree.read("class/thermal/thermal_zone1/policy"), "untouched");

    // Already at the recorded state: a neighbour goes first.
    assert_eq!(
        plan.apply().unwrap(),
        vec![CoolingNudge {
            state: Some(3),
            via: Some(2),
            zones: 1
        }]
    );
}

#[test]
fn unrecorded_cooling_devices_only_kick_the_governor() {
    let tree = Tree::new();
    pwm_fan_tree(&tree, true);
    let fan = HwmonFan::new(tree.path("class/hwmon/hwmon2"));
    let plan = fan
        .restore_plan_with(&tree.path("class/thermal"), 2, MODE_MANUAL)
        .unwrap();
    // The linked cooling device wins over the pwm-fan-typed one.
    assert_eq!(
        plan.kind,
        RestoreKind::CoolingDevice {
            enable: MODE_MANUAL,
            devices: vec![CoolingRecord::unrecorded(
                tree.path("class/thermal/cooling_device1")
            )],
        }
    );
    tree.file("devices/virtual/thermal/cooling_device1/cur_state", "1");
    assert_eq!(
        plan.apply().unwrap(),
        vec![CoolingNudge {
            state: None,
            via: None,
            zones: 0
        }]
    );
    assert_eq!(tree.read("class/thermal/cooling_device1/cur_state"), "1");
    assert_eq!(FanRestore::from_line(&plan.to_line()), Some(plan));
}

#[test]
fn chip_fan_restores_automatic_mode() {
    let tree = Tree::new();
    tree.file("class/hwmon/hwmon1/name", "nct6775")
        .file("class/hwmon/hwmon1/pwm1", "100")
        .file("class/hwmon/hwmon1/pwm1_enable", "1")
        .file("class/thermal/cooling_device0/type", "pwm-fan")
        .file("class/thermal/cooling_device0/cur_state", "1");
    let fan = HwmonFan::new(tree.path("class/hwmon/hwmon1"));
    assert_eq!(fan.driver(), None);
    let plan = fan.restore_plan(&tree.path("class/thermal"), 5).unwrap();
    assert_eq!(plan.kind, RestoreKind::Automatic { mode: 5 });
    assert_eq!(FanRestore::from_line(&plan.to_line()), Some(plan.clone()));
    assert_eq!(plan.apply().unwrap(), Vec::new());
    assert_eq!(tree.read("class/hwmon/hwmon1/pwm1_enable"), "5");
    assert_eq!(FanRestore::from_line("bogus\t1\t/x"), None);
}

#[test]
fn sysfs_pwm_exports_orders_writes_and_releases() {
    use lemnos_hal::raw::{Polarity, PwmConfig, RawPwm};
    let tree = Tree::new();
    tree.file("class/pwm/pwmchip0/npwm", "2")
        .file("class/pwm/pwmchip0/export", "")
        .file("class/pwm/pwmchip0/unexport", "");
    let sys = SysRoot::new(&tree.0);
    assert_eq!(
        lemnos_hal::HalError::kind(&SysfsPwm::open(&sys, 0, 2).unwrap_err()),
        lemnos_hal::ErrorKind::NotFound
    );
    // The fake kernel: the channel directory as an export creates it.
    tree.file("class/pwm/pwmchip0/pwm1/period", "0")
        .file("class/pwm/pwmchip0/pwm1/duty_cycle", "0")
        .file("class/pwm/pwmchip0/pwm1/enable", "0")
        .file("class/pwm/pwmchip0/pwm1/polarity", "normal");
    let mut pwm = SysfsPwm::open(&sys, 0, 1).unwrap();
    let config = PwmConfig {
        period_ns: 40_000,
        duty_ns: 10_000,
        polarity: Polarity::Inversed,
        enabled: true,
    };
    pwm.configure(&config).unwrap();
    assert_eq!(pwm.config(), Ok(config));
    // A shorter period than the current duty: duty goes first.
    let shorter = PwmConfig {
        period_ns: 5_000,
        duty_ns: 1_000,
        ..config
    };
    pwm.configure(&shorter).unwrap();
    assert_eq!(tree.read("class/pwm/pwmchip0/pwm1/period"), "5000");
    assert!(
        pwm.configure(&PwmConfig {
            duty_ns: 6_000,
            ..shorter
        })
        .is_err()
    );
    // Found already exported: release disables but does not unexport.
    pwm.release().unwrap();
    assert_eq!(tree.read("class/pwm/pwmchip0/pwm1/enable"), "0");
    assert_eq!(tree.read("class/pwm/pwmchip0/unexport"), "");
}
