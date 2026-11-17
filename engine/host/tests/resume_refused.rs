//! A rebind whose record waits for a bind that fails: the failed binds keep the record, and the
//! bind that succeeds takes it up. This binary holds its own refusal counter, so no other test
//! sees a bind refused.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use lemnos_core::{
    CallerId, Config, Health, Lifecycle, MethodId, Result, ResumeRecord, Status, StreamHandle,
    contract, driver,
};
use lemnos_host::board::parse;
use lemnos_host::{DeviceState, Host, Options, entry};
use lemnos_sim::{SimBoard, SimClock};
use lemnos_wire::ring::{Consumer, ProducerWords, producer};

const CONSUMER: CallerId = CallerId(7);

/// How many of the next binds are refused, as a device that is not there yet is.
static REFUSALS: AtomicU32 = AtomicU32::new(0);

#[contract("example.keeper", version = 1)]
pub trait Keeper {
    #[method(1)]
    fn set(&mut self, value: u32) -> Result<u32>;
    #[method(2)]
    fn read(&mut self) -> Result<u32>;
    #[method(3)]
    fn go_gone(&mut self) -> Result<u32>;
}

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
        let left = REFUSALS.load(Ordering::SeqCst);
        let refused = left > 0
            && REFUSALS
                .compare_exchange(left, left - 1, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok();
        if refused {
            return Err(Status::NO_DEVICE);
        }
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
        self.value = u32::from_le_bytes(bytes);
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

fn start() -> (Host, SimClock) {
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
    (host, clock)
}

fn call(host: &Host, method: u16, args: &[u8]) -> Result<u32> {
    let id = MethodId {
        contract: 0,
        method,
    };
    let mut reply = [0u8; 8];
    host.call(0, id, args, &mut reply)?;
    Ok(u32::from_le_bytes(reply[..4].try_into().unwrap()))
}

fn words(consumer: &Consumer) -> ProducerWords {
    consumer.producer().unwrap()
}

#[test]
fn a_bind_that_fails_keeps_the_record_for_the_bind_that_succeeds() {
    let (host, clock) = start();
    let consumer = host
        .stream_attach(0, "frames", CONSUMER)
        .unwrap()
        .consumer()
        .unwrap();
    call(&host, 1, &7u32.to_le_bytes()).unwrap();
    call(&host, 3, &[]).unwrap();
    host.tick();
    assert_eq!(words(&consumer).state, producer::RETRYING);

    // The next two binds are refused: the record must survive both.
    REFUSALS.store(2, Ordering::SeqCst);
    for _ in 0..20 {
        clock.advance(Duration::from_secs(10));
        host.tick();
        if host.device("k").unwrap().state == DeviceState::Running {
            break;
        }
    }
    assert_eq!(host.device("k").unwrap().state, DeviceState::Running);
    assert_eq!(
        REFUSALS.load(Ordering::SeqCst),
        0,
        "both refusals were used"
    );
    clock.advance(Duration::from_millis(1));
    host.tick();
    let back = words(&consumer);
    assert_eq!(
        (back.state, back.edges, back.recovery),
        (producer::RUNNING, 2, producer::KEPT),
        "the bind that succeeded took up the record the refused binds kept"
    );
    assert_eq!(call(&host, 2, &[]).unwrap(), 7);
}
