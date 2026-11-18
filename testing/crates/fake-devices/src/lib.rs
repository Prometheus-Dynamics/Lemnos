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
        fn read_batch_stamped(&mut self, _into: &mut [u8]) -> Result<usize> {
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
        fn read_batch_stamped(&mut self, _into: &mut [u8]) -> Result<usize> {
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

/// The accelerometer contract's `read` (method 1), which the gyroscope's read shares.
const ACCEL_READ: MethodId = MethodId {
    contract: 0,
    method: 1,
};

/// The sample period of a FIFO at 200 Hz, in nanoseconds: what the stamps are spaced by.
const FIFO_PERIOD_NS: u64 = 5_000_000;
/// The drain period the driver's tick uses: half the FIFO's depth at 200 Hz (73 frames, 365 ms).
const DRAIN_PERIOD_NS: u64 = 365_000_000;
const FIFO_DEPTH: usize = 256;

/// How a fake FIFO stamps its samples.
///
/// `Model` stamps each sample at its own time on the chip's clock, as the driver's sample clock does
/// once it has converged. `Tick` stamps the way the driver did before that model: each sample waits
/// for the next tick, and takes the tick's host time less the nominal periods back from the newest
/// sample, flagged as tick time (the record's mode byte is 1). Only the accelerometer has `Tick`: the
/// gyroscope has no sensor time, and its stamps are always its own clock's.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Stamps {
    Model,
    Tick,
}

/// One sample in a fake FIFO's queue.
#[derive(Clone, Copy)]
struct Queued {
    /// Sequence number, counted from the first sample the chip took.
    seq: u64,
    /// The stamp, once it has one: the time and whether it is tick time.
    stamp: Option<(u64, bool)>,
}

/// A fake FIFO's sample stream, shared by the fake accelerometer and gyroscope.
///
/// The chip takes a sample every `FIFO_PERIOD_NS / (1 + oscillator_ppm / 1e6)` on its own oscillator, from
/// the first tick. The FIFO is read at the time of the read (the samples up to the host's time, which
/// is the last tick's time plus the time since, measured on the monotonic clock), as the driver's read
/// does. `stamp_error_ppm` is a planted error on the model's stamps: a rate error the checks must catch,
/// and 0 for a correct model. `torn` answers every batch one byte too long, and `fail` refuses every
/// batch with `IO`: the faults a capture must report.
struct Fifo {
    value: Vector3,
    torn: bool,
    fail: bool,
    stamps: Stamps,
    true_period_ns: f64,
    stamp_factor: f64,
    /// The host time of the chip's first sample: the first tick's time (`None` until the first tick).
    origin: Option<u64>,
    /// Samples the chip has taken so far.
    taken: u64,
    queue: std::collections::VecDeque<Queued>,
    /// The last tick's host time, and when the tick ran (to read the host time between ticks).
    last_tick: Option<(u64, std::time::Instant)>,
}

impl Fifo {
    fn new(cfg: &Config<'_>, stamps: Stamps) -> Self {
        let ppm = cfg.float("oscillator_ppm").unwrap_or(0.0);
        let planted = cfg.float("stamp_error_ppm").unwrap_or(0.0);
        Self {
            value: vector(cfg, 1.0),
            torn: cfg.bool("torn").unwrap_or(false),
            fail: cfg.bool("fail").unwrap_or(false),
            stamps,
            true_period_ns: FIFO_PERIOD_NS as f64 / (1.0 + ppm / 1e6),
            stamp_factor: 1.0 + planted / 1e6,
            origin: None,
            taken: 0,
            queue: std::collections::VecDeque::new(),
            last_tick: None,
        }
    }

    /// The host's time now: the last tick, moved on by the time since it ran.
    fn now_ns(&self) -> u64 {
        self.last_tick
            .map(|(at, ran)| at.saturating_add(ran.elapsed().as_nanos() as u64))
            .unwrap_or(0)
    }

    /// The chip takes every sample up to `upto`, and each is stamped as the mode says.
    fn take_until(&mut self, upto: u64) {
        let Some(origin) = self.origin else { return };
        loop {
            let true_ns = origin + (self.taken as f64 * self.true_period_ns).round() as u64;
            if true_ns > upto {
                break;
            }
            if self.queue.len() == FIFO_DEPTH {
                self.queue.pop_front();
            }
            let stamp = match self.stamps {
                Stamps::Model => {
                    let at =
                        origin + ((true_ns - origin) as f64 * self.stamp_factor).round() as u64;
                    Some((at, false))
                }
                Stamps::Tick => None,
            };
            self.queue.push_back(Queued {
                seq: self.taken,
                stamp,
            });
            self.taken += 1;
        }
    }

    /// A tick: the first tick sets the chip's origin, the samples due are taken, and in tick mode the
    /// samples still waiting take this tick's time less the nominal periods back from the newest.
    fn tick(&mut self, now_ns: u64) {
        self.origin.get_or_insert(now_ns);
        self.last_tick = Some((now_ns, std::time::Instant::now()));
        self.take_until(now_ns);
        if self.stamps == Stamps::Tick {
            let Some(newest) = self.queue.back().map(|q| q.seq) else {
                return;
            };
            for entry in self.queue.iter_mut().filter(|q| q.stamp.is_none()) {
                let back = newest - entry.seq;
                let at = now_ns.saturating_sub(back.saturating_mul(FIFO_PERIOD_NS));
                entry.stamp = Some((at, true));
            }
        }
    }

    /// Writes the stamped samples that fit, oldest first: records of `RECORD_LEN` bytes, or with `stamped`
    /// records of `STAMPED_RECORD_LEN` bytes, each led by its mode byte (1 tick time, 0 the model).
    /// The FIFO is taken up to the read's time first, as the driver's read takes it.
    fn batch(&mut self, into: &mut [u8], stamped: bool) -> Result<usize> {
        use lemnos_core::{Encode, Writer};
        if self.fail {
            return Err(Status::IO);
        }
        let now = self.now_ns();
        self.take_until(now);
        let record = if stamped {
            lemnos_contract_accelerometer::STAMPED_RECORD_LEN
        } else {
            lemnos_contract_accelerometer::RECORD_LEN
        };
        let mut used = 0;
        while let Some(entry) = self.queue.front().copied() {
            // A sample waiting for the next tick ends the batch: the stamps are in order.
            let Some((at_ns, tick)) = entry.stamp else {
                break;
            };
            if used + record > into.len() {
                break;
            }
            let sample = lemnos_contract_accelerometer::Sample {
                at_ns,
                value: self.value,
            };
            let mut at = used;
            if stamped {
                into[at] = u8::from(tick);
                at += 1;
            }
            let mut writer = Writer::new(&mut into[at..used + record]);
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

/// A fake accelerometer with a FIFO, as the bmi088 driver's batches are shaped: one record per
/// sample, stamped from the host's clock, the sample period 5 ms nominal. Its config:
///
/// - `oscillator_ppm`: the chip's rate error, positive when the chip runs fast (default 0).
/// - `stamp_error_ppm`: a planted rate error on the model's stamps (default 0; a check must catch it).
/// - `stamps = "model"` (default) or `"tick"`: the driver's model, or the tick-time stamping it used
///   before the model converged (the pre-fix behaviour; see [`Stamps`]).
/// - `torn = true` and `fail = true`: as in [`Fifo`]. For the B8 tool's rehearsal
///   (`testing/crates/bmi088-batch`).
#[driver("fake-fifo-accelerometer", provides = [Accelerometer])]
pub struct FakeFifoAccelerometer {
    fifo: Fifo,
}

impl Lifecycle for FakeFifoAccelerometer {
    type Needs = FakeFifoAccelerometerNeeds;
    const TICKS: bool = true;

    fn check_config<'a>(cfg: &Config<'a>) -> core::result::Result<(), ConfigError<'a>> {
        cfg.check_keys(&[
            Key::optional("x", Kind::Float),
            Key::optional("y", Kind::Float),
            Key::optional("z", Kind::Float),
            Key::optional("torn", Kind::Bool),
            Key::optional("fail", Kind::Bool),
            Key::optional("oscillator_ppm", Kind::Float),
            Key::optional("stamp_error_ppm", Kind::Float),
            Key::optional("stamps", Kind::Str),
        ])?;
        match cfg.str("stamps") {
            None | Some("model") | Some("tick") => Ok(()),
            Some(_) => Err(ConfigError::new("stamps", "must be \"model\" or \"tick\"")),
        }
    }

    fn start(_needs: FakeFifoAccelerometerNeeds, cfg: &Config<'_>) -> Result<Self> {
        let stamps = match cfg.str("stamps") {
            Some("tick") => Stamps::Tick,
            _ => Stamps::Model,
        };
        Ok(Self {
            fifo: Fifo::new(cfg, stamps),
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
        self.fifo.tick(now_ns);
        Some(now_ns.saturating_add(DRAIN_PERIOD_NS))
    }
}

impl Accelerometer for FakeFifoAccelerometer {
    fn read(&mut self) -> Result<Vector3> {
        Ok(self.fifo.value)
    }

    fn read_batch(&mut self, into: &mut [u8]) -> Result<usize> {
        self.fifo.batch(into, false)
    }

    fn read_batch_stamped(&mut self, into: &mut [u8]) -> Result<usize> {
        self.fifo.batch(into, true)
    }
}

/// A fake gyroscope with a FIFO, the same shape as the accelerometer's. It has no sensor time, so its
/// stamps are its own clock's model (never tick time): the chip takes its samples on its own oscillator
/// (`oscillator_ppm`), stamped on the host's clock, and a planted `stamp_error_ppm` is as for the
/// accelerometer. `torn` and `fail` are as on the accelerometer. For the B8 gyroscope rehearsal, and check 7
/// (the spread of the offset between the two dies' stamps).
#[driver("fake-fifo-gyroscope", provides = [Gyroscope])]
pub struct FakeFifoGyroscope {
    fifo: Fifo,
}

impl Lifecycle for FakeFifoGyroscope {
    type Needs = FakeFifoGyroscopeNeeds;
    const TICKS: bool = true;

    fn check_config<'a>(cfg: &Config<'a>) -> core::result::Result<(), ConfigError<'a>> {
        cfg.check_keys(&[
            Key::optional("x", Kind::Float),
            Key::optional("y", Kind::Float),
            Key::optional("z", Kind::Float),
            Key::optional("torn", Kind::Bool),
            Key::optional("fail", Kind::Bool),
            Key::optional("oscillator_ppm", Kind::Float),
            Key::optional("stamp_error_ppm", Kind::Float),
        ])
    }

    fn start(_needs: FakeFifoGyroscopeNeeds, cfg: &Config<'_>) -> Result<Self> {
        Ok(Self {
            fifo: Fifo::new(cfg, Stamps::Model),
        })
    }

    fn describe(&self, out: &mut Description) {
        out.add(
            Item::value("angular_rate.x", ACCEL_READ)
                .unit(Unit::RadianPerSecond, 0)
                .field("x"),
        );
    }

    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        self.fifo.tick(now_ns);
        Some(now_ns.saturating_add(DRAIN_PERIOD_NS))
    }
}

impl Gyroscope for FakeFifoGyroscope {
    fn read(&mut self) -> Result<Vector3> {
        Ok(self.fifo.value)
    }

    fn read_batch(&mut self, into: &mut [u8]) -> Result<usize> {
        self.fifo.batch(into, false)
    }

    fn read_batch_stamped(&mut self, into: &mut [u8]) -> Result<usize> {
        self.fifo.batch(into, true)
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
/// and `hwmon-fan` do. It starts at its configured `duty`, the duty the fan was at before any driver
/// took it (the baseline of a real hwmon fan, such as pwm1 179 on the B14 board, which is 702 per
/// mille). The first write takes the fan over and records that baseline. `release` puts the fan at
/// full duty, its safe state, as hwmon's and pwm-channel-fan's do; a hand-back puts back the baseline.
/// `file`, when given, keeps the duty in that file as decimal per mille (as a sysfs attribute keeps
/// its value), so a test can read it after the daemon has stopped. A fan that is not taken at drop
/// goes to full duty, as hwmon does. `fail = true` refuses every write with `IO`, as a hwmon attribute
/// refuses one when the kernel says no, and the duty stays where it was. For the B14 fan-control
/// rehearsal (`testing/fan-control`) and its daemon tests.
#[driver("fake-fan", provides = [Fan])]
pub struct FakeFan {
    duty: u16,
    /// The duty when the fan was taken over: `None` until the first write.
    baseline: Option<u16>,
    file: Option<PathBuf>,
    fail: bool,
}

impl FakeFan {
    /// Sets the duty, and the file that keeps it. The file is written first, so a refused write
    /// leaves the duty where it was.
    fn put(&mut self, per_mille: u16) -> Result<()> {
        if let Some(path) = &self.file {
            std::fs::write(path, format!("{per_mille}\n")).map_err(|_| Status::IO)?;
        }
        self.duty = per_mille;
        Ok(())
    }
}

impl FakeFan {
    /// A refused duty write is logged beside the file, with its time in unix milliseconds, so a run can read
    /// the controller's retry schedule (B14 check 11) after the fact. Only the controller's writes are logged,
    /// not the release or hand-back writes.
    fn log_refused(&self) {
        use std::io::Write;
        let Some(path) = &self.file else { return };
        let log = format!("{}.failures", path.display());
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(log) {
            let ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0);
            let _ = writeln!(f, "{ms}");
        }
    }
}

impl Lifecycle for FakeFan {
    type Needs = FakeFanNeeds;

    fn check_config<'a>(cfg: &Config<'a>) -> core::result::Result<(), ConfigError<'a>> {
        cfg.check_keys(&[
            Key::optional("duty", Kind::Int),
            Key::optional("fail", Kind::Bool),
            Key::optional("file", Kind::Str),
        ])?;
        if let Some(duty) = cfg.int("duty")
            && !(0..=i64::from(FAN_MAX)).contains(&duty)
        {
            return Err(ConfigError::new("duty", "must be 0 to 1000"));
        }
        Ok(())
    }

    fn start(_needs: FakeFanNeeds, cfg: &Config<'_>) -> Result<Self> {
        let configured = cfg.int("duty").unwrap_or(i64::from(FAN_MAX)) as u16;
        let file = cfg.str("file").map(PathBuf::from);
        // A file that is there holds the fan's duty from before, as a sysfs attribute does.
        let duty = match file.as_ref().map(std::fs::read_to_string) {
            Some(Ok(text)) => text.trim().parse().unwrap_or(configured),
            _ => configured,
        };
        let mut fan = Self {
            duty,
            baseline: None,
            file,
            fail: cfg.bool("fail").unwrap_or(false),
        };
        let at = fan.duty;
        fan.put(at)?;
        Ok(fan)
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
        self.baseline.get_or_insert(self.duty);
        let written = self.put(per_mille);
        if written.is_err() {
            self.log_refused();
        }
        written
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
        if self.baseline.is_none() {
            return Ok(());
        }
        self.put(FAN_MAX)
    }

    fn hand_back(&mut self) -> Result<()> {
        if self.fail {
            return Err(Status::IO);
        }
        // Nothing taken, nothing to put back. The baseline is forgotten once it is back.
        let Some(baseline) = self.baseline else {
            return Ok(());
        };
        self.put(baseline)?;
        self.baseline = None;
        Ok(())
    }
}

impl Drop for FakeFan {
    fn drop(&mut self) {
        // As hwmon-fan: a fan a driver took and let go of is left at full duty. Nothing can report
        // an error from here.
        if self.baseline.is_some() {
            let _ = self.put(FAN_MAX);
        }
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
