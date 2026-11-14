//! A stream's producer state covers what feeds it: its own device and every device that links to
//! it, directly or through others. The host knows nothing of what any of them is: a feeder-shaped
//! device that `link`s to a holder-shaped one that has the stream is only a `link` edge.

use std::sync::Arc;
use std::time::Duration;

use lemnos_core::{
    CallerId, Config, Health, Lifecycle, MethodId, Result, Status, StreamHandle, contract, driver,
};
use lemnos_hal::blocks::StreamFrame;
use lemnos_host::board::parse;
use lemnos_host::{DeviceState, Host, Options, entry};
use lemnos_sim::{SimBoard, SimClock};
use lemnos_wire::ring::{Consumer, ProducerWords, STATE_OPEN, producer};

const CONSUMER: CallerId = CallerId(7);

#[contract("example.node", version = 1)]
pub trait Node {
    /// Reports itself failed (it is recovering by itself) or working again.
    #[method(1)]
    fn set_failed(&mut self, failed: u32) -> Result<u32>;
    /// Breaks the way a driver does: it panics, and the host binds it again.
    #[method(2)]
    fn explode(&mut self) -> Result<u32>;
    /// Publishes one frame (a device that has a stream).
    #[method(3)]
    fn emit(&mut self) -> Result<u32>;
}

/// The device with the stream: holder-shaped.
#[driver("holder", provides = [Node])]
pub struct Holder {
    #[needs(free)]
    frames: StreamHandle,
    failed: bool,
}

/// A device with no stream, linked to another (a feeder; a second feeder links to the first).
#[driver("feeder", provides = [Node])]
pub struct Feeder {
    failed: bool,
}

impl Lifecycle for Holder {
    type Needs = HolderNeeds;
    const TICKS: bool = true;

    fn start(needs: HolderNeeds, _: &Config<'_>) -> Result<Self> {
        Ok(Self {
            frames: needs.frames,
            failed: false,
        })
    }

    fn health(&self) -> Health {
        if self.failed {
            Health::Failed("recovering")
        } else {
            Health::Ok
        }
    }

    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        Some(now_ns + 1_000)
    }
}

impl Lifecycle for Feeder {
    type Needs = FeederNeeds;
    const TICKS: bool = true;

    fn start(_: FeederNeeds, _: &Config<'_>) -> Result<Self> {
        Ok(Self { failed: false })
    }

    fn health(&self) -> Health {
        if self.failed {
            Health::Failed("recovering")
        } else {
            Health::Ok
        }
    }

    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        Some(now_ns + 1_000)
    }
}

impl Node for Holder {
    fn set_failed(&mut self, failed: u32) -> Result<u32> {
        self.failed = failed != 0;
        Ok(0)
    }

    fn explode(&mut self) -> Result<u32> {
        panic!("the device broke");
    }

    fn emit(&mut self) -> Result<u32> {
        let buffer = self
            .frames
            .next_buffer()
            .map_err(|_| Status::IO)?
            .ok_or(Status::AGAIN)?;
        self.frames
            .publish(StreamFrame {
                buffer,
                bytes: 8,
                sequence: 1,
                identity: 1,
                timestamp_ns: 5,
            })
            .map_err(|_| Status::IO)?;
        Ok(buffer)
    }
}

impl Node for Feeder {
    fn set_failed(&mut self, failed: u32) -> Result<u32> {
        self.failed = failed != 0;
        Ok(0)
    }

    fn explode(&mut self) -> Result<u32> {
        panic!("the device broke");
    }

    fn emit(&mut self) -> Result<u32> {
        Err(Status::NOT_SUPPORTED)
    }
}

/// r (0): the holder with the stream. s (1): links to r. l (2): links to s. u (3): a second
/// holder with a stream that nothing links to and that links to nothing.
const BOARD: &str = "
[[devices]]
id = \"r\"
driver = \"holder\"
[devices.streams.frames]
buffers = 2
buffer_bytes = 16
[[devices]]
id = \"s\"
driver = \"feeder\"
link = \"r\"
[[devices]]
id = \"l\"
driver = \"feeder\"
link = \"s\"
[[devices]]
id = \"u\"
driver = \"holder\"
[devices.streams.frames]
buffers = 2
buffer_bytes = 16
";

const R: u32 = 0;
const S: u32 = 1;
const L: u32 = 2;
const U: u32 = 3;

struct Rig {
    host: Host,
    clock: SimClock,
}

impl Rig {
    fn new() -> Self {
        let text =
            format!("format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n{BOARD}");
        let sim = SimBoard::new();
        let clock = sim.clock();
        let host = Host::start(
            &parse(&text).unwrap(),
            &[entry::<Holder>(), entry::<Feeder>()],
            Box::new(sim),
            Arc::new(clock.clone()),
            Options::default(),
        )
        .unwrap();
        Self { host, clock }
    }

    fn call(&self, device: u32, method: u16) -> Result<usize> {
        let id = MethodId {
            contract: 0,
            method,
        };
        self.host.call(device, id, &[], &mut [0u8; 8])
    }

    fn attach(&self, device: u32) -> Consumer {
        self.host
            .stream_attach(device, "frames", CONSUMER)
            .unwrap()
            .consumer()
            .unwrap()
    }

    /// Lets the drivers' ticks run, so each reads its own health.
    fn settle(&self) {
        self.clock.advance(Duration::from_millis(1));
        self.host.tick();
    }

    /// The device's own recovery begins (or ends), as its driver reports it.
    fn set_failed(&self, device: u32, failed: bool) {
        let id = MethodId {
            contract: 0,
            method: 1,
        };
        let args = u32::from(failed).to_le_bytes();
        self.host.call(device, id, &args, &mut [0u8; 8]).unwrap();
        self.settle();
    }

    fn explode(&self, device: u32) {
        assert!(self.call(device, 2).is_err());
        self.settle();
    }

    /// Brings every rebound device back: the clock goes past the backoff and the host retries.
    fn bring_back(&self, id: &str) {
        for _ in 0..20 {
            self.clock.advance(Duration::from_secs(10));
            self.host.tick();
            if self.host.device(id).unwrap().state == DeviceState::Running {
                self.settle();
                return;
            }
        }
        panic!("{id} did not come back");
    }
}

fn words(consumer: &Consumer) -> ProducerWords {
    consumer.producer().unwrap()
}

#[test]
fn an_upstream_device_recovering_stalls_the_stream_once_and_resumes_kept_before_a_frame() {
    let rig = Rig::new();
    let mut consumer = rig.attach(R);
    rig.call(R, 3).unwrap();
    assert!(consumer.pop_frame().unwrap().is_some());
    let before = words(&consumer);
    assert_eq!((before.state, before.edges), (producer::RUNNING, 0));

    rig.clock.advance(Duration::from_secs(2));
    rig.set_failed(S, true);
    let stalled = words(&consumer);
    assert_eq!((stalled.state, stalled.edges), (producer::RETRYING, 1));
    assert!(stalled.since_ns >= 2_000_000_000, "{stalled:?}");
    assert_eq!(consumer.state(), STATE_OPEN);
    assert!(consumer.pop_frame().unwrap().is_none(), "frames stop");
    // Still failing, ticked again: no second edge.
    rig.settle();
    rig.settle();
    assert_eq!(words(&consumer).edges, 1);

    rig.set_failed(S, false);
    let back = words(&consumer);
    assert_eq!(
        (back.state, back.edges, back.recovery),
        (producer::RUNNING, 2, producer::KEPT)
    );
    rig.call(R, 3).unwrap();
    assert!(
        consumer.pop_frame().unwrap().is_some(),
        "frames on the same attachment"
    );
    assert_eq!(rig.host.stream_stats(R, "frames").unwrap().attachments, 1);
}

#[test]
fn a_device_two_links_upstream_counts_too() {
    let rig = Rig::new();
    let consumer = rig.attach(R);
    rig.set_failed(L, true);
    assert_eq!(words(&consumer).state, producer::RETRYING);
    rig.set_failed(L, false);
    let back = words(&consumer);
    assert_eq!((back.edges, back.recovery), (2, producer::KEPT));
}

#[test]
fn an_upstream_device_the_host_rebinds_resumes_the_stream_lost() {
    let rig = Rig::new();
    let consumer = rig.attach(R);
    rig.explode(S);
    let stalled = words(&consumer);
    assert_eq!((stalled.state, stalled.edges), (producer::RETRYING, 1));
    rig.bring_back("s");
    let back = words(&consumer);
    assert_eq!(
        (back.state, back.edges, back.recovery),
        (producer::RUNNING, 2, producer::LOST)
    );
}

#[test]
fn the_producing_device_and_its_upstream_failing_one_after_the_other_are_one_stall() {
    let rig = Rig::new();
    let consumer = rig.attach(R);
    // The producing device first, then the feeder: the stall began with the first.
    rig.clock.advance(Duration::from_secs(1));
    rig.set_failed(R, true);
    let first = words(&consumer);
    rig.clock.advance(Duration::from_secs(2));
    rig.set_failed(S, true);
    let both = words(&consumer);
    assert_eq!((both.state, both.edges), (producer::RETRYING, 1));
    assert_eq!(both.since_ns, first.since_ns);
    assert!(both.since_ns < 2_000_000_000, "{both:?}");
    // One back: still stalled, no edge. Both back: one resume.
    rig.set_failed(R, false);
    assert_eq!(
        (words(&consumer).state, words(&consumer).edges),
        (producer::RETRYING, 1)
    );
    rig.set_failed(S, false);
    let back = words(&consumer);
    assert_eq!(
        (back.state, back.edges, back.recovery),
        (producer::RUNNING, 2, producer::KEPT)
    );

    // The feeder first, then the producing device, healed in the other order.
    rig.clock.advance(Duration::from_secs(1));
    rig.set_failed(S, true);
    let first = words(&consumer);
    rig.clock.advance(Duration::from_secs(2));
    rig.set_failed(R, true);
    assert_eq!(words(&consumer).since_ns, first.since_ns);
    assert_eq!(words(&consumer).edges, 3);
    rig.set_failed(S, false);
    assert_eq!(words(&consumer).state, producer::RETRYING);
    rig.set_failed(R, false);
    let back = words(&consumer);
    assert_eq!(
        (back.state, back.edges, back.recovery),
        (producer::RUNNING, 4, producer::KEPT)
    );
}

#[test]
fn the_stall_resumes_lost_when_either_device_was_rebound() {
    // The feeder rebound, the producing device recovering by itself.
    let rig = Rig::new();
    let consumer = rig.attach(R);
    rig.set_failed(R, true);
    rig.explode(S);
    rig.set_failed(R, false);
    assert_eq!(words(&consumer).state, producer::RETRYING);
    rig.bring_back("s");
    let back = words(&consumer);
    assert_eq!(
        (back.state, back.edges, back.recovery),
        (producer::RUNNING, 2, producer::LOST)
    );

    // The producing device rebound, the feeder recovering by itself and back first.
    let rig = Rig::new();
    let consumer = rig.attach(R);
    rig.set_failed(S, true);
    rig.explode(R);
    rig.set_failed(S, false);
    assert_eq!(words(&consumer).state, producer::RETRYING);
    rig.bring_back("r");
    let back = words(&consumer);
    assert_eq!(
        (back.state, back.edges, back.recovery),
        (producer::RUNNING, 2, producer::LOST)
    );
}

#[test]
fn a_stream_with_no_upstream_and_a_device_that_links_to_nothing_read_as_before() {
    let rig = Rig::new();
    let on_r = rig.attach(R);
    let on_u = rig.attach(U);
    // The feeder's recovery is no concern of a stream it does not feed.
    rig.set_failed(S, true);
    assert_eq!(words(&on_r).state, producer::RETRYING);
    assert_eq!(
        (words(&on_u).state, words(&on_u).edges),
        (producer::RUNNING, 0)
    );
    rig.set_failed(S, false);
    // A device with no upstream: its own failure is the stream's.
    rig.set_failed(U, true);
    assert_eq!(
        (words(&on_u).state, words(&on_u).edges),
        (producer::RETRYING, 1)
    );
    assert_eq!(words(&on_r).edges, 2, "r's stream does not follow u");
    rig.set_failed(U, false);
    let back = words(&on_u);
    assert_eq!((back.edges, back.recovery), (2, producer::KEPT));
}

#[test]
fn a_consumer_attaching_during_an_upstream_stall_reads_it() {
    let rig = Rig::new();
    rig.clock.advance(Duration::from_secs(3));
    rig.set_failed(S, true);
    let consumer = rig.attach(R);
    let stalled = words(&consumer);
    assert_eq!((stalled.state, stalled.edges), (producer::RETRYING, 1));
    assert!(stalled.since_ns >= 3_000_000_000, "{stalled:?}");
}

#[test]
fn a_driver_reporting_failed_is_never_rebound_for_it_so_what_it_holds_is_kept() {
    // `Health::Failed` is a driver recovering by itself (`Health::Gone` is the one that asks to be
    // bound again). However long it reports it, the host keeps the same instance: it stays running,
    // never enters a backoff, and its driver's own state survives to the recovery.
    let rig = Rig::new();
    let consumer = rig.attach(R);
    rig.set_failed(S, true);
    for _ in 0..30 {
        rig.clock.advance(Duration::from_secs(10));
        rig.host.tick();
        assert_eq!(rig.host.device("s").unwrap().state, DeviceState::Running);
    }
    assert_eq!(words(&consumer).edges, 1);
    // The same instance still holds the flag the test set on it: a new instance would read ok.
    rig.settle();
    assert_eq!(words(&consumer).state, producer::RETRYING);
    assert_eq!(
        rig.host.device("s").unwrap().health,
        Health::Failed("recovering")
    );
    rig.set_failed(S, false);
    assert_eq!(words(&consumer).recovery, producer::KEPT);
}
