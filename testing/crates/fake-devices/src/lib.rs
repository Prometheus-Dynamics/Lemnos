//! Drivers with no hardware behind them, for testing the host and the tools
//! built on it. Each reads its value from `config`.

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use lemnos_contract_accelerometer::Accelerometer;
use lemnos_contract_gyroscope::Gyroscope;
use lemnos_contract_magnetometer::Magnetometer;

pub mod probe;
use lemnos_core::{
    Config, ConfigError, Description, Item, Key, Kind, Lifecycle, MethodId, Result, Status, Unit,
    Vector3, contract, driver,
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
                ])
            }

            fn start(_needs: $needs, cfg: &Config<'_>) -> Result<Self> {
                if cfg.bool("refuse").unwrap_or(false) {
                    return Err(Status::IO);
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
