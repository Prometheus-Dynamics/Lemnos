//! The sensors power up on the host's ticks, with no client: the host ticks a device from its
//! bind, and each power-up step is a tick the driver asks for at the time the chip needs. The
//! simulated chips refuse transfers for the waits the datasheets give after a reset or power-on
//! write (`SimI2c::set_settle`), so a step that comes too early fails, as it does on a board.

use std::sync::Arc;
use std::time::Duration;

use lemnos_core::Health;
use lemnos_host::board::parse;
use lemnos_host::{Host, entry};
use lemnos_sim::{FaultScript, Layout, SimBoard, SimClock, SimI2c};

const BOARD: &str = r#"
format = "lemnos.board"
schema_version = 1
[board]
id = "reference"

[[devices]]
id = "imu"
driver = "bmi088"
bus = "pio-i2c:sda=8,scl=7"
config = { accel_rate = 200, gyro_rate = 200, fifo = false, accel_address = 0x18, gyro_address = 0x68 }

[[devices]]
id = "mag"
driver = "bmm150"
bus = "i2c-1"
config = { address = 0x10 }

[[devices]]
id = "power"
driver = "ina2xx"
bus = "i2c-1"
config = { chip = "ina238", address = 0x40, shunt_ohms = 0.010, max_current_a = 2.0 }
"#;

const DEVICES: [&str; 3] = ["imu", "mag", "power"];

/// The reference board's three sensors on simulated buses, with the waits the chips need. Returns
/// the host, the clock, the PIO bus, the i2c-1 bus, and i2c-1's fault script.
fn board() -> (Host, SimClock, SimI2c, SimI2c, FaultScript) {
    let sim = SimBoard::new();
    let clock = sim.clock();
    let pio = sim.add_i2c("pio-i2c:sda=8,scl=7");
    let i2c1 = sim.add_i2c("i2c-1");
    let faults = sim.faults("i2c-1").expect("i2c-1 is on the board");

    // BMI088: the accelerometer at 0x18 (chip ID 0x1e), the gyroscope at 0x68 (0x0f). The
    // accelerometer's soft reset (0x7e) takes 1 ms, its suspend-mode write (0x7c) 1 ms, its power
    // control (0x7d) 5 ms; the gyroscope's soft reset (0x14) 30 ms.
    pio.attach(0x18, Layout::Bytes);
    pio.set_bytes(0x18, 0x00, &[0x1e]);
    pio.set_settle(0x18, 0x7e, Duration::from_millis(1));
    pio.set_settle(0x18, 0x7c, Duration::from_millis(1));
    pio.set_settle(0x18, 0x7d, Duration::from_millis(5));
    pio.attach(0x68, Layout::Bytes);
    pio.set_bytes(0x68, 0x00, &[0x0f]);
    pio.set_settle(0x68, 0x14, Duration::from_millis(30));

    // BMM150 at 0x10: chip ID 0x32 at 0x40; its power-on write (0x4b) takes 3 ms.
    i2c1.attach(0x10, Layout::Bytes);
    i2c1.set_bytes(0x10, 0x40, &[0x32]);
    i2c1.set_settle(0x10, 0x4b, Duration::from_millis(3));

    // INA238 at 0x40: manufacturer ID 0x5449 at 0x3e, device ID 0x2381 at 0x3f.
    i2c1.attach(0x40, Layout::Words);
    i2c1.set_word(0x40, 0x3e, &[0x54, 0x49]);
    i2c1.set_word(0x40, 0x3f, &[0x23, 0x81]);

    let host = Host::start(
        &parse(BOARD).unwrap(),
        &[
            entry::<lemnos_driver_bmi088::Bmi088>(),
            entry::<lemnos_driver_bmm150::Bmm150>(),
            entry::<lemnos_driver_ina2xx::Ina2xx>(),
        ],
        Box::new(sim),
        Arc::new(clock.clone()),
        lemnos_test_options::options(),
    )
    .unwrap();
    (host, clock, pio, i2c1, faults)
}

fn health(host: &Host, id: &str) -> Health {
    host.device(id).unwrap().health
}

/// Runs what the daemon runs between wakes, for `for_ms` of simulated time, and returns when
/// each device first reported `Health::Ok`, measured from `start_ns`.
fn run(host: &Host, clock: &SimClock, for_ms: u64, start_ns: u64) -> [Option<u64>; 3] {
    let mut ok_at = [None; 3];
    for _ in 0..for_ms {
        host.run_ticks();
        for (slot, id) in ok_at.iter_mut().zip(DEVICES) {
            if slot.is_none() && health(host, id) == Health::Ok {
                *slot = Some(clock.now_ns() - start_ns);
            }
        }
        clock.advance(Duration::from_millis(1));
    }
    ok_at
}

const MS: u64 = 1_000_000;

/// The bound from bind when the chips answer their first steps: all three are `running ok`
/// within 250 ms, with no client call.
#[test]
fn the_three_sensors_reach_running_ok_within_the_bound_with_no_client() {
    let (host, clock, _pio, _i2c1, _faults) = board();
    let start = clock.now_ns();
    let ok_at = run(&host, &clock, 250, start);
    for (id, at) in DEVICES.iter().zip(ok_at) {
        let at = at.unwrap_or_else(|| {
            panic!(
                "{id} never reached running ok within 250 ms of bind, no client: {:?}",
                health(&host, id)
            )
        });
        assert!(at <= 250 * MS, "{id} took {} ms", at / MS);
    }
}

/// A step that fails once is retried by the tick, not by a client: the device is `Failed` with
/// the reason while it waits, and `running ok` after the retry (100 ms later).
#[test]
fn a_power_up_step_that_fails_once_is_retried_by_the_tick() {
    let (host, clock, _pio, _i2c1, faults) = board();
    faults.fail_next(lemnos_core::ErrorKind::Timeout);
    let start = clock.now_ns();
    let ok_at = run(&host, &clock, 400, start);
    for (id, at) in DEVICES.iter().zip(ok_at) {
        let at = at.unwrap_or_else(|| {
            panic!(
                "{id} did not recover from one failed step: {:?}",
                health(&host, id)
            )
        });
        assert!(at <= 400 * MS, "{id} took {} ms after one failure", at / MS);
    }
}

/// A chip that stops answering after it was found is retried under the failure rule: it is never
/// given up, the backoff is capped at 8 s, and it is `running ok` within the cap of its return.
#[test]
fn a_chip_that_stops_answering_is_retried_for_as_long_as_it_takes() {
    let (host, clock, _pio, i2c1, _faults) = board();
    i2c1.set_dead(0x40, true);
    // Dead for 20 s of simulated time: retried all along, never `running ok`, never given up.
    let mut degraded_for = 0;
    for _ in 0..20_000 {
        host.run_ticks();
        assert_ne!(
            health(&host, "power"),
            Health::Ok,
            "it cannot be ok while dead"
        );
        if matches!(health(&host, "power"), Health::Degraded(_)) {
            degraded_for += 1;
        }
        clock.advance(Duration::from_millis(1));
    }
    assert_eq!(
        degraded_for, 20_000,
        "it stays degraded, never failed for good"
    );
    i2c1.set_dead(0x40, false);
    let back = clock.now_ns();
    let ok_at = run(&host, &clock, 8_500, back);
    let at =
        ok_at[2].expect("the power monitor is retried and ok within the 8 s cap of its return");
    assert!(
        at <= 8_100 * MS,
        "the power monitor took {} ms after it returned",
        at / MS
    );
    assert_eq!(health(&host, "power"), Health::Ok);
}
