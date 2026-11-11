//! A host-owned device's driver changes nothing, whatever it does and from wherever: merged
//! drivers that write when they bind and when they tick, bound to devices the board gives to
//! the host, beside the same drivers on devices that are Lemnos's.
//!
//! Nothing here is real hardware: a fake sysfs tree, the simulated board's line and I2C bus,
//! and a plain file standing where a watchdog's node would be.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use lemnos_core::{Config, Lifecycle, MethodId, Result, Status, StreamHandle, contract, driver};
use lemnos_driver_bmm150::Bmm150;
use lemnos_driver_fan_control::FanController;
use lemnos_driver_gpio_power_switch::GpioPowerSwitch;
use lemnos_driver_hw_watchdog::HwWatchdog;
use lemnos_driver_hwmon_fan::HwmonFan;
use lemnos_driver_thermal_zone::ThermalZone;
use lemnos_hal::blocks::{BufferKind, StreamFrame};
use lemnos_host::board::parse;
use lemnos_host::{DeviceState, DriverEntry, Host, Options, entry};
use lemnos_sim::{SimBoard, SimClock};
use lemnos_wire::ring::Consumer;

const HEADER: &str = "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n";

fn host_on(sim: SimBoard, devices: &str, drivers: &[DriverEntry]) -> (Host, SimClock) {
    let clock = sim.clock();
    let host = Host::start(
        &parse(&format!("{HEADER}{devices}")).unwrap(),
        drivers,
        Box::new(sim),
        Arc::new(clock.clone()),
        Options::default(),
    )
    .unwrap();
    (host, clock)
}

/// Lets `seconds` pass a second at a time, with the host's ticks.
fn run(host: &Host, clock: &SimClock, seconds: u32) {
    host.tick();
    for _ in 0..seconds {
        clock.advance(Duration::from_secs(1));
        host.tick();
    }
}

/// Why a device is waiting, when it is.
fn waiting(host: &Host, id: &str) -> Option<String> {
    match host.device(id).unwrap().state {
        DeviceState::Backoff { reason, .. } => Some(reason),
        _ => None,
    }
}

/// A directory of its own, removed on drop.
struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("host-owned-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        Self(root)
    }

    fn write(&self, path: &str, contents: &str) {
        let path = self.0.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn read(&self, path: &str) -> String {
        fs::read_to_string(self.0.join(path))
            .unwrap()
            .trim()
            .to_string()
    }

    fn root(&self) -> &Path {
        &self.0
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

// A driver that writes from `tick`, through another device.

/// A fan in automatic mode at `pwm1` 10 and a thermal zone at 90 °C, both Lemnos's, and a fan
/// controller on them owned as `owner` says. At 90 °C the controller's curve asks for full duty.
fn controller(tree: &Dir, owner: &str) -> (Host, SimClock) {
    tree.write("class/hwmon/hwmon3/name", "ours");
    tree.write("class/hwmon/hwmon3/pwm1", "10");
    tree.write("class/hwmon/hwmon3/pwm1_enable", "2");
    tree.write("class/thermal/thermal_zone0/temp", "90000");
    let sys = tree.root().display();
    let devices = format!(
        r#"
[[devices]]
id = "fan"
driver = "hwmon-fan"
config = {{ sys_root = "{sys}", name = "ours" }}

[[devices]]
id = "zone"
driver = "thermal-zone"
config = {{ path = "{sys}/class/thermal/thermal_zone0" }}

[[devices]]
id = "ctl"
driver = "fan-control"
{owner}
uses = {{ fan = "fan", t0 = "zone" }}
"#
    );
    host_on(
        SimBoard::new(),
        &devices,
        &[
            entry::<HwmonFan>(),
            entry::<ThermalZone>(),
            entry::<FanController>(),
        ],
    )
}

#[test]
fn a_host_owned_controller_ticks_and_never_moves_the_fan_it_uses() {
    let tree = Dir::new("ctl-host");
    let (host, clock) = controller(&tree, "owner = \"host\"");
    assert_eq!(host.device("ctl").unwrap().state, DeviceState::Running);
    run(&host, &clock, 20);
    // It ticked, read 90 °C, and asked for full duty every second: each ask was refused before
    // the fan's driver ran, so the kernel's files are as they were.
    assert_eq!(tree.read("class/hwmon/hwmon3/pwm1"), "10");
    assert_eq!(tree.read("class/hwmon/hwmon3/pwm1_enable"), "2");
    // Its reads go through: the controller's own state is a value.
    let ctl = host.device("ctl").unwrap().index;
    let state = MethodId {
        contract: 0,
        method: 1,
    };
    assert!(host.call(ctl, state, &[], &mut [0; 128]).is_ok());
    // Stopping it hands nothing back, because nothing was taken.
    host.shutdown();
    assert_eq!(tree.read("class/hwmon/hwmon3/pwm1"), "10");
    assert_eq!(tree.read("class/hwmon/hwmon3/pwm1_enable"), "2");
}

#[test]
fn the_same_controller_owned_by_lemnos_drives_the_fan() {
    let tree = Dir::new("ctl-ours");
    let (host, clock) = controller(&tree, "");
    run(&host, &clock, 2);
    assert_eq!(tree.read("class/hwmon/hwmon3/pwm1"), "255");
    assert_eq!(tree.read("class/hwmon/hwmon3/pwm1_enable"), "1");
    host.shutdown();
}

// A driver that writes a line when it binds.

fn switch(owner: &str) -> (Host, lemnos_hal::mock::MockLine) {
    let sim = SimBoard::new();
    let line = sim.add_line("power");
    let devices = format!(
        "[[devices]]\nid = \"sw\"\ndriver = \"gpio-power-switch\"\n{owner}\nuses = {{ line = \"power\" }}\n"
    );
    let (host, _) = host_on(sim, &devices, &[entry::<GpioPowerSwitch>()]);
    (host, line)
}

#[test]
fn a_host_owned_switch_never_drives_its_line_and_stays_unbound() {
    let (host, line) = switch("owner = \"host\"");
    // Binding it means driving the line, which the line refused: the driver could not start.
    let reason = waiting(&host, "sw").expect("not running");
    assert!(
        reason.contains(&format!("{:?}", Status::ACCESS)),
        "refused, and not for any other reason: {reason}"
    );
    assert!(line.configs().is_empty(), "the line was never configured");
    assert_eq!(line.level(), None, "nor driven");
    host.shutdown();
    assert!(line.configs().is_empty());
}

#[test]
fn the_same_switch_owned_by_lemnos_drives_its_line() {
    let (host, line) = switch("");
    assert_eq!(host.device("sw").unwrap().state, DeviceState::Running);
    assert_eq!(line.configs().len(), 1);
    assert_eq!(line.level(), Some(true));
}

// A driver that writes registers on I2C when it binds.

fn magnetometer(owner: &str) -> (Host, lemnos_sim::SimI2c) {
    let sim = SimBoard::new();
    let bus = sim.add_i2c("i2c");
    bus.attach(0x10, lemnos_sim::Layout::Bytes);
    let devices =
        format!("[[devices]]\nid = \"mag\"\ndriver = \"bmm150\"\nbus = \"i2c\"\n{owner}\n");
    let (host, _) = host_on(sim, &devices, &[entry::<Bmm150>()]);
    (host, bus)
}

/// Asks the chip for a sample, which its driver can only give by writing to it first.
fn sample(host: &Host) -> Result<usize> {
    let read = MethodId {
        contract: 0,
        method: 1,
    };
    host.call(0, read, &[], &mut [0; 64])
}

#[test]
fn a_host_owned_chip_sees_nothing_on_its_bus() {
    let (host, bus) = magnetometer("owner = \"host\"");
    assert_eq!(bus.transactions(), 0);
    // Whatever the driver tries, when it binds or when it is asked for a sample, needs a
    // register written, and the bus refuses before anything goes out.
    let _ = sample(&host);
    let _ = sample(&host);
    assert_eq!(bus.transactions(), 0, "refused before the bus");
    host.shutdown();
    assert_eq!(bus.transactions(), 0);
    // The same chip as Lemnos's is written to.
    let (host, bus) = magnetometer("");
    let _ = sample(&host);
    assert!(bus.transactions() > 0);
}

// A driver that reaches hardware by itself and writes from `tick`.

fn watchdog(dir: &Dir, owner: &str) -> (Host, SimClock) {
    dir.write("dev/watchdog0", "");
    let root = dir.root().display();
    let devices = format!(
        "[[devices]]\nid = \"dog\"\ndriver = \"hw-watchdog\"\n{owner}\n\
         config = {{ device = \"watchdog0\", dev_root = \"{root}/dev\", sys_root = \"{root}/sys\" }}\n"
    );
    host_on(SimBoard::new(), &devices, &[entry::<HwWatchdog>()])
}

#[test]
fn a_driver_that_writes_past_its_needs_is_never_started_on_a_host_owned_device() {
    let dir = Dir::new("dog-host");
    let (host, clock) = watchdog(&dir, "owner = \"host\"");
    run(&host, &clock, 10);
    let reason = waiting(&host, "dog").expect("not running");
    assert!(reason.contains("host-owned"), "{reason}");
    assert_eq!(dir.read("dev/watchdog0"), "", "never fed, never opened");
    host.shutdown();
    assert_eq!(dir.read("dev/watchdog0"), "");
}

#[test]
fn the_same_driver_owned_by_lemnos_feeds_its_node_from_its_tick() {
    let dir = Dir::new("dog-ours");
    let (host, clock) = watchdog(&dir, "");
    assert_eq!(host.device("dog").unwrap().state, DeviceState::Running);
    run(&host, &clock, 3);
    assert!(!dir.read("dev/watchdog0").is_empty(), "fed");
}

// A stream: a consumer's jobs are changes asked of the driver, and reach it as methods do.

#[contract("example.jobs", version = 1)]
pub trait Jobs {
    /// Jobs the driver has read off its stream.
    #[method(1)]
    fn jobs(&mut self) -> Result<u32>;
    /// Whether the stream says its buffers can be handed to hardware.
    #[method(2)]
    fn handles(&mut self) -> Result<bool>;
}

#[driver("jobs", provides = [Jobs])]
pub struct JobReader {
    #[needs]
    frames: StreamHandle,
    jobs: u32,
}

impl Lifecycle for JobReader {
    type Needs = JobReaderNeeds;
    const TICKS: bool = true;
    const HOST_OWNED: lemnos_core::HostOwned = lemnos_core::HostOwned::ThroughNeeds;

    fn start(needs: JobReaderNeeds, _: &Config<'_>) -> Result<Self> {
        Ok(Self {
            frames: needs.frames,
            jobs: 0,
        })
    }

    fn describe(&self, out: &mut lemnos_core::Description) {
        for method in [1, 2] {
            out.add(lemnos_core::Item::value(
                if method == 1 { "jobs" } else { "handles" },
                MethodId {
                    contract: 0,
                    method,
                },
            ));
        }
    }

    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        let mut job = [0u8; 16];
        while let Ok(Some(_)) = self.frames.next_job(&mut job) {
            self.jobs += 1;
        }
        // A frame out is the device being read: it passes on any device.
        if let Ok(Some(buffer)) = self.frames.next_buffer() {
            let _ = self.frames.publish(StreamFrame {
                buffer,
                bytes: 0,
                sequence: 0,
                identity: 0,
                timestamp_ns: now_ns,
            });
        }
        Some(now_ns + 1_000_000)
    }
}

impl Jobs for JobReader {
    fn jobs(&mut self) -> Result<u32> {
        Ok(self.jobs)
    }

    fn handles(&mut self) -> Result<bool> {
        Ok(self.frames.buffer_kind() == BufferKind::Handles)
    }
}

fn job_reader(owner: &str) -> (Host, SimClock) {
    let devices = format!(
        "[[devices]]\nid = \"j\"\ndriver = \"jobs\"\n{owner}\n\
         [devices.streams.frames]\nbuffers = 4\nbuffer_bytes = 64\njob_slots = 4\njob_bytes = 16\n"
    );
    host_on(SimBoard::new(), &devices, &[entry::<JobReader>()])
}

fn read_u32(host: &Host, method: u16) -> u32 {
    let mut reply = [0u8; 8];
    let id = MethodId {
        contract: 0,
        method,
    };
    let n = host.call(0, id, &[], &mut reply).unwrap();
    assert!(n >= 1);
    reply[..n]
        .iter()
        .rev()
        .fold(0, |value, byte| value << 8 | u32::from(*byte))
}

#[test]
fn a_consumers_jobs_never_reach_a_host_owned_devices_driver_and_its_frames_still_come() {
    for (owner, expected) in [("owner = \"host\"", 0), ("", 2)] {
        let (host, clock) = job_reader(owner);
        let lease = host
            .stream_attach(0, "frames", lemnos_core::CallerId(5))
            .unwrap();
        let mut consumer = Consumer::attach(Arc::clone(&lease.ring), lease.layout).unwrap();
        assert!(consumer.push_job(b"change it").unwrap());
        assert!(consumer.push_job(b"and again").unwrap());
        run(&host, &clock, 2);
        assert_eq!(read_u32(&host, 1), expected, "{owner:?}");
        assert!(consumer.pop_frame().unwrap().is_some(), "{owner:?}");
        assert_eq!(host.device("j").unwrap().state, DeviceState::Running);
    }
}

#[test]
fn a_host_owned_devices_stream_lends_no_buffer_to_hardware() {
    let dma = "memory = \"dma-buf\"\n";
    for (owner, handles) in [("owner = \"host\"", 0), ("", 1)] {
        let devices = format!(
            "[[devices]]\nid = \"j\"\ndriver = \"jobs\"\n{owner}\n\
             [devices.streams.frames]\nbuffers = 4\nbuffer_bytes = 64\n{dma}"
        );
        let (host, _) = host_on(SimBoard::new(), &devices, &[entry::<JobReader>()]);
        assert_eq!(read_u32(&host, 2), handles, "{owner:?}");
    }
}

#[test]
fn only_the_host_owned_callers_changes_are_refused() {
    // The fan is Lemnos's and takes a change from anyone else: what was refused above was the
    // controller's asking, not the fan.
    let tree = Dir::new("ctl-direct");
    let (host, _clock) = controller(&tree, "owner = \"host\"");
    let fan = host.device("fan").unwrap().index;
    let set_duty = MethodId {
        contract: 0,
        method: 1,
    };
    host.call(fan, set_duty, &500u16.to_le_bytes(), &mut [0; 8])
        .unwrap();
    assert_eq!(tree.read("class/hwmon/hwmon3/pwm1"), "128");
}
