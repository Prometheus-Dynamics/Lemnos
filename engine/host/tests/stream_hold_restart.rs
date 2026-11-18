//! A frame a consumer holds stays as it was taken across a recovery restart of its producer: the
//! driver panics and the host binds a new instance, twice, while the consumer keeps two frames of a
//! four-buffer stream. No kept frame's buffer is granted to the producer again, and its bytes and
//! its metadata read back unchanged, until the consumer gives it back.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use lemnos_core::{
    CallerId, Config, Lifecycle, MethodId, Result, Status, StreamHandle, contract, driver,
};
use lemnos_hal::blocks::StreamFrame;
use lemnos_host::board::parse;
use lemnos_host::{DeviceState, Host, Options, entry};
use lemnos_sim::{SimBoard, SimClock};
use lemnos_wire::ring::{Consumer, FrameSlot, STATE_OPEN};

const CONSUMER: CallerId = CallerId(7);
const BUFFERS: u32 = 4;
const FRAME_BYTES: usize = 16;

/// Frames painted so far, by every instance of the driver: a rebound instance carries the count on,
/// so each frame's number and fill are its own.
static PAINTED: AtomicU32 = AtomicU32::new(0);

#[contract("example.painter", version = 1)]
pub trait Painting {
    /// Fills the next buffer with the frame's number and publishes it. Answers the buffer in the
    /// high half and the number in the low half; `AGAIN` when the stream has no buffer for it.
    #[method(1)]
    fn paint(&mut self) -> Result<u32>;
    /// Fails the way a driver does when it breaks: it panics, and the host binds it again.
    #[method(2)]
    fn explode(&mut self) -> Result<u32>;
}

#[driver("painter", provides = [Painting])]
pub struct Painter {
    #[needs(free)]
    frames: StreamHandle,
}

impl Lifecycle for Painter {
    type Needs = PainterNeeds;

    fn start(needs: PainterNeeds, _: &Config<'_>) -> Result<Self> {
        Ok(Self {
            frames: needs.frames,
        })
    }
}

impl Painting for Painter {
    fn paint(&mut self) -> Result<u32> {
        let buffer = self
            .frames
            .next_buffer()
            .map_err(|_| Status::IO)?
            .ok_or(Status::AGAIN)?;
        let number = PAINTED.fetch_add(1, Ordering::SeqCst) + 1;
        let fill = [number as u8; FRAME_BYTES];
        self.frames
            .write(buffer, 0, &fill)
            .map_err(|_| Status::IO)?;
        self.frames
            .publish(StreamFrame {
                buffer,
                bytes: FRAME_BYTES as u32,
                sequence: u64::from(number),
                identity: 0,
                timestamp_ns: u64::from(number) * 1000,
                end_ns: 0,
            })
            .map_err(|_| Status::IO)?;
        Ok(buffer << 16 | (number & 0xffff))
    }

    fn explode(&mut self) -> Result<u32> {
        panic!("the device broke");
    }
}

fn start() -> (Host, SimClock) {
    let text = format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
         [[devices]]\nid = \"p\"\ndriver = \"painter\"\n\
         [devices.streams.frames]\nbuffers = {BUFFERS}\nbuffer_bytes = {FRAME_BYTES}\n"
    );
    let sim = SimBoard::new();
    let clock = sim.clock();
    let handle = clock.clone();
    let host = Host::start(
        &parse(&text).unwrap(),
        &[entry::<Painter>()],
        Box::new(sim),
        Arc::new(clock),
        Options::default(),
    )
    .unwrap();
    (host, handle)
}

/// One `paint` call: the buffer and the frame's number, or `None` when the stream had no buffer.
fn paint(host: &Host) -> Option<(u32, u32)> {
    let method = MethodId {
        contract: 0,
        method: 1,
    };
    let mut reply = [0u8; 8];
    match host.call(0, method, &[], &mut reply) {
        Ok(_) => {
            let answer = u32::from_le_bytes(reply[..4].try_into().unwrap());
            Some((answer >> 16, answer & 0xffff))
        }
        Err(Status::AGAIN) => None,
        Err(other) => panic!("paint failed: {other:?}"),
    }
}

/// The producer's recovery restart: the driver panics, the host retries it after its backoff, and a
/// new instance is bound on the same stream.
fn restart_producer(host: &Host, clock: &SimClock) {
    let method = MethodId {
        contract: 0,
        method: 2,
    };
    assert!(host.call(0, method, &[], &mut [0u8; 8]).is_err());
    assert_ne!(
        host.device("p").unwrap().state,
        DeviceState::Running,
        "the driver broke"
    );
    for _ in 0..20 {
        clock.advance(Duration::from_secs(10));
        host.tick();
        if host.device("p").unwrap().state == DeviceState::Running {
            return;
        }
    }
    panic!("the painter did not come back");
}

/// The next frame on the ring. A pop that only reports frames replaced before they were read gives
/// nothing, and the frame behind them comes with the next pop.
fn take(consumer: &mut Consumer) -> FrameSlot {
    for _ in 0..3 {
        if let Some(frame) = consumer
            .pop_entry()
            .unwrap()
            .and_then(|entry| entry.frame())
        {
            return frame;
        }
    }
    panic!("a frame was published and none is on the ring");
}

/// A frame the consumer keeps: what it was told and a copy of its bytes, both from the take.
struct Kept {
    frame: FrameSlot,
    bytes: [u8; FRAME_BYTES],
}

fn keep(consumer: &mut Consumer) -> Kept {
    let frame = take(consumer);
    let mut bytes = [0u8; FRAME_BYTES];
    assert!(consumer.read(frame.buffer, 0, &mut bytes));
    assert_eq!(bytes, [frame.sequence as u8; FRAME_BYTES]);
    Kept { frame, bytes }
}

/// The kept frame reads back as it was taken: the read is checked against reuse, so it also fails
/// if the buffer was published again.
fn assert_intact(consumer: &Consumer, kept: &Kept, when: &str) {
    let mut now = [0u8; FRAME_BYTES];
    assert!(
        consumer.read(kept.frame.buffer, 0, &mut now),
        "{when}: buffer {} of kept frame {} was reused",
        kept.frame.buffer,
        kept.frame.sequence
    );
    assert_eq!(
        now, kept.bytes,
        "{when}: kept frame {} in buffer {} changed",
        kept.frame.sequence, kept.frame.buffer
    );
}

#[test]
fn frames_held_across_producer_restarts_are_never_granted_again_and_keep_their_bytes() {
    let (host, clock) = start();
    let mut consumer = host
        .stream_attach(0, "frames", CONSUMER)
        .unwrap()
        .consumer()
        .unwrap();

    // The first kept frame, then a restart of the producer, then the second, then another restart
    // with both held.
    let (buffer, number) = paint(&host).expect("a buffer for the first frame");
    let first = keep(&mut consumer);
    assert_eq!(
        (first.frame.buffer, first.frame.sequence),
        (buffer, u64::from(number))
    );
    restart_producer(&host, &clock);
    assert_intact(&consumer, &first, "after the first restart");
    let (buffer, number) = paint(&host).expect("a buffer for the second frame");
    let second = keep(&mut consumer);
    assert_eq!(
        (second.frame.buffer, second.frame.sequence),
        (buffer, u64::from(number))
    );
    assert_ne!(first.frame.buffer, second.frame.buffer);
    restart_producer(&host, &clock);
    let kept = [&first, &second];
    for frame in kept {
        assert_intact(&consumer, frame, "after the second restart");
    }

    // The producer writes on: forty asks, with a restart in the middle. Half the frames are read
    // and given back, half are left unread to be replaced. Every buffer it is granted is one of
    // the two the consumer does not hold.
    let mut painted = 0;
    for round in 0..40 {
        if round == 20 {
            restart_producer(&host, &clock);
        }
        if let Some((buffer, _)) = paint(&host) {
            painted += 1;
            for frame in kept {
                assert_ne!(
                    buffer, frame.frame.buffer,
                    "round {round}: the producer was granted the buffer of kept frame {}",
                    frame.frame.sequence
                );
            }
            if round % 2 == 0 {
                let read = take(&mut consumer);
                assert!(consumer.release(read.buffer));
            }
        }
        for frame in kept {
            assert_intact(&consumer, frame, "while the producer writes on");
        }
    }
    assert!(
        painted >= 20,
        "the producer kept publishing in the two buffers left: {painted}"
    );
    let stats = host.stream_stats(0, "frames").unwrap();
    assert_eq!(
        (stats.faults, stats.attachments, consumer.state()),
        (0, 1, STATE_OPEN),
        "the same attachment throughout: {stats:?}"
    );

    // Given back, the kept buffers are the producer's again.
    assert!(consumer.release(first.frame.buffer));
    assert!(consumer.release(second.frame.buffer));
    while consumer.pop_entry().unwrap().is_some_and(|entry| {
        entry
            .frame()
            .is_some_and(|frame| consumer.release(frame.buffer))
    }) {}
    let mut granted = [false; BUFFERS as usize];
    for _ in 0..16 {
        if let Some((buffer, _)) = paint(&host) {
            granted[buffer as usize] = true;
            let read = take(&mut consumer);
            assert!(consumer.release(read.buffer));
        }
    }
    assert!(
        granted[first.frame.buffer as usize] && granted[second.frame.buffer as usize],
        "the released buffers came back to the producer: {granted:?}"
    );
}
