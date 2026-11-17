//! A rebound driver's resume record: what the host keeps when it lets go of a driver to bind it
//! again (`Driver::resume_record`), and whether the stream reads `Kept` or `Lost` once the device
//! is back. The stand-in keeps one value; a record it cannot restore is the way a real rebind
//! fails to carry on (the state no longer matches the hardware), and the stream then reads `Lost`.

use std::sync::Arc;
use std::time::Duration;

use lemnos_core::{
    CallerId, Config, Health, Lifecycle, MethodId, Result, ResumeRecord, StreamHandle, contract,
    driver,
};
use lemnos_host::board::parse;
use lemnos_host::{DeviceState, Host, Options, entry};
use lemnos_sim::{SimBoard, SimClock};
use lemnos_wire::ring::{Consumer, ProducerWords, producer};

const CONSUMER: CallerId = CallerId(7);

/// A value the stand-in's record cannot be restored from.
pub const UNRESTORABLE: u32 = u32::MAX;

#[contract("example.keeper", version = 1)]
pub trait Keeper {
    /// Sets the value the record keeps.
    #[method(1)]
    fn set(&mut self, value: u32) -> Result<u32>;
    /// Reads the value this instance holds.
    #[method(2)]
    fn read(&mut self) -> Result<u32>;
    /// The handle goes dead for good: the host lets go of this instance and binds it again.
    #[method(3)]
    fn go_gone(&mut self) -> Result<u32>;
    /// Breaks the way a driver does: it panics, and the host binds it again.
    #[method(4)]
    fn explode(&mut self) -> Result<u32>;
}

/// The device with a stream and one value of state.
#[driver("keeper", provides = [Keeper])]
pub struct Keeping {
    #[needs(free)]
    frames: StreamHandle,
    value: u32,
    gone: bool,
}

impl Lifecycle for Keeping {
    type Needs = KeepingNeeds;
    const TICKS: bool = true;

    fn start(needs: KeepingNeeds, _: &Config<'_>) -> Result<Self> {
        Ok(Self {
            frames: needs.frames,
            value: 0,
            gone: false,
        })
    }

    fn health(&self) -> Health {
        if self.gone {
            Health::Gone("the handle is dead")
        } else {
            Health::Ok
        }
    }

    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        let _ = &self.frames;
        Some(now_ns + 1_000)
    }

    fn resume_record(&self) -> Option<ResumeRecord> {
        ResumeRecord::new(&self.value.to_le_bytes())
    }

    fn resume(&mut self, record: &ResumeRecord) -> bool {
        let Ok(bytes) = <[u8; 4]>::try_from(record.bytes()) else {
            return false;
        };
        let value = u32::from_le_bytes(bytes);
        if value == UNRESTORABLE {
            return false;
        }
        self.value = value;
        true
    }
}

impl Keeper for Keeping {
    fn set(&mut self, value: u32) -> Result<u32> {
        self.value = value;
        Ok(value)
    }

    fn read(&mut self) -> Result<u32> {
        Ok(self.value)
    }

    fn go_gone(&mut self) -> Result<u32> {
        self.gone = true;
        Ok(0)
    }

    fn explode(&mut self) -> Result<u32> {
        panic!("the device broke");
    }
}

const BOARD: &str = "
format = \"lemnos.board\"
schema_version = 1
[board]
id = \"t\"
[[devices]]
id = \"k\"
driver = \"keeper\"
[devices.streams.frames]
buffers = 2
buffer_bytes = 16
";

pub struct Rig {
    pub host: Host,
    pub clock: SimClock,
}

impl Rig {
    fn new() -> Self {
        let sim = SimBoard::new();
        let clock = sim.clock();
        let host = Host::start(
            &parse(BOARD).unwrap(),
            &[entry::<Keeping>()],
            Box::new(sim),
            Arc::new(clock.clone()),
            Options::default(),
        )
        .unwrap();
        Self { host, clock }
    }

    fn call(&self, method: u16, args: &[u8]) -> Result<u32> {
        let id = MethodId {
            contract: 0,
            method,
        };
        let mut reply = [0u8; 8];
        self.host.call(0, id, args, &mut reply)?;
        Ok(u32::from_le_bytes(reply[..4].try_into().unwrap()))
    }

    pub fn attach(&self) -> Consumer {
        self.host
            .stream_attach(0, "frames", CONSUMER)
            .unwrap()
            .consumer()
            .unwrap()
    }

    pub fn set(&self, value: u32) {
        self.call(1, &value.to_le_bytes()).unwrap();
        self.settle();
    }

    /// What the instance holds now. A value the instance does not hold (a fresh start) reads 0.
    pub fn read(&self) -> u32 {
        self.call(2, &[]).unwrap()
    }

    /// The handle goes dead: the host rebinds the device, and the stream stalls.
    pub fn go_gone(&self) {
        self.call(3, &[]).unwrap();
        self.settle();
    }

    pub fn explode(&self) {
        assert!(self.call(4, &[]).is_err());
        self.settle();
    }

    /// Lets the ticks run, so each driver reads its own health.
    pub fn settle(&self) {
        self.clock.advance(Duration::from_millis(1));
        self.host.tick();
    }

    /// Brings the rebound device back: the clock goes past its backoff and the host retries.
    pub fn bring_back(&self) {
        for _ in 0..20 {
            self.clock.advance(Duration::from_secs(10));
            self.host.tick();
            if self.host.device("k").unwrap().state == DeviceState::Running {
                self.settle();
                return;
            }
        }
        panic!("the device did not come back");
    }

    pub fn off(&self) {
        self.host.off("k").unwrap();
        self.settle();
    }

    pub fn on(&self) {
        self.host.on("k").unwrap();
        self.settle();
    }
}

pub fn words(consumer: &Consumer) -> ProducerWords {
    consumer.producer().unwrap()
}

#[test]
fn a_rebound_driver_keeps_its_record_and_the_stream_resumes_kept_with_its_state() {
    let rig = Rig::new();
    let consumer = rig.attach();
    rig.set(7);
    rig.go_gone();
    let stalled = words(&consumer);
    assert_eq!((stalled.state, stalled.edges), (producer::RETRYING, 1));
    rig.bring_back();
    let back = words(&consumer);
    assert_eq!(
        (back.state, back.edges, back.recovery),
        (producer::RUNNING, 2, producer::KEPT)
    );
    assert_eq!(
        rig.read(),
        7,
        "the new instance carries the state the old one kept"
    );

    // Kept again on a second rebind: the record is made afresh from the instance that is bound.
    rig.go_gone();
    rig.bring_back();
    let again = words(&consumer);
    assert_eq!(
        (again.state, again.edges, again.recovery),
        (producer::RUNNING, 4, producer::KEPT)
    );
    assert_eq!(rig.read(), 7);
}

#[test]
fn a_record_the_new_instance_cannot_restore_reads_lost_and_the_instance_is_fresh() {
    let rig = Rig::new();
    let consumer = rig.attach();
    rig.set(UNRESTORABLE);
    rig.go_gone();
    rig.bring_back();
    let back = words(&consumer);
    assert_eq!(
        (back.state, back.edges, back.recovery),
        (producer::RUNNING, 2, producer::LOST)
    );
    assert_eq!(rig.read(), 0, "a fresh start holds no value");
}

#[test]
fn a_driver_that_panics_keeps_nothing_and_the_stream_resumes_lost() {
    let rig = Rig::new();
    let consumer = rig.attach();
    rig.set(7);
    rig.explode();
    assert_eq!(words(&consumer).state, producer::RETRYING);
    rig.bring_back();
    let back = words(&consumer);
    assert_eq!(
        (back.state, back.edges, back.recovery),
        (producer::RUNNING, 2, producer::LOST)
    );
    assert_eq!(rig.read(), 0);
}

#[test]
fn a_switch_off_and_on_is_a_fresh_start_so_a_kept_record_does_not_carry_over() {
    let rig = Rig::new();
    let consumer = rig.attach();
    rig.set(7);
    rig.go_gone();
    assert_eq!(words(&consumer).state, producer::RETRYING);
    // Switched off by hand while the rebind waits: what it kept is let go.
    rig.off();
    rig.on();
    let back = words(&consumer);
    assert_eq!(
        (back.state, back.edges, back.recovery),
        (producer::RUNNING, 2, producer::LOST),
        "a switch on is not a resume"
    );
    assert_eq!(rig.read(), 0);
}
