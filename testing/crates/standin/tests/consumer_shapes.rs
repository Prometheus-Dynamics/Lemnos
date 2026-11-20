//! What a consumer's tests did against the old mock daemon, done against the stand-in: devices and
//! readings, the status light and its read-back, a fan override undone when its client goes, raw
//! claims with edges, transactions and refusals, a claim ended with its client and on a restart,
//! and requests failing cleanly with no service. Each is the real service's answer, so the refusals
//! here are the ones a board gives.

use std::time::{Duration, Instant};

use lemnos_client::Client;
use lemnos_contract_fan::Fan;
use lemnos_contract_light::Light;
use lemnos_contract_raw_bus::{
    BIAS_PULL_UP, DRIVE_PUSH_PULL, EDGE_BOTH, EDGE_BYTES, EDGE_NONE, I2cOp, LINE_INPUT,
    LINE_OUTPUT, MAX_EDGES, RawBus, decode_edge, encode_i2c_ops,
};
use lemnos_contract_temperature::Temperature;
use lemnos_core::Status;
use lemnos_hal::raw::LineConfig;
use lemnos_sim::Layout;
use lemnos_standin::{FakeSys, SimBoard, StandIn};

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for: {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// A board like a small controller's: a fan and a CPU zone (fake sysfs), a status ring on an SPI
/// strip, a spare line and a PWM channel for raw use, and an I2C bus with one target.
struct Rig {
    // Dropped first: the service stops before its directory goes.
    standin: StandIn,
    _dir: Dir,
    line: lemnos_standin::mock::MockLine,
    pwm: lemnos_standin::mock::MockPwm,
    fan: lemnos_standin::FanFiles,
    zone: lemnos_standin::Zone,
}

/// The test's directory, removed when the rig drops.
struct Dir(std::path::PathBuf);

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn rig() -> Rig {
    let sim = SimBoard::new();
    let line = sim.add_line("aux-line");
    let pwm = sim.add_pwm("buzzer");
    let _strip = sim.add_spi("ring-spi");
    let i2c = sim.add_i2c("i2c-1");
    i2c.attach(0x50, Layout::Bytes);
    i2c.set_bytes(0x50, 0x00, &[0xaa, 0xbb]);
    // The fake sysfs goes in a directory made before the service starts, so the board can name it.
    let dir = StandIn::new_dir().unwrap();
    let sys = FakeSys::new(&dir).unwrap();
    let fan = sys.hwmon_fan("hwmon2", "pwmfan", 77, 2).unwrap();
    let zone = sys.thermal_zone(0, "cpu-thermal", 57_850).unwrap();
    let board = format!(
        "[[devices]]\nid = \"fan\"\ndriver = \"hwmon-fan\"\n\
         config = {{ name = \"pwmfan\", sys_root = \"{sys}\" }}\n\
         [[devices]]\nid = \"cpu-thermal\"\ndriver = \"thermal-zone\"\n\
         config = {{ path = \"{zone}\" }}\n\
         [[devices]]\nid = \"ring\"\ndriver = \"ws2812\"\nuses = {{ spi = \"ring-spi\" }}\n\
         config = {{ count = 4 }}\n\
         [[devices]]\nid = \"status\"\ndriver = \"status-light\"\nuses = {{ pixels = \"ring\" }}\n\
         config = {{ count = 4, fade_ms = 0 }}\n\
         [[devices]]\nid = \"aux\"\ndriver = \"raw-bus\"\n\
         uses = {{ line = \"aux-line\", pwm = \"buzzer\", i2c = \"i2c-1\" }}\n\
         config = {{ writes = true, allow_i2c = 0x50, line_safe = \"low\" }}\n",
        sys = sys.root().display(),
        zone = zone.dir().display(),
    );
    let standin = StandIn::start_in(&dir, &board, sim).unwrap();
    standin
        .wait_running(&["fan", "cpu-thermal", "ring", "status", "aux"])
        .unwrap();
    Rig {
        standin,
        _dir: Dir(dir),
        line,
        pwm,
        fan,
        zone,
    }
}

#[test]
fn devices_readings_and_the_light_read_back() {
    let rig = rig();
    let client = Client::connect(rig.standin.socket(), "consumer").unwrap();
    let names: Vec<String> = client.list().unwrap().into_iter().map(|d| d.name).collect();
    for name in ["fan", "cpu-thermal", "ring", "status", "aux"] {
        assert!(names.iter().any(|n| n == name), "{name} in {names:?}");
    }
    let mut zone = client.open::<dyn Temperature>("cpu-thermal").unwrap();
    assert!((zone.read().unwrap() - 57.85).abs() < 0.01);
    rig.zone.set(61_000).unwrap();
    assert!((zone.read().unwrap() - 61.0).abs() < 0.01);

    let mut light = client.open::<dyn Light>("status").unwrap();
    // A frame: for each LED its index (u16), then red, green, blue, white.
    let colours = [
        [10u8, 20, 30, 0],
        [40, 50, 60, 0],
        [70, 80, 90, 0],
        [100, 110, 120, 0],
    ];
    let mut frame = Vec::new();
    for (led, rgbw) in colours.iter().enumerate() {
        frame.extend_from_slice(&(led as u16).to_le_bytes());
        frame.extend_from_slice(rgbw);
    }
    light.show_frame(0, 1, &frame).unwrap();
    // `current`: generation (u64), count (u16), then 14 bytes per LED, its colour first.
    let mut shown = [0u8; 10 + 14 * 4];
    wait_for("the ring shows the frame", || {
        let n = light.current(&mut shown).unwrap();
        n == shown.len()
            && colours
                .iter()
                .enumerate()
                .all(|(led, rgbw)| shown[10 + 14 * led..14 + 14 * led] == rgbw[..])
    });
}

/// The override ends with its client: the fan goes back to what the kernel had (pwm1 77, automatic).
#[test]
fn a_fan_override_is_undone_when_its_client_goes() {
    let rig = rig();
    let client = Client::connect(rig.standin.socket(), "consumer").unwrap();
    let mut fan = client.open::<dyn Fan>("fan").unwrap();
    fan.set_duty(600).unwrap();
    assert_eq!(rig.fan.pwm1(), Some(153));
    assert_eq!(rig.fan.pwm1_enable(), Some(1));
    drop(fan);
    drop(client);
    wait_for("the fan is handed back", || {
        rig.fan.pwm1() == Some(77) && rig.fan.pwm1_enable() == Some(2)
    });
}

#[test]
fn raw_claims_edges_transactions_and_refusals() {
    let rig = rig();
    let owner = Client::connect(rig.standin.socket(), "owner").unwrap();
    let other = Client::connect(rig.standin.socket(), "other").unwrap();
    let mut mine = owner.open::<dyn RawBus>("aux").unwrap();
    let mut theirs = other.open::<dyn RawBus>("aux").unwrap();

    mine.claim().unwrap();
    assert_eq!(theirs.claim(), Err(Status::BUSY));
    assert_eq!(theirs.pwm_set(1000, 500, true), Err(Status::BUSY));

    // A register read, as a transaction list: pointer 0x00, then two bytes.
    let mut ops = [0u8; 32];
    let n = encode_i2c_ops(&[I2cOp::Write(&[0x00]), I2cOp::Read(2)], &mut ops).unwrap();
    let mut into = [0u8; 2];
    assert_eq!(mine.i2c_transactions(0x50, &ops[..n], &mut into), Ok(2));
    assert_eq!(into, [0xaa, 0xbb]);
    // An address the board didn't allow is refused before the bus.
    assert_eq!(
        mine.i2c_transactions(0x51, &ops[..n], &mut into),
        Err(Status::NOT_PERMITTED)
    );

    mine.line_configure(
        LINE_OUTPUT,
        true,
        false,
        BIAS_PULL_UP,
        DRIVE_PUSH_PULL,
        EDGE_NONE,
        0,
    )
    .unwrap();
    assert_eq!(rig.line.level(), Some(true));
    mine.line_configure(
        LINE_INPUT,
        false,
        false,
        BIAS_PULL_UP,
        DRIVE_PUSH_PULL,
        EDGE_BOTH,
        0,
    )
    .unwrap();
    rig.line.edge(true, 1_000);
    rig.line.edge(false, 2_000);
    let mut edges = [0u8; MAX_EDGES * EDGE_BYTES];
    let n = theirs.read_edges(&mut edges).unwrap();
    let seen: Vec<_> = edges[..n]
        .chunks(EDGE_BYTES)
        .map(|b| decode_edge(b).unwrap())
        .map(|e| (e.rising, e.timestamp_ns))
        .collect();
    assert_eq!(seen, [(true, 1_000), (false, 2_000)]);
}

/// A claim ends with its client, and a restart ends every claim: after it, the line is in its safe
/// state, another client claims, and the simulated parts kept their state.
#[test]
fn claims_end_with_their_client_and_on_a_restart() {
    let mut rig = rig();
    {
        let owner = Client::connect(rig.standin.socket(), "owner").unwrap();
        let mut aux = owner.open::<dyn RawBus>("aux").unwrap();
        aux.claim().unwrap();
        aux.line_configure(LINE_OUTPUT, true, false, 0, 0, EDGE_NONE, 0)
            .unwrap();
        aux.pwm_set(1000, 500, true).unwrap();
    }
    wait_for("the claim ends with its client", || {
        rig.line.config() == LineConfig::output(false) && !rig.pwm.current().enabled
    });

    let holder = Client::connect(rig.standin.socket(), "holder").unwrap();
    let mut aux = holder.open::<dyn RawBus>("aux").unwrap();
    aux.claim().unwrap();
    aux.line_configure(LINE_OUTPUT, true, false, 0, 0, EDGE_NONE, 0)
        .unwrap();
    rig.standin.restart().unwrap();
    rig.standin.wait_running(&["aux"]).unwrap();
    let after = Client::connect(rig.standin.socket(), "after").unwrap();
    let mut again = after.open::<dyn RawBus>("aux").unwrap();
    assert_eq!(again.claim(), Ok(()));
}

#[test]
fn requests_fail_cleanly_without_the_service() {
    let mut rig = rig();
    let client = Client::connect(rig.standin.socket(), "consumer").unwrap();
    let mut aux = client.open::<dyn RawBus>("aux").unwrap();
    rig.standin.stop();
    assert!(!rig.standin.is_running());
    assert!(aux.line_get().is_err());
    assert!(Client::connect(rig.standin.socket(), "late").is_err());
    assert!(
        rig.standin
            .wait_running_for(&["aux"], Duration::from_millis(50))
            .is_err()
    );
}

/// A device the board doesn't have, and a board that doesn't parse, are named.
#[test]
fn what_isnt_there_is_named() {
    let rig = rig();
    let error = rig
        .standin
        .wait_running_for(&["nope"], Duration::from_millis(50))
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("nope isn't running: not on the board"),
        "{error}"
    );
    assert!(matches!(
        StandIn::start("[[devices]\n", SimBoard::new()),
        Err(lemnos_standin::Error::Board(_))
    ));
}
