//! The driver over a fake sysfs tree: a fan named `pwmfan` (`hwmon2`) and a fan-controller chip
//! named `nct6775` (`hwmon5`) with a manual and an automatic mode. The tree does not emulate the
//! kernel: the values written are the values read back.

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
        write(&tree.0, &format!("{PWM_FAN}/name"), "pwmfan");
        write(&tree.0, &format!("{PWM_FAN}/pwm1"), "77");
        write(&tree.0, &format!("{PWM_FAN}/pwm1_enable"), "2");
        write(&tree.0, &format!("{CHIP}/name"), "nct6775");
        write(&tree.0, &format!("{CHIP}/pwm1"), "100");
        write(&tree.0, &format!("{CHIP}/pwm1_enable"), "5");
        write(&tree.0, &format!("{CHIP}/fan1_input"), "0");
        tree
    }

    fn root(&self) -> &Path {
        &self.0
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

fn read(root: &Path, path: &str) -> String {
    fs::read_to_string(root.join(path)).unwrap().trim().into()
}

const PWM_FAN: &str = "class/hwmon/hwmon2";
const CHIP: &str = "class/hwmon/hwmon5";

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
    assert!(by_name.pwm.ends_with("hwmon2/pwm1"));
    let by_dir = bind(tree.root(), &[("hwmon", ConfigValue::Str("hwmon5"))]).unwrap();
    assert!(by_dir.pwm.ends_with("hwmon5/pwm1"));
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
    assert_eq!(fan.rpm().unwrap(), Some(0));
    write(tree.root(), &format!("{CHIP}/fan1_input"), "1234");
    assert_eq!(fan.rpm().unwrap(), Some(1234));
    assert_eq!(fan.set_duty(1001), Err(Status::INVALID_ARGUMENT));
    assert_eq!(read(tree.root(), &format!("{CHIP}/pwm1_enable")), "5");
    fan.set_duty(500).unwrap();
    assert_eq!(read(tree.root(), &format!("{CHIP}/pwm1")), "128");
    assert_eq!(fan.duty().unwrap(), 502);
}

#[test]
fn the_first_write_takes_the_fan_to_manual_and_release_puts_it_at_full_duty() {
    let tree = Tree::new("release");
    let root = tree.root();
    let mut fan = pwm_fan(root);
    fan.set_duty(500).unwrap();
    assert_eq!(read(root, &format!("{PWM_FAN}/pwm1_enable")), "1");
    assert_eq!(read(root, &format!("{PWM_FAN}/pwm1")), "128");

    fan.release().unwrap();
    assert_eq!(read(root, &format!("{PWM_FAN}/pwm1")), "255");
    assert_eq!(read(root, &format!("{PWM_FAN}/pwm1_enable")), "1");

    // The next write takes the fan again, and it is not handed back to anything in between.
    fan.set_duty(900).unwrap();
    assert_eq!(read(root, &format!("{PWM_FAN}/pwm1")), "230");
}

#[test]
fn dropping_a_driver_that_took_its_fan_leaves_it_at_full_duty_and_the_other_alone() {
    let tree = Tree::new("drop");
    let root = tree.root();
    let mut fan = pwm_fan(root);
    let mut other = chip(root);
    fan.set_duty(1000).unwrap();
    other.set_duty(500).unwrap();
    drop(fan);
    assert_eq!(read(root, &format!("{PWM_FAN}/pwm1")), "255");
    assert_eq!(read(root, &format!("{PWM_FAN}/pwm1_enable")), "1");
    // The chip was taken too, so it is at full duty; its mode is not restored.
    drop(other);
    assert_eq!(read(root, &format!("{CHIP}/pwm1")), "255");
    assert_eq!(read(root, &format!("{CHIP}/pwm1_enable")), "1");
}

#[test]
fn a_panic_leaves_a_taken_fan_at_full_duty() {
    let tree = Tree::new("panic");
    let root = tree.root().to_path_buf();
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        let mut fan = pwm_fan(&root);
        fan.set_duty(300).unwrap();
        panic!("the driver panicked");
    }));
    assert!(outcome.is_err());
    assert_eq!(read(&root, &format!("{PWM_FAN}/pwm1")), "255");
    assert_eq!(read(&root, &format!("{PWM_FAN}/pwm1_enable")), "1");
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

    // A release that cannot write pwm1 reports it, and is tried again.
    let pwm = root.join(CHIP).join("pwm1");
    fs::remove_file(&pwm).unwrap();
    assert!(fan.release().is_err());
    assert!(matches!(Driver::health(&fan), Health::Degraded(_)));
    write(root, &format!("{CHIP}/pwm1"), "1");
    fan.release().unwrap();
    assert_eq!(read(root, &format!("{CHIP}/pwm1")), "255");
    assert!(matches!(Driver::health(&fan), Health::Ok));
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

#[test]
fn the_board_gives_the_speed_range_and_the_stall_limit() {
    let tree = Tree::new("rpm-limits");
    let fan = bind(
        tree.root(),
        &[
            ("name", ConfigValue::Str("pwmfan")),
            ("max_rpm", ConfigValue::Int(3000)),
            ("warn_below_rpm", ConfigValue::Int(200)),
        ],
    )
    .unwrap();
    let mut out = Description::new();
    Driver::describe(&fan, &mut out);
    let rpm = out.find("rpm").unwrap();
    assert_eq!(
        (rpm.min, rpm.max, rpm.step, rpm.warn_low, rpm.warn_high),
        (Some(0), Some(3000), Some(1), Some(200), None)
    );
    // Without the board's figures the speed has none, and a bad one is refused at bind.
    let bare = pwm_fan(tree.root());
    let mut out = Description::new();
    Driver::describe(&bare, &mut out);
    let rpm = out.find("rpm").unwrap();
    assert_eq!((rpm.min, rpm.max, rpm.warn_low), (None, None, None));
    assert!(matches!(
        bind(tree.root(), &[("max_rpm", ConfigValue::Int(-1))]),
        Err(Status::INVALID_ARGUMENT)
    ));
}

#[test]
fn the_fan_names_its_hwmon_directory_as_its_location() {
    let tree = Tree::new("location");
    let fan = pwm_fan(tree.root());
    let mut out = Description::new();
    Driver::describe(&fan, &mut out);
    assert!(
        out.location().ends_with("class/hwmon/hwmon2"),
        "{}",
        out.location()
    );
}
