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
fn hand_back_puts_back_the_value_then_the_mode_the_take_over_found() {
    // The baseline is pwm1 77 in automatic mode (2). The take-over sets manual mode and a duty.
    let tree = Tree::new("handback");
    let root = tree.root();
    let mut fan = pwm_fan(root);
    fan.set_duty(900).unwrap();
    assert_eq!(read(root, &format!("{PWM_FAN}/pwm1")), "230");
    assert_eq!(read(root, &format!("{PWM_FAN}/pwm1_enable")), "1");

    fan.hand_back().unwrap();
    assert_eq!(
        read(root, &format!("{PWM_FAN}/pwm1")),
        "77",
        "the value the take-over found, not full duty"
    );
    assert_eq!(
        read(root, &format!("{PWM_FAN}/pwm1_enable")),
        "2",
        "the mode the take-over found"
    );
    assert!(matches!(Driver::health(&fan), Health::Ok));
    // Given back, the driver writes nothing more when it is dropped.
    drop(fan);
    assert_eq!(read(root, &format!("{PWM_FAN}/pwm1")), "77");
    assert_eq!(read(root, &format!("{PWM_FAN}/pwm1_enable")), "2");
}

#[test]
fn a_rebind_after_a_drop_keeps_the_baseline_the_first_take_over_recorded() {
    // A fan taken over, then let go of by a drop (its safe state is full duty), then bound again. The second
    // take-over must not record the drop's full duty as the baseline: a hand-back gives the first one back.
    let tree = Tree::new("rebind");
    let root = tree.root();
    let mut fan = pwm_fan(root);
    fan.set_duty(900).unwrap();
    drop(fan);
    assert_eq!(read(root, &format!("{PWM_FAN}/pwm1")), "255");
    let mut again = pwm_fan(root);
    again.set_duty(400).unwrap();
    again.hand_back().unwrap();
    assert_eq!(
        read(root, &format!("{PWM_FAN}/pwm1")),
        "77",
        "the first baseline, not the drop's full duty"
    );
    assert_eq!(read(root, &format!("{PWM_FAN}/pwm1_enable")), "2");
}

#[test]
fn a_failed_hand_back_keeps_the_baseline_and_is_tried_again() {
    let tree = Tree::new("handback-retry");
    let root = tree.root();
    let mut fan = pwm_fan(root);
    fan.set_duty(900).unwrap();

    let pwm = root.join(PWM_FAN).join("pwm1");
    fs::remove_file(&pwm).unwrap();
    assert!(fan.hand_back().is_err());
    assert!(matches!(Driver::health(&fan), Health::Degraded(_)));
    // Still taken: the next write is a duty, not a take-over.
    write(root, &format!("{PWM_FAN}/pwm1"), "1");
    fan.hand_back().unwrap();
    assert_eq!(read(root, &format!("{PWM_FAN}/pwm1")), "77");
    assert_eq!(read(root, &format!("{PWM_FAN}/pwm1_enable")), "2");
    assert!(matches!(Driver::health(&fan), Health::Ok));
}

#[test]
fn a_hand_back_with_nothing_taken_writes_nothing() {
    let tree = Tree::new("handback-untaken");
    let mut fan = chip(tree.root());
    fan.hand_back().unwrap();
    drop(fan);
    assert_eq!(read(tree.root(), &format!("{CHIP}/pwm1")), "100");
    assert_eq!(read(tree.root(), &format!("{CHIP}/pwm1_enable")), "5");
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

/// The item as the wire carries it, for the display path.
fn wire_item(item: &lemnos_core::Item) -> lemnos_wire::OwnedItem {
    lemnos_wire::OwnedItem {
        kind: lemnos_core::ItemKind::Value,
        name: item.name.to_string(),
        unit: item.unit.symbol().to_string(),
        unit_state: item.unit_state,
        exponent: item.exponent,
        min: item.min,
        max: item.max,
        real_min: item.real_min,
        real_max: item.real_max,
        real_step: item.real_step,
        real_warn_low: item.real_warn_low,
        real_warn_high: item.real_warn_high,
        contract: item.method.contract,
        method: item.method.method,
        field: item.field.map(str::to_string),
        reading: None,
        step: item.step,
        warn_low: item.warn_low,
        warn_high: item.warn_high,
        group: item.group.map(str::to_string),
    }
}

/// A duty of 302 per mille shows as 302, through the display path. Fails before the fix: the
/// item had exponent -3, so it showed 0.302.
#[test]
fn a_duty_of_302_per_mille_displays_as_302() {
    let tree = Tree::new("duty-display");
    let fan = chip(tree.root());
    let mut out = Description::new();
    Driver::describe(&fan, &mut out);
    let duty = out.find("duty").unwrap();
    // The reply of `duty` is a u16, here 302 per mille (77 in pwm counts).
    let mut fan = fan;
    write(tree.root(), &format!("{CHIP}/pwm1"), "77");
    let reply = fan.duty().unwrap().to_le_bytes();
    let shown = lemnos_wire::text::render(
        &lemnos_wire::OwnedShape::U16,
        false,
        &reply,
        &wire_item(duty),
    )
    .unwrap();
    assert_eq!(shown, "302", "a per-mille count is shown as the count");
    assert_eq!(duty.exponent, 0);
}

/// The read and the control have names of their own, so a client finds the control by name.
/// Fails before the fix: both were named `duty`.
#[test]
fn the_duty_read_and_the_duty_control_have_different_names() {
    let tree = Tree::new("duty-names");
    let fan = chip(tree.root());
    let mut out = Description::new();
    Driver::describe(&fan, &mut out);
    let read = out.items().find(|item| item.name == "duty").unwrap();
    assert_eq!(read.kind, lemnos_core::ItemKind::Value);
    let control = out.find("set_duty").unwrap();
    assert_eq!(control.kind, lemnos_core::ItemKind::Control);
    assert_eq!(control.method, SET_DUTY);
}

/// The duty item's step is the fan's own resolution, and the read-back is within half of it of the duty asked
/// for, for every duty: `pwm1` has 255 levels, so two duties it can hold are at most 4 per mille apart, and a duty
/// reads back at most 2 away. 300 reads back 302 and 900 reads back 902, as on the board (B14, BOARD-4).
/// The driver before this change gave no such figure: its duty item said a step of 1, the unit.
#[test]
fn the_duty_step_is_the_fans_resolution_and_every_duty_reads_back_within_half_of_it() {
    assert_eq!(DUTY_STEP, 4);
    for duty in 0..=MAX_DUTY {
        let back = pwm_to_duty(duty_to_pwm(duty));
        assert!(
            u32::from(back.abs_diff(duty)) <= DUTY_STEP / 2,
            "{duty} reads back {back}"
        );
    }
    assert_eq!(pwm_to_duty(duty_to_pwm(300)), 302);
    assert_eq!(pwm_to_duty(duty_to_pwm(900)), 902);
    // Two neighbouring levels are never further apart than the step.
    for pwm in 0..PWM_MAX {
        assert!(u32::from(pwm_to_duty(pwm + 1) - pwm_to_duty(pwm)) <= DUTY_STEP);
    }
}

/// Callers for the ownership tests: two clients and the daemon's own writes (`CallerId::LOCAL`).
const CALLER_A: CallerId = CallerId(7);
const CALLER_B: CallerId = CallerId(8);
const SEC: u64 = 1_000_000_000;

#[test]
fn the_owner_going_hands_the_fan_back_to_its_baseline() {
    let tree = Tree::new("owner-gone");
    let root = tree.root();
    let mut fan = pwm_fan(root);
    Lifecycle::caller(&mut fan, CALLER_A);
    fan.set_duty(900).unwrap();
    assert_eq!(read(root, &format!("{PWM_FAN}/pwm1")), "230");

    Lifecycle::caller_gone(&mut fan, CALLER_A);
    assert_eq!(
        read(root, &format!("{PWM_FAN}/pwm1")),
        "77",
        "the baseline the take-over found"
    );
    assert_eq!(read(root, &format!("{PWM_FAN}/pwm1_enable")), "2");
    assert!(matches!(Driver::health(&fan), Health::Ok));
}

#[test]
fn an_older_caller_going_leaves_the_newer_owners_duty() {
    let tree = Tree::new("owner-older");
    let root = tree.root();
    let mut fan = pwm_fan(root);
    Lifecycle::caller(&mut fan, CALLER_A);
    fan.set_duty(900).unwrap();
    Lifecycle::caller(&mut fan, CALLER_B);
    fan.set_duty(400).unwrap();

    Lifecycle::caller_gone(&mut fan, CALLER_A);
    assert_eq!(
        read(root, &format!("{PWM_FAN}/pwm1")),
        "102",
        "B's duty stays"
    );
    assert_eq!(read(root, &format!("{PWM_FAN}/pwm1_enable")), "1");

    Lifecycle::caller_gone(&mut fan, CALLER_B);
    assert_eq!(read(root, &format!("{PWM_FAN}/pwm1")), "77");
    assert_eq!(read(root, &format!("{PWM_FAN}/pwm1_enable")), "2");
}

#[test]
fn a_controller_write_after_a_client_keeps_the_fan_when_the_client_goes() {
    // The daemon's own controller writes as LOCAL. Its write after the client's makes it the owner, so the
    // client going hands nothing back.
    let tree = Tree::new("owner-local");
    let root = tree.root();
    let mut fan = pwm_fan(root);
    Lifecycle::caller(&mut fan, CALLER_A);
    fan.set_duty(900).unwrap();
    Lifecycle::caller(&mut fan, CallerId::LOCAL);
    fan.set_duty(500).unwrap();

    Lifecycle::caller_gone(&mut fan, CALLER_A);
    assert_eq!(
        read(root, &format!("{PWM_FAN}/pwm1")),
        "128",
        "the controller's duty stays"
    );
    assert_eq!(read(root, &format!("{PWM_FAN}/pwm1_enable")), "1");
}

#[test]
fn a_release_makes_its_caller_the_owner() {
    let tree = Tree::new("owner-release");
    let root = tree.root();
    let mut fan = pwm_fan(root);
    Lifecycle::caller(&mut fan, CALLER_A);
    fan.set_duty(900).unwrap();
    Lifecycle::caller(&mut fan, CALLER_B);
    fan.release().unwrap();

    Lifecycle::caller_gone(&mut fan, CALLER_A);
    assert_eq!(
        read(root, &format!("{PWM_FAN}/pwm1")),
        "255",
        "B's release stays"
    );
    Lifecycle::caller_gone(&mut fan, CALLER_B);
    assert_eq!(read(root, &format!("{PWM_FAN}/pwm1")), "77");
}

#[test]
fn a_hand_back_owed_to_a_gone_owner_is_retried_from_the_tick_and_waits_longer_each_time() {
    let tree = Tree::new("owner-retry");
    let root = tree.root();
    let mut fan = pwm_fan(root);
    Lifecycle::caller(&mut fan, CALLER_A);
    Lifecycle::tick(&mut fan, SEC);
    fan.set_duty(900).unwrap();
    let pwm = root.join(PWM_FAN).join("pwm1");
    fs::remove_file(&pwm).unwrap();

    // The owner goes at 1 s: the first try fails, so it is owed. The next try is 1 s on.
    Lifecycle::caller_gone(&mut fan, CALLER_A);
    assert!(matches!(Driver::health(&fan), Health::Degraded(_)));
    assert_eq!(Lifecycle::tick(&mut fan, SEC), Some(2 * SEC));

    // Not due at 1.5 s: nothing is tried, so a value written meanwhile stands.
    write(root, &format!("{PWM_FAN}/pwm1"), "1");
    assert_eq!(Lifecycle::tick(&mut fan, SEC + SEC / 2), Some(2 * SEC));
    assert_eq!(read(root, &format!("{PWM_FAN}/pwm1")), "1");

    // Due at 2 s, and the attribute is still missing: the second wait is 2 s, so the next try is at 4 s.
    fs::remove_file(&pwm).unwrap();
    assert_eq!(Lifecycle::tick(&mut fan, 2 * SEC), Some(4 * SEC));
    assert!(matches!(Driver::health(&fan), Health::Degraded(_)));

    // Due at 4 s with the attribute back: the baseline is written, and nothing is owed any more.
    write(root, &format!("{PWM_FAN}/pwm1"), "1");
    assert_eq!(Lifecycle::tick(&mut fan, 4 * SEC), None);
    assert_eq!(read(root, &format!("{PWM_FAN}/pwm1")), "77");
    assert_eq!(read(root, &format!("{PWM_FAN}/pwm1_enable")), "2");
    assert!(matches!(Driver::health(&fan), Health::Ok));
}

#[test]
fn the_owed_hand_back_waits_1_2_4_then_8_seconds_and_never_longer() {
    let waits: Vec<u64> = (1..=6).map(retry_wait_ns).collect();
    assert_eq!(waits, [SEC, 2 * SEC, 4 * SEC, 8 * SEC, 8 * SEC, 8 * SEC]);
    assert_eq!(retry_wait_ns(u32::MAX), RETRY_CAP_NS);
}

#[test]
fn a_new_writer_takes_the_fan_over_from_an_owed_hand_back() {
    let tree = Tree::new("owner-claim");
    let root = tree.root();
    let mut fan = pwm_fan(root);
    Lifecycle::caller(&mut fan, CALLER_A);
    fan.set_duty(900).unwrap();
    let pwm = root.join(PWM_FAN).join("pwm1");
    fs::remove_file(&pwm).unwrap();
    Lifecycle::caller_gone(&mut fan, CALLER_A);
    assert!(matches!(Driver::health(&fan), Health::Degraded(_)));

    // B writes before the owed hand-back is due. The fan is B's now, so nothing is handed back under it.
    write(root, &format!("{PWM_FAN}/pwm1"), "1");
    Lifecycle::caller(&mut fan, CALLER_B);
    fan.set_duty(400).unwrap();
    assert_eq!(read(root, &format!("{PWM_FAN}/pwm1")), "102");
    assert_eq!(Lifecycle::tick(&mut fan, 100 * SEC), None);
    assert_eq!(read(root, &format!("{PWM_FAN}/pwm1")), "102");

    // B's going hands back to the baseline, which is still the one the first take-over recorded.
    Lifecycle::caller_gone(&mut fan, CALLER_B);
    assert_eq!(read(root, &format!("{PWM_FAN}/pwm1")), "77");
    assert_eq!(read(root, &format!("{PWM_FAN}/pwm1_enable")), "2");
}

/// The note the driver pushed last, taken from its ring.
fn last_note(fan: &mut HwmonFan) -> Option<lemnos_core::Note> {
    let mut last = None;
    while let Some(note) = Lifecycle::notes(fan).and_then(|notes| notes.pop()) {
        last = Some(note);
    }
    last
}

#[test]
fn a_hand_back_notes_the_baseline_it_restored_and_nothing_when_it_had_none() {
    let tree = Tree::new("handback-record");
    let mut fan = pwm_fan(tree.root());
    assert_eq!(last_note(&mut fan), None);
    fan.set_duty(900).unwrap();
    fan.hand_back().unwrap();
    assert_eq!(
        last_note(&mut fan),
        Some(lemnos_core::Note::new("handed back to the governor")),
        "mode 2 is the governor"
    );
    // A second hand-back has nothing to give back, so it notes nothing.
    fan.hand_back().unwrap();
    assert_eq!(last_note(&mut fan), None);
}

#[test]
fn a_fan_without_a_mode_attribute_is_never_taken_so_there_is_no_mode_to_lose() {
    let tree = Tree::new("no-mode");
    let root = tree.root();
    let mut fan = pwm_fan(root);
    fs::remove_file(root.join(PWM_FAN).join("pwm1_enable")).unwrap();
    assert_eq!(
        fan.set_duty(500),
        Err(Status::NOT_FOUND),
        "the take-over reads the mode first, and a fan without one is refused"
    );
    assert_eq!(
        read(root, &format!("{PWM_FAN}/pwm1")),
        "77",
        "pwm1 is not written"
    );
    fan.hand_back().unwrap();
    assert_eq!(
        last_note(&mut fan),
        None,
        "nothing was taken, so nothing is handed back"
    );
}

#[test]
fn the_hand_back_notes_name_the_baseline_mode_the_safe_state_and_the_controller() {
    assert_eq!(
        HandBack::Baseline { pwm: 77, mode: 1 }.note(),
        lemnos_core::Note::with_value("handed back to the baseline pwm1_enable mode", 1)
    );
    assert_eq!(
        HandBack::SafeState.note(),
        lemnos_core::Note::new("handed back to full duty: no baseline recorded")
    );
    assert_eq!(
        HandBack::Controller.note(),
        lemnos_core::Note::new("handed back to the controller")
    );
}

#[test]
fn a_client_that_replaced_the_controllers_write_hands_the_fan_back_to_it_without_writing() {
    // The daemon's write (LOCAL) is replaced by a client's, which goes: nothing is written, the controller owns
    // the fan again, and its next step writes its duty.
    let tree = Tree::new("owner-controller");
    let root = tree.root();
    let mut fan = pwm_fan(root);
    Lifecycle::caller(&mut fan, CallerId::LOCAL);
    fan.set_duty(500).unwrap();
    Lifecycle::caller(&mut fan, CALLER_A);
    fan.set_duty(900).unwrap();
    Lifecycle::caller_gone(&mut fan, CALLER_A);
    assert_eq!(
        read(root, &format!("{PWM_FAN}/pwm1")),
        "230",
        "the client's duty is left for the controller's step"
    );
    assert_eq!(
        read(root, &format!("{PWM_FAN}/pwm1_enable")),
        "1",
        "no baseline mode was written"
    );
    assert_eq!(
        last_note(&mut fan),
        Some(lemnos_core::Note::new("handed back to the controller"))
    );
    // The controller is the owner: the client's going again changes nothing.
    Lifecycle::caller_gone(&mut fan, CALLER_A);
    assert_eq!(last_note(&mut fan), None);
}

/// A timed override holds the duty, and the owner's baseline comes back on the tick at its deadline,
/// with that tick the next due one the host is given.
#[test]
fn a_timed_override_hands_the_fan_back_on_the_tick_at_its_deadline() {
    let tree = Tree::new("override-deadline");
    let root = tree.root();
    let mut fan = pwm_fan(root);
    Lifecycle::caller(&mut fan, CALLER_A);
    Lifecycle::tick(&mut fan, SEC);
    fan.override_for(900, 5_000).unwrap();
    // The tick after the call arms it: the next due time is the deadline, 5 s on.
    assert_eq!(Lifecycle::tick(&mut fan, SEC), Some(6 * SEC));
    assert_eq!(fan.duty().unwrap(), pwm_to_duty(duty_to_pwm(900)));
    assert_eq!(
        read(root, &format!("{PWM_FAN}/pwm1_enable")),
        "1",
        "held manual"
    );

    // Not yet: a millisecond early, the duty stands and the deadline is still the next due time.
    assert_eq!(
        Lifecycle::tick(&mut fan, 6 * SEC - 1_000_000),
        Some(6 * SEC)
    );
    assert_eq!(fan.duty().unwrap(), pwm_to_duty(duty_to_pwm(900)));

    // At the deadline the baseline is written (pwm 77, mode 2), and nothing is due any more.
    assert_eq!(Lifecycle::tick(&mut fan, 6 * SEC), None);
    assert_eq!(read(root, &format!("{PWM_FAN}/pwm1")), "77");
    assert_eq!(read(root, &format!("{PWM_FAN}/pwm1_enable")), "2");
}

/// The owner going before the deadline hands the fan back at once, and the timer goes with it.
#[test]
fn the_owner_going_before_a_timed_overrides_deadline_hands_the_fan_back_at_once() {
    let tree = Tree::new("override-gone");
    let root = tree.root();
    let mut fan = pwm_fan(root);
    Lifecycle::caller(&mut fan, CALLER_A);
    Lifecycle::tick(&mut fan, SEC);
    fan.override_for(900, 5_000).unwrap();
    Lifecycle::tick(&mut fan, SEC);
    Lifecycle::tick(&mut fan, 2 * SEC);
    Lifecycle::caller_gone(&mut fan, CALLER_A);
    assert_eq!(
        read(root, &format!("{PWM_FAN}/pwm1")),
        "77",
        "at once, not at 6 s"
    );
    assert_eq!(read(root, &format!("{PWM_FAN}/pwm1_enable")), "2");
    assert_eq!(Lifecycle::tick(&mut fan, 6 * SEC), None, "no timer is left");
}

/// A new set replaces the timer: the deadline of the override it replaced does nothing.
#[test]
fn a_new_set_replaces_a_timed_overrides_timer() {
    let tree = Tree::new("override-replaced-set");
    let root = tree.root();
    let mut fan = pwm_fan(root);
    Lifecycle::caller(&mut fan, CALLER_A);
    Lifecycle::tick(&mut fan, SEC);
    fan.override_for(900, 5_000).unwrap();
    Lifecycle::tick(&mut fan, SEC);
    Lifecycle::tick(&mut fan, 3 * SEC);
    fan.set_duty(300).unwrap();
    assert_eq!(
        Lifecycle::tick(&mut fan, 6 * SEC),
        None,
        "no hand-back at the old deadline"
    );
    assert_eq!(fan.duty().unwrap(), pwm_to_duty(duty_to_pwm(300)));
    assert_eq!(
        read(root, &format!("{PWM_FAN}/pwm1_enable")),
        "1",
        "still manual"
    );
}

/// A new override replaces the old one's timer, and its own deadline is the one that counts.
#[test]
fn a_new_timed_override_replaces_the_timer_with_its_own_deadline() {
    let tree = Tree::new("override-replaced-override");
    let root = tree.root();
    let mut fan = pwm_fan(root);
    Lifecycle::caller(&mut fan, CALLER_A);
    Lifecycle::tick(&mut fan, SEC);
    fan.override_for(900, 5_000).unwrap();
    Lifecycle::tick(&mut fan, SEC);
    Lifecycle::tick(&mut fan, 2 * SEC);
    Lifecycle::caller(&mut fan, CALLER_B);
    fan.override_for(400, 2_000).unwrap();
    // The new deadline, 2 s after the tick that arms it, is the next due time: not A's 6 s.
    assert_eq!(Lifecycle::tick(&mut fan, 2 * SEC), Some(4 * SEC));
    assert_eq!(fan.duty().unwrap(), pwm_to_duty(duty_to_pwm(400)));
    assert_eq!(Lifecycle::tick(&mut fan, 4 * SEC), None);
    assert_eq!(read(root, &format!("{PWM_FAN}/pwm1")), "77");
}

/// Bounds: 1 s to 10 min, refused by name outside them, and a refusal sets nothing.
#[test]
fn an_override_outside_one_second_to_ten_minutes_is_refused_and_sets_nothing() {
    let tree = Tree::new("override-bounds");
    let root = tree.root();
    let mut fan = pwm_fan(root);
    Lifecycle::caller(&mut fan, CALLER_A);
    Lifecycle::tick(&mut fan, SEC);
    assert_eq!(fan.override_for(900, 999), Err(Status::INVALID_ARGUMENT));
    assert_eq!(
        fan.override_for(900, 600_001),
        Err(Status::INVALID_ARGUMENT)
    );
    assert_eq!(
        read(root, &format!("{PWM_FAN}/pwm1")),
        "77",
        "nothing was taken"
    );
    assert_eq!(read(root, &format!("{PWM_FAN}/pwm1_enable")), "2");
    fan.override_for(900, 1_000).unwrap();
    fan.override_for(900, 600_000).unwrap();
}
