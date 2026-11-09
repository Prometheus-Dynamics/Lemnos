//! The hand-back over a fake sysfs tree: a `pwm-fan` fan (driver link, a thermal cooling
//! device and a zone bound to it) and a fan-controller chip with a manual and automatic mode.
//! The tree does not emulate the kernel, so each test moves the governor's state itself where
//! the kernel would.

use super::*;
use lemnos_core::{ConfigValue, Driver, LocalCaller, Provider, Resources, Tag};
use std::fs;
use std::panic::{AssertUnwindSafe, catch_unwind};

/// Provides no other devices: the fan's `needs` is empty.
struct NoDevices;

impl Provider for NoDevices {
    fn provided(&self, _need: &str) -> bool {
        false
    }

    fn device(&mut self, _need: &str, _tag: Tag) -> Result<LocalCaller> {
        Err(Status::NOT_FOUND)
    }
}

/// A fake sysfs tree in its own directory, removed on drop.
struct Tree(PathBuf);

impl Tree {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("hwmon-fan-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let tree = Self(root);
        tree.build();
        tree
    }

    fn root(&self) -> &Path {
        &self.0
    }

    fn build(&self) {
        let fan = "devices/platform/cooling_fan";
        write(&self.0, &format!("{fan}/hwmon/hwmon2/name"), "pwmfan");
        write(&self.0, &format!("{fan}/hwmon/hwmon2/pwm1"), "77");
        write(&self.0, &format!("{fan}/hwmon/hwmon2/pwm1_enable"), "1");
        link(&self.0, &format!("{fan}/hwmon/hwmon2/device"), fan);
        write(&self.0, "bus/platform/drivers/pwm-fan/bind", "");
        link(
            &self.0,
            &format!("{fan}/driver"),
            "bus/platform/drivers/pwm-fan",
        );
        link(
            &self.0,
            "class/hwmon/hwmon2",
            &format!("{fan}/hwmon/hwmon2"),
        );
        let cdev = "devices/virtual/thermal/cooling_device0";
        write(&self.0, &format!("{cdev}/type"), "pwm-fan");
        write(&self.0, &format!("{cdev}/cur_state"), "1");
        write(&self.0, &format!("{cdev}/max_state"), "4");
        link(&self.0, "class/thermal/cooling_device0", cdev);
        let zone = "devices/virtual/thermal/thermal_zone0";
        write(&self.0, &format!("{zone}/type"), "cpu-thermal");
        write(&self.0, &format!("{zone}/temp"), "57850");
        write(&self.0, &format!("{zone}/policy"), "step_wise");
        link(&self.0, &format!("{zone}/cdev0"), cdev);
        link(&self.0, "class/thermal/thermal_zone0", zone);

        write(&self.0, "class/hwmon/hwmon5/name", "nct6775");
        write(&self.0, "class/hwmon/hwmon5/pwm1", "100");
        write(&self.0, "class/hwmon/hwmon5/pwm1_enable", "5");
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn write(root: &Path, path: &str, contents: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

fn link(root: &Path, link: &str, target: &str) {
    let link = root.join(link);
    fs::create_dir_all(link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(root.join(target), link).unwrap();
}

fn read(root: &Path, path: &str) -> String {
    fs::read_to_string(root.join(path)).unwrap().trim().into()
}

const PWM_FAN: &str = "class/hwmon/hwmon2";
const CHIP: &str = "class/hwmon/hwmon5";
const COOLING_STATE: &str = "devices/virtual/thermal/cooling_device0/cur_state";
const POLICY: &str = "class/thermal/thermal_zone0/policy";

/// Binds a fan on the tree at `root`, with `extra` config beside the tree's `sys_root`.
fn bind(root: &Path, extra: &[(&str, ConfigValue<'_>)]) -> Result<HwmonFan> {
    let sys = root.to_str().unwrap();
    let mut entries = vec![("sys_root", ConfigValue::Str(sys))];
    entries.extend_from_slice(extra);
    let mut provider = NoDevices;
    let mut resources = Resources::new(&mut provider, None);
    HwmonFan::bind(&mut resources, &Config::new(&entries))
}

fn pwm_fan(root: &Path) -> HwmonFan {
    match bind(root, &[("name", ConfigValue::Str("pwmfan"))]) {
        Ok(fan) => fan,
        Err(status) => panic!("bind failed: {status:?}"),
    }
}

fn chip(root: &Path) -> HwmonFan {
    match bind(root, &[("name", ConfigValue::Str("nct6775"))]) {
        Ok(fan) => fan,
        Err(status) => panic!("bind failed: {status:?}"),
    }
}

#[test]
fn the_duty_scale_is_rounded_both_ways() {
    assert_eq!(duty_to_pwm(0), 0);
    assert_eq!(duty_to_pwm(900), 230);
    assert_eq!(duty_to_pwm(1000), 255);
    assert_eq!(duty_to_pwm(5000), 255);
    assert_eq!(pwm_to_duty(0), 0);
    assert_eq!(pwm_to_duty(77), 302);
    assert_eq!(pwm_to_duty(230), 902);
    assert_eq!(pwm_to_duty(255), 1000);
    assert_eq!(pwm_to_duty(900), 1000);
    for duty in 0..=1000 {
        let back = i32::from(pwm_to_duty(duty_to_pwm(duty)));
        assert!(
            (back - i32::from(duty)).abs() <= 2,
            "{duty} came back as {back}"
        );
    }
}

#[test]
fn a_fan_is_found_by_name_or_hwmon_directory_or_not_at_all() {
    let tree = Tree::new("find");
    let by_name = pwm_fan(tree.root());
    assert!(by_name.fan.ends_with("hwmon2"));
    let by_dir = bind(tree.root(), &[("hwmon", ConfigValue::Str("hwmon5"))]).unwrap();
    assert!(by_dir.fan.ends_with("hwmon5"));
    assert!(matches!(
        bind(tree.root(), &[("name", ConfigValue::Str("missing"))]),
        Err(Status::NOT_FOUND)
    ));
    assert!(matches!(
        bind(tree.root(), &[("hwmon", ConfigValue::Str("../hwmon2"))]),
        Err(Status::INVALID_ARGUMENT)
    ));
}

#[test]
fn duty_and_rpm_read_back_and_out_of_range_duty_writes_nothing() {
    let tree = Tree::new("read");
    let mut fan = chip(tree.root());
    assert_eq!(fan.duty().unwrap(), 392);
    assert_eq!(fan.rpm().unwrap(), None);
    write(tree.root(), &format!("{CHIP}/fan1_input"), "1234");
    assert_eq!(fan.rpm().unwrap(), Some(1234));

    assert_eq!(fan.set_duty(1001), Err(Status::INVALID_ARGUMENT));
    assert_eq!(read(tree.root(), &format!("{CHIP}/pwm1_enable")), "5");
    fan.set_duty(500).unwrap();
    assert_eq!(read(tree.root(), &format!("{CHIP}/pwm1")), "128");
    assert_eq!(fan.duty().unwrap(), 502);
}

#[test]
fn a_pwm_fan_is_handed_back_on_release_and_can_be_taken_again() {
    let tree = Tree::new("release");
    let root = tree.root();
    let mut fan = pwm_fan(root);
    assert_eq!(fan.duty().unwrap(), 302);

    // The first write takes the fan: manual mode, and the kernel follows pwm1 to state 4.
    fan.set_duty(1000).unwrap();
    assert_eq!(read(root, &format!("{PWM_FAN}/pwm1_enable")), "1");
    assert_eq!(read(root, &format!("{PWM_FAN}/pwm1")), "255");
    write(root, COOLING_STATE, "4");
    assert_eq!(fan.duty().unwrap(), 1000);

    // Release: pwm1, its mode and the governor's state come back, and the zone re-evaluates.
    fan.release().unwrap();
    assert_eq!(read(root, &format!("{PWM_FAN}/pwm1")), "77");
    assert_eq!(read(root, &format!("{PWM_FAN}/pwm1_enable")), "1");
    assert_eq!(read(root, COOLING_STATE), "1");
    assert_eq!(read(root, POLICY), "step_wise");

    // The governor moves on, and the next write takes the fan again, recording that state.
    write(root, COOLING_STATE, "2");
    fan.set_duty(900).unwrap();
    write(root, COOLING_STATE, "4");
    fan.release().unwrap();
    assert_eq!(read(root, COOLING_STATE), "2");
    assert_eq!(read(root, &format!("{PWM_FAN}/pwm1")), "77");

    // Released, so the governor's later state is left alone when the driver is dropped.
    write(root, COOLING_STATE, "3");
    drop(fan);
    assert_eq!(read(root, COOLING_STATE), "3");
}

#[test]
fn dropping_the_drivers_hands_both_fans_back_exactly() {
    let tree = Tree::new("drop");
    let root = tree.root();
    let mut fan = pwm_fan(root);
    let mut other = chip(root);
    fan.set_duty(1000).unwrap();
    write(root, COOLING_STATE, "4");
    other.set_duty(500).unwrap();
    assert_eq!(read(root, &format!("{CHIP}/pwm1_enable")), "1");
    assert_eq!(read(root, &format!("{CHIP}/pwm1")), "128");

    // The zone's policy is written back, so its mtime moves past the epoch.
    fs::File::options()
        .write(true)
        .open(root.join(POLICY))
        .unwrap()
        .set_modified(std::time::UNIX_EPOCH)
        .unwrap();
    drop(fan);
    assert!(fs::metadata(root.join(POLICY)).unwrap().modified().unwrap() > std::time::UNIX_EPOCH);
    assert_eq!(read(root, COOLING_STATE), "1");
    assert_eq!(read(root, &format!("{PWM_FAN}/pwm1")), "77");
    assert_eq!(read(root, &format!("{PWM_FAN}/pwm1_enable")), "1");

    // The chip keeps its own saved mode, 5, and not a default.
    drop(other);
    assert_eq!(read(root, &format!("{CHIP}/pwm1")), "100");
    assert_eq!(read(root, &format!("{CHIP}/pwm1_enable")), "5");
}

#[test]
fn a_panic_hands_the_fan_back() {
    let tree = Tree::new("panic");
    let root = tree.root().to_path_buf();
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        let mut fan = pwm_fan(&root);
        fan.set_duty(1000).unwrap();
        write(&root, COOLING_STATE, "4");
        panic!("the driver panicked");
    }));
    assert!(outcome.is_err());
    assert_eq!(read(&root, &format!("{PWM_FAN}/pwm1")), "77");
    assert_eq!(read(&root, &format!("{PWM_FAN}/pwm1_enable")), "1");
    assert_eq!(read(&root, COOLING_STATE), "1");
}

#[test]
fn a_missing_attribute_fails_the_call_and_the_next_call_tries_again() {
    let tree = Tree::new("missing");
    let root = tree.root();
    let mut fan = chip(root);
    let enable = root.join(CHIP).join("pwm1_enable");
    fs::remove_file(&enable).unwrap();
    assert_eq!(fan.set_duty(500), Err(Status::NOT_FOUND));
    assert!(matches!(Driver::health(&fan), Health::Degraded(_)));

    write(root, &format!("{CHIP}/pwm1_enable"), "5");
    fan.set_duty(500).unwrap();
    assert!(matches!(Driver::health(&fan), Health::Ok));

    // A restore that cannot write pwm1 reports it, still restores the mode, and is tried again.
    let pwm = root.join(CHIP).join("pwm1");
    fs::remove_file(&pwm).unwrap();
    assert!(fan.release().is_err());
    assert_eq!(read(root, &format!("{CHIP}/pwm1_enable")), "5");
    write(root, &format!("{CHIP}/pwm1"), "1");
    fan.release().unwrap();
    assert_eq!(read(root, &format!("{CHIP}/pwm1")), "100");
}

#[test]
fn a_release_with_nothing_taken_writes_nothing() {
    let tree = Tree::new("untaken");
    let mut fan = chip(tree.root());
    fan.release().unwrap();
    drop(fan);
    assert_eq!(read(tree.root(), &format!("{CHIP}/pwm1")), "100");
    assert_eq!(read(tree.root(), &format!("{CHIP}/pwm1_enable")), "5");
}
