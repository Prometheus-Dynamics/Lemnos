//! Drivers with no hardware behind them, for testing the host and the tools
//! built on it. Each reads its value from `config`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use lemnos_contract_fan::Fan;
use lemnos_contract_temperature::Temperature;

use lemnos_contract_accelerometer::Accelerometer;
use lemnos_contract_gyroscope::Gyroscope;
use lemnos_contract_magnetometer::Magnetometer;

pub mod probe;
use lemnos_core::{
    Config, ConfigError, Description, Item, Key, Kind, Lifecycle, MethodId, RefusalKind, Result,
    Status, Unit, Vector3, contract, driver,
};
pub use probe::FakeProbe;

/// `example.counter/1`: a contract the tools have no code for.
#[contract("example.counter", version = 1)]
pub trait Counter {
    #[method(1)]
    fn get(&mut self) -> Result<u32>;
    #[method(2)]
    fn set(&mut self, value: u32) -> Result<()>;
    /// The value, without counting.
    #[method(7)]
    fn peek(&mut self) -> Result<u32>;
    /// Panics, to exercise containment.
    #[method(3)]
    fn crash(&mut self) -> Result<()>;
    /// Never comes back until released, to exercise deadlines.
    #[method(4)]
    fn hang(&mut self, millis: u32) -> Result<()>;
    /// Fails like a bus that stopped answering.
    #[method(5)]
    fn fail(&mut self) -> Result<()>;
    #[method(6)]
    fn dump(&mut self, #[out] into: &mut [u8]) -> Result<usize>;
}

fn vector(cfg: &Config<'_>, scale: f32) -> Vector3 {
    let axis = |key| cfg.float(key).unwrap_or(0.0) as f32 * scale;
    Vector3 {
        x: axis("x"),
        y: axis("y"),
        z: axis("z"),
    }
}

macro_rules! fake_vector {
    ($name:ident, $needs:ident, $tag:literal, $contract:ident, $unit:ident, { $($extra:tt)* }) => {
        #[driver($tag, provides = [$contract])]
        pub struct $name {
            value: Vector3,
        }

        impl Lifecycle for $name {
            type Needs = $needs;

            fn check_config<'a>(cfg: &Config<'a>) -> core::result::Result<(), ConfigError<'a>> {
                cfg.check_keys(&[
                    Key::optional("x", Kind::Float),
                    Key::optional("y", Kind::Float),
                    Key::optional("z", Kind::Float),
                    Key::optional("refuse", Kind::Bool),
                    Key::optional("refusal", Kind::Str),
                ])
            }

            fn start(_needs: $needs, cfg: &Config<'_>) -> Result<Self> {
                if cfg.bool("refuse").unwrap_or(false) {
                    return Err(Status::IO);
                }
                // `refusal = "absent"` or `"failed"`: the bind is refused with that kind, as a driver
                // says it with `Config::refuse`. Any other word is a setting the driver does not take.
                if let Some(word) = cfg.str("refusal") {
                    let (status, kind) = match word {
                        "absent" => (Status::NO_DEVICE, RefusalKind::Absent),
                        "failed" => (Status::IO, RefusalKind::Failed),
                        _ => return Err(Status::INVALID_ARGUMENT),
                    };
                    return Err(cfg.refuse(status, kind, format_args!("fake refusal: {word}")));
                }
                Ok(Self {
                    value: vector(cfg, 1.0),
                })
            }

            fn describe(&self, out: &mut Description) {
                let read = MethodId {
                    contract: 0,
                    method: 1,
                };
                for (name, field) in [("x", "x"), ("y", "y"), ("z", "z")] {
                    out.add(Item::value(name, read).unit(Unit::$unit, 0).field(field).group($tag));
                }
            }
        }

        impl $contract for $name {
            fn read(&mut self) -> Result<Vector3> {
                Ok(self.value)
            }
            $($extra)*
        }
    };
}

// A fake has one value, not a stream, so it has no batches.
fake_vector!(
    FakeAccelerometer,
    FakeAccelerometerNeeds,
    "fake-accelerometer",
    Accelerometer,
    MetrePerSecondSquared,
    {
        fn read_batch(&mut self, _into: &mut [u8]) -> Result<usize> {
            Err(Status::NOT_SUPPORTED)
        }
    }
);
fake_vector!(
    FakeGyroscope,
    FakeGyroscopeNeeds,
    "fake-gyroscope",
    Gyroscope,
    RadianPerSecond,
    {
        fn read_batch(&mut self, _into: &mut [u8]) -> Result<usize> {
            Err(Status::NOT_SUPPORTED)
        }
    }
);
fake_vector!(
    FakeMagnetometer,
    FakeMagnetometerNeeds,
    "fake-magnetometer",
    Magnetometer,
    Tesla,
    {}
);

/// How many counters have bound, across the process.
pub static COUNTER_BINDS: AtomicU32 = AtomicU32::new(0);

#[driver("fake-counter", provides = [Counter])]
pub struct FakeCounter {
    value: u32,
}

impl Lifecycle for FakeCounter {
    type Needs = FakeCounterNeeds;

    fn check_config<'a>(cfg: &Config<'a>) -> core::result::Result<(), ConfigError<'a>> {
        cfg.check_keys(&[Key::optional("start", Kind::Int)])
    }

    fn start(_needs: FakeCounterNeeds, cfg: &Config<'_>) -> Result<Self> {
        COUNTER_BINDS.fetch_add(1, Ordering::Relaxed);
        Ok(Self {
            value: cfg.int("start").unwrap_or(0) as u32,
        })
    }

    fn describe(&self, out: &mut Description) {
        let id = |method| MethodId {
            contract: 0,
            method,
        };
        out.add(
            Item::value("count", id(1))
                .dimensionless()
                .range(0, i64::from(u32::MAX)),
        );
        out.add(Item::control("target", id(2)).dimensionless());
        out.add(Item::action("crash", id(3)));
    }
}

impl Counter for FakeCounter {
    fn get(&mut self) -> Result<u32> {
        self.value += 1;
        Ok(self.value)
    }

    fn set(&mut self, value: u32) -> Result<()> {
        self.value = value;
        Ok(())
    }

    fn peek(&mut self) -> Result<u32> {
        Ok(self.value)
    }

    fn crash(&mut self) -> Result<()> {
        panic!("deliberate driver panic");
    }

    fn hang(&mut self, millis: u32) -> Result<()> {
        std::thread::sleep(Duration::from_millis(u64::from(millis)));
        Ok(())
    }

    fn fail(&mut self) -> Result<()> {
        Err(Status::IO)
    }

    fn dump(&mut self, into: &mut [u8]) -> Result<usize> {
        let n = into.len().min(4);
        into[..n].copy_from_slice(&self.value.to_le_bytes()[..n]);
        Ok(n)
    }
}

/// A fake accelerometer with a FIFO, as the bmi088 driver's batches are shaped: one record per
/// sample, stamped with the host's clock, the sample period 5 ms, and the value from `config`.
/// `torn = true` answers every batch one byte too long, so a batch is never whole records: the
/// fault a capture must report. For the B8 tool's rehearsal (`testing/crates/bmi088-batch`).
#[driver("fake-fifo-accelerometer", provides = [Accelerometer])]
pub struct FakeFifoAccelerometer {
    value: Vector3,
    torn: bool,
    next_at: Option<u64>,
    queue: std::collections::VecDeque<lemnos_contract_accelerometer::Sample>,
}

const FIFO_PERIOD_NS: u64 = 5_000_000;
/// The accelerometer contract's `read` (method 1).
const ACCEL_READ: MethodId = MethodId {
    contract: 0,
    method: 1,
};
const FIFO_DEPTH: usize = 256;

impl Lifecycle for FakeFifoAccelerometer {
    type Needs = FakeFifoAccelerometerNeeds;
    const TICKS: bool = true;

    fn check_config<'a>(cfg: &Config<'a>) -> core::result::Result<(), ConfigError<'a>> {
        cfg.check_keys(&[
            Key::optional("x", Kind::Float),
            Key::optional("y", Kind::Float),
            Key::optional("z", Kind::Float),
            Key::optional("torn", Kind::Bool),
        ])
    }

    fn start(_needs: FakeFifoAccelerometerNeeds, cfg: &Config<'_>) -> Result<Self> {
        Ok(Self {
            value: vector(cfg, 1.0),
            torn: cfg.bool("torn").unwrap_or(false),
            next_at: None,
            queue: std::collections::VecDeque::new(),
        })
    }

    fn describe(&self, out: &mut Description) {
        out.add(
            Item::value("acceleration.x", ACCEL_READ)
                .unit(Unit::MetrePerSecondSquared, 0)
                .field("x"),
        );
    }

    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        let mut at = self.next_at.unwrap_or(now_ns);
        while at <= now_ns {
            if self.queue.len() == FIFO_DEPTH {
                self.queue.pop_front();
            }
            self.queue.push_back(lemnos_contract_accelerometer::Sample {
                at_ns: at,
                value: self.value,
            });
            at += FIFO_PERIOD_NS;
        }
        self.next_at = Some(at);
        Some(at)
    }
}

impl Accelerometer for FakeFifoAccelerometer {
    fn read(&mut self) -> Result<Vector3> {
        Ok(self.value)
    }

    fn read_batch(&mut self, into: &mut [u8]) -> Result<usize> {
        use lemnos_core::{Encode, Writer};
        let record = lemnos_contract_accelerometer::RECORD_LEN;
        let mut used = 0;
        while let Some(sample) = self.queue.front().copied() {
            if used + record > into.len() {
                break;
            }
            let mut writer = Writer::new(&mut into[used..used + record]);
            sample
                .encode(&mut writer)
                .map_err(|_| Status::INVALID_ARGUMENT)?;
            self.queue.pop_front();
            used += record;
        }
        if self.torn && used > 0 {
            // One byte more than whole records: the batch is not whole records, and a capture refuses it.
            used += 1;
        }
        Ok(used)
    }
}

/// Duty is per mille, as `fan/1` takes it.
const FAN_MAX: u16 = 1000;
/// The fan contract's `set_duty` (method 1) and `duty` (method 2), and `release` (method 4).
const FAN_SET_DUTY: MethodId = MethodId {
    contract: 0,
    method: 1,
};
const FAN_DUTY: MethodId = MethodId {
    contract: 0,
    method: 2,
};
const FAN_RELEASE: MethodId = MethodId {
    contract: 0,
    method: 4,
};

/// A fan on `fan/1` that takes a duty per mille and reports it back, the way `pwm-channel-fan`
/// and `hwmon-fan` do. It starts at full duty, its safe state, unless `duty` says otherwise, and
/// `release` puts it back there. `fail = true` refuses every write (`set_duty` and `release`)
/// with `IO`, as a hwmon attribute refuses one when the kernel says no, and the duty stays where
/// it was. For the B14 fan-control rehearsal (`testing/fan-control`) and its daemon test.
#[driver("fake-fan", provides = [Fan])]
pub struct FakeFan {
    duty: u16,
    fail: bool,
}

impl Lifecycle for FakeFan {
    type Needs = FakeFanNeeds;

    fn check_config<'a>(cfg: &Config<'a>) -> core::result::Result<(), ConfigError<'a>> {
        cfg.check_keys(&[
            Key::optional("duty", Kind::Int),
            Key::optional("fail", Kind::Bool),
        ])?;
        if let Some(duty) = cfg.int("duty")
            && !(0..=i64::from(FAN_MAX)).contains(&duty)
        {
            return Err(ConfigError::new("duty", "must be 0 to 1000"));
        }
        Ok(())
    }

    fn start(_needs: FakeFanNeeds, cfg: &Config<'_>) -> Result<Self> {
        let duty = cfg.int("duty").unwrap_or(i64::from(FAN_MAX)) as u16;
        Ok(Self {
            duty,
            fail: cfg.bool("fail").unwrap_or(false),
        })
    }

    fn describe(&self, out: &mut Description) {
        // Duty is a count of per mille: exponent 0. The control has its own name, so a `set`
        // finds it and not the read.
        out.add(
            Item::value("duty", FAN_DUTY)
                .unit(Unit::One, 0)
                .range(0, i64::from(FAN_MAX))
                .step(1),
        );
        out.add(
            Item::control("set_duty", FAN_SET_DUTY)
                .unit(Unit::One, 0)
                .range(0, i64::from(FAN_MAX))
                .step(1),
        );
        out.add(Item::action("release", FAN_RELEASE));
    }
}

impl Fan for FakeFan {
    fn set_duty(&mut self, per_mille: u16) -> Result<()> {
        if per_mille > FAN_MAX {
            return Err(Status::INVALID_ARGUMENT);
        }
        if self.fail {
            return Err(Status::IO);
        }
        self.duty = per_mille;
        Ok(())
    }

    fn duty(&mut self) -> Result<u16> {
        Ok(self.duty)
    }

    fn rpm(&mut self) -> Result<Option<u32>> {
        // No tachometer on a stand-in.
        Ok(None)
    }

    fn release(&mut self) -> Result<()> {
        if self.fail {
            return Err(Status::IO);
        }
        self.duty = FAN_MAX;
        Ok(())
    }
}

/// A thermal zone on `temperature/1`, read the way a sysfs zone is: `path` is a directory with a
/// `temp` file in millidegrees Celsius, and each read opens it. A test or a rehearsal sets the
/// temperature by writing that file, and steps it by writing again. A missing file fails the read
/// with `NOT_FOUND`, which is what a missing `thermal_zoneN` gives. `fail = true` fails every read
/// with `IO`, as a zone that will not give a number does.
#[driver("fake-thermal-zone", provides = [Temperature])]
pub struct FakeThermalZone {
    temp: PathBuf,
    fail: bool,
}

/// The temperature attribute's read (`temperature/1`'s `read`, method 1).
const ZONE_READ: MethodId = MethodId {
    contract: 0,
    method: 1,
};

impl Lifecycle for FakeThermalZone {
    type Needs = FakeThermalZoneNeeds;

    fn check_config<'a>(cfg: &Config<'a>) -> core::result::Result<(), ConfigError<'a>> {
        cfg.check_keys(&[
            Key::required("path", Kind::Str),
            Key::optional("fail", Kind::Bool),
        ])
    }

    fn start(_needs: FakeThermalZoneNeeds, cfg: &Config<'_>) -> Result<Self> {
        let dir = cfg.str("path").ok_or(Status::INVALID_ARGUMENT)?;
        Ok(Self {
            temp: PathBuf::from(dir).join("temp"),
            fail: cfg.bool("fail").unwrap_or(false),
        })
    }

    fn describe(&self, out: &mut Description) {
        out.add(Item::value("temperature", ZONE_READ).unit(Unit::DegreeCelsius, 0));
    }
}

impl Temperature for FakeThermalZone {
    fn read(&mut self) -> Result<f32> {
        if self.fail {
            return Err(Status::IO);
        }
        let text = std::fs::read_to_string(&self.temp).map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => Status::NOT_FOUND,
            std::io::ErrorKind::PermissionDenied => Status::ACCESS,
            _ => Status::IO,
        })?;
        let milli: i32 = text.trim().parse().map_err(|_| Status::IO)?;
        Ok(milli as f32 / 1000.0)
    }
}
