//! The driver over a fake sysfs tree: a fan named `pwmfan` (`hwmon2`) and a fan-controller chip
//! named `nct6775` (`hwmon5`) with a manual and an automatic mode. The tree does not emulate the
//! kernel: the values written are the values read back.

use super::*;
use lemnos_contract_self_test::{Outcome, Record, SelfTest, reason};
use lemnos_core::{
    CallerId, Config, ConfigValue, Driver, JobState, Lifecycle, LocalCaller, Provider, Reader,
    Resources, Tag, Writer,
};
use lemnos_measure::test_alloc::{Counting, allocations};
use std::fs;
use std::panic::{AssertUnwindSafe, catch_unwind};

#[global_allocator]
static ALLOCATOR: Counting = Counting;

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
/// The `spin` test's settle, which the driver waits before it reads the tachometer.
const SETTLE: u64 = 3_000_000_000;
/// The `calling` callers the in-use rule is tested with.
const ASKER: CallerId = CallerId(7);
const OTHER: CallerId = CallerId(5);

/// A tachometer range of 1000 to 2000 revolutions per minute, from the board file.
const RANGE: [(&str, ConfigValue<'static>); 2] = [
    ("self_test_rpm_min", ConfigValue::Int(1000)),
    ("self_test_rpm_max", ConfigValue::Int(2000)),
];

/// Starts `spin` on `fan` and takes its first step: the fan is turned, and the answer waits.
fn start_spin(fan: &mut HwmonFan) -> lemnos_core::JobId {
    let job = SelfTest::run(fan, "spin", false).unwrap();
    Driver::tick(fan, 0);
    job
}

/// The answer of `job` as the driver's `job` gives it, and the record it carries.
fn answer(fan: &mut HwmonFan, job: lemnos_core::JobId) -> (JobState, Option<Record>) {
    let mut buffer = [0u8; lemnos_contract_self_test::RECORD_BYTES + 8];
    let mut writer = Writer::new(&mut buffer);
    let state = Lifecycle::job(fan, job, &mut writer);
    let used = writer.len();
    let record = match state {
        JobState::Finished(lemnos_core::Status::OK) => {
            Some(Record::read(&mut Reader::new(&buffer[..used])).unwrap())
        }
        _ => None,
    };
    (state, record)
}

/// The fan's duty and mode files as the tree holds them now.
fn duty_and_mode(root: &Path) -> (String, String) {
    (
        read(root, &format!("{PWM_FAN}/pwm1")),
        read(root, &format!("{PWM_FAN}/pwm1_enable")),
    )
}

/// A tachometer reading of `rpm`, as the fan's kernel file gives it.
fn set_rpm(root: &Path, rpm: &str) {
    write(root, &format!("{PWM_FAN}/fan1_input"), rpm);
}

#[test]
fn spin_turns_the_fan_and_puts_its_duty_and_mode_back() {
    let tree = Tree::new("spin-pass");
    set_rpm(tree.root(), "1500");
    let mut fan = bind(
        tree.root(),
        &[("name", ConfigValue::Str("pwmfan")), RANGE[0], RANGE[1]],
    )
    .unwrap();
    let job = start_spin(&mut fan);
    assert_eq!(
        duty_and_mode(tree.root()),
        ("128".into(), "1".into()),
        "the test duty, in manual mode"
    );
    assert_eq!(
        answer(&mut fan, job).0,
        JobState::Running {
            done: 0,
            total: 3000
        }
    );
    Driver::tick(&mut fan, SETTLE);
    let (state, record) = answer(&mut fan, job);
    assert_eq!(state, JobState::Finished(lemnos_core::Status::OK));
    let record = record.unwrap();
    assert_eq!(record.outcome, Outcome::Pass);
    assert_eq!(record.measures().next().unwrap().value, 1500);
    assert_eq!(
        duty_and_mode(tree.root()),
        ("77".into(), "2".into()),
        "the duty and the automatic mode it found"
    );
}

#[test]
fn spin_outside_the_range_fails_with_the_speed_it_read() {
    let tree = Tree::new("spin-range");
    set_rpm(tree.root(), "2500");
    let mut fan = bind(
        tree.root(),
        &[("name", ConfigValue::Str("pwmfan")), RANGE[0], RANGE[1]],
    )
    .unwrap();
    let job = start_spin(&mut fan);
    Driver::tick(&mut fan, SETTLE);
    let (_, record) = answer(&mut fan, job);
    let record = record.unwrap();
    assert_eq!(record.outcome, Outcome::Fail);
    assert_eq!(record.reason, reason::OUT_OF_RANGE);
    assert_eq!(record.measures().next().unwrap().value, 2500);
    assert_eq!(duty_and_mode(tree.root()), ("77".into(), "2".into()));
}

#[test]
fn spin_without_a_tachometer_is_skipped_and_leaves_the_fan_as_it_was() {
    let tree = Tree::new("spin-absent");
    let mut fan = bind(
        tree.root(),
        &[("name", ConfigValue::Str("pwmfan")), RANGE[0], RANGE[1]],
    )
    .unwrap();
    let job = SelfTest::run(&mut fan, "spin", false).unwrap();
    Driver::tick(&mut fan, 0);
    let (state, record) = answer(&mut fan, job);
    assert_eq!(state, JobState::Finished(lemnos_core::Status::OK));
    let record = record.unwrap();
    assert_eq!(record.outcome, Outcome::Skipped);
    assert_eq!(record.reason, reason::ABSENT);
    assert_eq!(
        duty_and_mode(tree.root()),
        ("77".into(), "2".into()),
        "nothing was written"
    );
}

#[test]
fn spin_without_a_range_is_skipped_before_anything_is_written() {
    let tree = Tree::new("spin-unconfigured");
    set_rpm(tree.root(), "1500");
    let mut fan = bind(tree.root(), &[("name", ConfigValue::Str("pwmfan"))]).unwrap();
    let job = SelfTest::run(&mut fan, "spin", false).unwrap();
    Driver::tick(&mut fan, 0);
    let (_, record) = answer(&mut fan, job);
    let record = record.unwrap();
    assert_eq!(record.reason, reason::NOT_CONFIGURED);
    assert_eq!(duty_and_mode(tree.root()), ("77".into(), "2".into()));
}

#[test]
fn cancelling_spin_puts_the_fan_back_at_once() {
    let tree = Tree::new("spin-cancel");
    set_rpm(tree.root(), "1500");
    let mut fan = bind(
        tree.root(),
        &[("name", ConfigValue::Str("pwmfan")), RANGE[0], RANGE[1]],
    )
    .unwrap();
    let job = start_spin(&mut fan);
    assert_eq!(duty_and_mode(tree.root()).0, "128");
    Lifecycle::job_cancel(&mut fan, job);
    assert_eq!(
        duty_and_mode(tree.root()),
        ("77".into(), "2".into()),
        "put back by the cancel"
    );
    assert_eq!(
        answer(&mut fan, job).0,
        JobState::Finished(lemnos_core::Status::CANCELLED)
    );
    assert!(
        fan.runner.last("spin").is_none(),
        "a cancelled test leaves last as it was"
    );
}

#[test]
fn a_tachometer_that_cannot_be_read_is_no_response_and_the_fan_is_put_back() {
    let tree = Tree::new("spin-unreadable");
    // A directory where the tachometer's file is: reading it fails.
    fs::create_dir_all(tree.root().join(format!("{PWM_FAN}/fan1_input"))).unwrap();
    let mut fan = bind(
        tree.root(),
        &[("name", ConfigValue::Str("pwmfan")), RANGE[0], RANGE[1]],
    )
    .unwrap();
    let job = start_spin(&mut fan);
    Driver::tick(&mut fan, SETTLE);
    let (_, record) = answer(&mut fan, job);
    let record = record.unwrap();
    assert_eq!(record.outcome, Outcome::Fail);
    assert_eq!(record.reason, reason::NO_RESPONSE);
    assert_eq!(duty_and_mode(tree.root()), ("77".into(), "2".into()));
}

#[test]
fn a_duty_change_or_a_release_is_refused_while_spin_runs() {
    let tree = Tree::new("spin-busy");
    set_rpm(tree.root(), "1500");
    let mut fan = bind(
        tree.root(),
        &[("name", ConfigValue::Str("pwmfan")), RANGE[0], RANGE[1]],
    )
    .unwrap();
    let job = start_spin(&mut fan);
    assert_eq!(fan.set_duty(300), Err(lemnos_core::Status::BUSY));
    assert_eq!(fan.release(), Err(lemnos_core::Status::BUSY));
    Driver::tick(&mut fan, SETTLE);
    answer(&mut fan, job);
    assert!(
        fan.set_duty(300).is_ok(),
        "allowed again once the test has ended"
    );
}

#[test]
fn spin_is_refused_while_another_caller_uses_the_fan() {
    let tree = Tree::new("spin-in-use");
    set_rpm(tree.root(), "1500");
    let mut fan = bind(
        tree.root(),
        &[("name", ConfigValue::Str("pwmfan")), RANGE[0], RANGE[1]],
    )
    .unwrap();
    Driver::caller(&mut fan, OTHER);
    Driver::caller(&mut fan, ASKER);
    assert_eq!(
        SelfTest::run(&mut fan, "spin", false),
        Err(lemnos_core::Status::BUSY)
    );
    assert_eq!(
        duty_and_mode(tree.root()),
        ("77".into(), "2".into()),
        "refused before anything changed"
    );
    Driver::caller_gone(&mut fan, OTHER);
    assert!(
        SelfTest::run(&mut fan, "spin", false).is_ok(),
        "the other caller has gone"
    );
}

#[test]
fn spin_on_a_host_owned_fan_is_skipped_and_nothing_is_written() {
    let tree = Tree::new("spin-host-owned");
    set_rpm(tree.root(), "1500");
    let entries = [
        ("sys_root", ConfigValue::Str(tree.root().to_str().unwrap())),
        ("name", ConfigValue::Str("pwmfan")),
        RANGE[0],
        RANGE[1],
    ];
    let mut provider = NoDevices;
    let mut resources = Resources::new(&mut provider, None);
    let mut fan = HwmonFan::bind(&mut resources, &Config::new(&entries).for_host_owned()).unwrap();
    let job = SelfTest::run(&mut fan, "spin", false).unwrap();
    Driver::tick(&mut fan, 0);
    let (_, record) = answer(&mut fan, job);
    assert_eq!(record.unwrap().reason, reason::READ_ONLY);
    assert_eq!(duty_and_mode(tree.root()), ("77".into(), "2".into()));
}

#[test]
fn idle_ticks_allocate_nothing() {
    let tree = Tree::new("spin-idle");
    let mut fan = bind(tree.root(), &[("name", ConfigValue::Str("pwmfan"))]).unwrap();
    let ((), count) = allocations(|| {
        for at in 0..1000u64 {
            Driver::tick(&mut fan, at);
        }
    });
    assert_eq!(count, 0, "allocations in 1000 idle ticks");
}

/// The refusal `check_config` gives for `entries`, or `None` when it accepts them.
fn refusal<'a>(entries: &'a [(&'a str, ConfigValue<'a>)]) -> Option<ConfigError<'a>> {
    <HwmonFan as lemnos_core::Lifecycle>::check_config(&Config::new(entries)).err()
}

#[test]
fn check_config_accepts_a_full_config() {
    let entries = [
        ("sys_root", ConfigValue::Str("/sys")),
        ("name", ConfigValue::Str("pwmfan")),
        ("max_rpm", ConfigValue::Int(3000)),
        ("warn_below_rpm", ConfigValue::Int(200)),
        ("self_test_duty", ConfigValue::Int(500)),
        ("self_test_rpm_min", ConfigValue::Int(1000)),
        ("self_test_rpm_max", ConfigValue::Int(2000)),
    ];
    assert_eq!(refusal(&entries), None);
}

#[test]
fn check_config_refuses_an_unknown_key_by_name_and_suggests_the_near_one() {
    let err = refusal(&[("max_rp", ConfigValue::Int(3000))]).unwrap();
    assert_eq!(err.key, "max_rp");
    assert_eq!(err.suggestion, Some("max_rpm"));
    assert_eq!(
        refusal(&[("bogus", ConfigValue::Int(1))]).unwrap().key,
        "bogus"
    );
}

#[test]
fn check_config_refuses_a_wrong_kind() {
    let err = refusal(&[("max_rpm", ConfigValue::Str("3000"))]).unwrap();
    assert_eq!(err.key, "max_rpm");
}

#[test]
fn check_config_refuses_a_hwmon_that_is_a_path_and_accepts_a_name() {
    assert_eq!(
        refusal(&[("hwmon", ConfigValue::Str("../hwmon2"))])
            .unwrap()
            .key,
        "hwmon"
    );
    assert!(refusal(&[("hwmon", ConfigValue::Str(""))]).is_some());
    assert_eq!(refusal(&[("hwmon", ConfigValue::Str("hwmon2"))]), None);
}

#[test]
fn check_config_refuses_a_negative_rpm_and_accepts_zero() {
    assert_eq!(
        refusal(&[("max_rpm", ConfigValue::Int(-1))]).unwrap().key,
        "max_rpm"
    );
    assert_eq!(refusal(&[("warn_below_rpm", ConfigValue::Int(0))]), None);
}

#[test]
fn check_config_keeps_the_test_duty_to_one_to_a_thousand() {
    assert!(refusal(&[("self_test_duty", ConfigValue::Int(0))]).is_some());
    assert!(refusal(&[("self_test_duty", ConfigValue::Int(1001))]).is_some());
    assert_eq!(refusal(&[("self_test_duty", ConfigValue::Int(1))]), None);
    assert_eq!(refusal(&[("self_test_duty", ConfigValue::Int(1000))]), None);
}

#[test]
fn check_config_refuses_a_test_range_that_is_backwards() {
    let backwards = [
        ("self_test_rpm_min", ConfigValue::Int(2000)),
        ("self_test_rpm_max", ConfigValue::Int(1000)),
    ];
    assert_eq!(refusal(&backwards).unwrap().key, "self_test_rpm_min");
    let equal = [
        ("self_test_rpm_min", ConfigValue::Int(1000)),
        ("self_test_rpm_max", ConfigValue::Int(1000)),
    ];
    assert_eq!(refusal(&equal), None);
}
