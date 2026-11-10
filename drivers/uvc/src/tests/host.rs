//! The driver on a real host, with a board that hands out mock cameras by name: frames reach a
//! consumer on the real ring, and a camera that leaves is opened again by its name.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lemnos_contract_camera_frames::Counters;
use lemnos_core::{
    CallerId, Decode, Encode, Health, HostClock, LocalCaller, MethodId, Provider, Reader, Result,
    Status, Tag, UsbDeviceHandle, Writer,
};
use lemnos_host::board::parse;
use lemnos_host::{DeviceState, Host, Options, entry};
use lemnos_wire::ring::Consumer;

use super::fixture::{self, CONTROL_INTERFACE, MODE_MJPEG, STREAM_INTERFACE};
use super::mock::MockUsb;
use super::rig::{EOF, FID, payload};
use crate::Uvc;
use crate::session::GONE;

const NAME: &str = "usb:1-4";
const CONSUMER: CallerId = CallerId(7);

#[derive(Clone, Default)]
struct Clock(Arc<AtomicU64>);

impl HostClock for Clock {
    fn now_ns(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
}

/// USB devices by board name, as the Linux backend gives them: each open finds what is plugged
/// in under the name now.
#[derive(Clone, Default)]
struct Board {
    plugged: Arc<Mutex<BTreeMap<String, MockUsb>>>,
    opens: Arc<Mutex<Vec<String>>>,
}

impl Board {
    fn plug(&self, name: &str, device: MockUsb) {
        self.plugged
            .lock()
            .unwrap()
            .insert(name.to_string(), device);
    }

    fn opens(&self) -> Vec<String> {
        self.opens.lock().unwrap().clone()
    }
}

impl Provider for Board {
    fn provided(&self, need: &str) -> bool {
        self.plugged.lock().unwrap().contains_key(need)
    }

    fn device(&mut self, _need: &str, _tag: Tag) -> Result<LocalCaller> {
        Err(Status::NOT_SUPPORTED)
    }

    fn usb_device(&mut self, need: &str) -> Result<UsbDeviceHandle> {
        self.opens.lock().unwrap().push(need.to_string());
        let device = self.plugged.lock().unwrap().get(need).cloned();
        Ok(Box::new(device.ok_or(Status::NO_DEVICE)?))
    }
}

/// A camera as the kernel leaves it once plugged in: its video driver bound to both interfaces.
fn camera() -> MockUsb {
    let usb = MockUsb::new(fixture::webcam().bytes);
    usb.bind_kernel_driver(CONTROL_INTERFACE);
    usb.bind_kernel_driver(STREAM_INTERFACE);
    usb
}

fn enc<T: Encode + ?Sized>(value: &T) -> Vec<u8> {
    let mut buffer = [0u8; 64];
    let mut writer = Writer::new(&mut buffer);
    value.encode(&mut writer).unwrap();
    let used = writer.len();
    buffer[..used].to_vec()
}

struct Rig {
    host: Host,
    clock: Clock,
    board: Board,
    index: u32,
}

impl Rig {
    fn start(first: MockUsb) -> Self {
        let text = format!(
            "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"uvc\"\n\
             [[devices]]\nid = \"webcam\"\ndriver = \"uvc\"\nbus = \"{NAME}\"\n\
             config = {{ frame_bytes = 4096 }}\n\
             [devices.streams.frames]\nbuffers = 4\nbuffer_bytes = 4096\n"
        );
        let board = Board::default();
        board.plug(NAME, first);
        let clock = Clock::default();
        clock.0.store(1_000, Ordering::Relaxed);
        let host = Host::start(
            &parse(&text).unwrap(),
            &[entry::<Uvc>()],
            Box::new(board.clone()),
            Arc::new(clock.clone()),
            Options::default(),
        )
        .unwrap();
        let index = host.device("webcam").unwrap().index;
        Self {
            host,
            clock,
            board,
            index,
        }
    }

    fn call(&self, method: u16, args: &[u8]) -> Result<Vec<u8>> {
        let mut reply = [0u8; 64];
        let id = MethodId {
            contract: 0,
            method,
        };
        let n = self.host.call(self.index, id, args, &mut reply)?;
        Ok(reply[..n].to_vec())
    }

    fn counters(&self) -> Result<Counters> {
        let bytes = self.call(8, &[])?;
        Ok(Counters::decode(&mut Reader::new(&bytes)).unwrap())
    }

    /// The host's ticks, enough times for the driver to do what is due now.
    fn ticks(&self) {
        for _ in 0..16 {
            self.host.tick();
        }
    }

    fn advance(&self, by: Duration) {
        self.clock
            .0
            .fetch_add(by.as_nanos() as u64, Ordering::Relaxed);
        self.ticks();
    }

    fn health(&self) -> Health {
        self.host.device("webcam").unwrap().health
    }
}

#[test]
fn frames_reach_a_consumer_and_a_camera_that_leaves_is_opened_again_by_name() {
    let first = camera();
    let rig = Rig::start(first.clone());
    rig.ticks();
    assert_eq!(rig.board.opens(), vec![NAME]);
    assert_eq!(
        first.state().detaches,
        vec![CONTROL_INTERFACE, STREAM_INTERFACE]
    );
    assert_eq!(
        first.state().claimed,
        vec![CONTROL_INTERFACE, STREAM_INTERFACE]
    );

    rig.call(3, &enc(&MODE_MJPEG)).unwrap();
    rig.call(6, &[]).unwrap();
    rig.ticks();
    let lease = rig
        .host
        .stream_attach(rig.index, "frames", CONSUMER)
        .unwrap();
    let mut consumer = Consumer::attach(Arc::clone(&lease.ring), lease.layout).unwrap();
    first.queue(vec![
        payload(EOF, b""),
        payload(FID, b"one "),
        payload(FID | EOF, b"frame"),
    ]);
    rig.ticks();
    let frame = consumer.pop_frame().unwrap().expect("a frame on the ring");
    let mut bytes = vec![0u8; frame.bytes as usize];
    let offset = lease.layout.buffer_offset(frame.buffer).unwrap();
    assert!(lease.buffers.read(offset, &mut bytes));
    assert_eq!(bytes, b"one frame");
    assert!(consumer.release(frame.buffer, frame.fence).unwrap());
    assert_eq!(rig.counters().unwrap().frames, 1);

    // The camera leaves; another is plugged in where it was. The old handle can't reach it.
    first.unplug();
    let second = camera();
    rig.board.plug(NAME, second.clone());
    rig.ticks();
    assert_eq!(rig.health(), Health::Failed(GONE));

    // A caller asking after it hears `NO_DEVICE`, and the host drops the driver.
    for _ in 0..Options::default().error_limit {
        assert_eq!(rig.counters(), Err(Status::NO_DEVICE));
    }
    assert!(matches!(
        rig.host.device("webcam").unwrap().state,
        DeviceState::Backoff { .. }
    ));

    // The host binds it again, which opens the device by its name: the new camera.
    rig.advance(Options::default().retry_first);
    assert_eq!(rig.board.opens(), vec![NAME, NAME]);
    assert_eq!(
        rig.host.device("webcam").unwrap().state,
        DeviceState::Running
    );
    assert_eq!(
        second.state().detaches,
        vec![CONTROL_INTERFACE, STREAM_INTERFACE],
        "its own driver, bound again on the new device, is detached again"
    );
    assert_eq!(
        second.state().claimed,
        vec![CONTROL_INTERFACE, STREAM_INTERFACE]
    );
    assert_eq!(rig.health(), Health::Ok);
    assert_eq!(rig.counters(), Ok(Counters::default()));
}
