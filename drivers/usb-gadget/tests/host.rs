//! The driver as a host runs it: bound from a board file on the simulated board, its contract
//! called through the host, its jobs posted by a consumer on the host's own frame stream and
//! completed there, its ticks from the host's simulated clock. The simulated board's function
//! plays the USB host. Nothing here goes near a real controller.

use std::sync::Arc;
use std::time::Duration;

use lemnos_contract_usb_function::{
    COMPLETION_BYTES, Completion, DATA_OFFSET, EVENT_BYTES, Event, EventKind, JOB_BYTES, Job,
    JobKind, JobStatus, UsbFunction,
};
use lemnos_core::{
    Bound, Caller, CallerId, Decode, Encode, Health, MethodId, Reader, Result, Status, Writer,
};
use lemnos_driver_usb_gadget::UsbGadget;
use lemnos_hal::blocks::{ControlSetup, UsbFunctionEvent};
use lemnos_hal::mock::{ControlAnswer, MockUsbFunction};
use lemnos_host::board::parse;
use lemnos_host::{Host, Options, entry};
use lemnos_sim::{SimBoard, SimClock};
use lemnos_wire::ring::{Consumer, FrameSlot, STATE_FAULT};

const IN: u8 = 0x81;
const OUT: u8 = 0x02;
const BUFFER_BYTES: usize = 1024;

/// One gadget on the controller the simulated board calls `udc0`, with its data stream.
const BOARD: &str = r#"
format = "lemnos.board"
schema_version = 1
[board]
id = "t"

[[devices]]
id = "gadget"
driver = "usb-gadget"
uses = { function = "udc0" }
config = { vendor = 0x1209, product = 1, serial = "S0", interfaces = "interface 0xff 0 0 pipe\nendpoint in bulk\nendpoint out bulk\n" }

[devices.streams.data]
buffers = 8
buffer_bytes = 1024
job_slots = 16
job_bytes = 22
"#;

/// The contract's calls, carried by the host to the device.
struct Through<'a> {
    host: &'a Host,
    args: [u8; 256],
}

impl Caller for Through<'_> {
    fn args(&mut self) -> &mut [u8] {
        &mut self.args
    }

    fn call(&mut self, method: u16, args_len: usize, reply: &mut [u8]) -> Result<usize> {
        let id = MethodId {
            contract: 0,
            method,
        };
        self.host.call(0, id, &self.args[..args_len], reply)
    }
}

struct Rig {
    host: Host,
    clock: SimClock,
    /// The USB host's side of the function.
    usb: MockUsbFunction,
}

impl Rig {
    fn start() -> Self {
        let sim = SimBoard::new();
        let usb = sim.add_usb_function("udc0");
        let clock = sim.clock();
        let host = Host::start(
            &parse(BOARD).unwrap(),
            &[entry::<UsbGadget>()],
            Box::new(sim),
            Arc::new(clock.clone()),
            Options::default(),
        )
        .unwrap();
        let rig = Self { host, clock, usb };
        rig.ticks(1);
        assert_eq!(rig.host.device("gadget").unwrap().health, Health::Ok);
        assert!(rig.usb.descriptors().is_some(), "the gadget is enabled");
        rig
    }

    /// `count` rounds of the driver's ticks, 20 ms of the host's clock apart: long enough
    /// for the driver to be due whether or not it is serving.
    fn ticks(&self, count: u32) {
        for _ in 0..count {
            self.clock.advance(Duration::from_millis(20));
            self.host.tick();
            self.host.run_ticks();
        }
    }

    fn device(&self) -> Bound<dyn UsbFunction, Through<'_>> {
        Bound::new(Through {
            host: &self.host,
            args: [0; 256],
        })
    }
}

/// A consumer of the data stream, as a client of the service is one.
struct Serving {
    ring: Consumer,
}

impl Serving {
    fn attach(rig: &Rig, caller: u64) -> Self {
        let ring = rig
            .host
            .stream_attach(0, "data", CallerId(caller))
            .unwrap()
            .consumer_writable()
            .unwrap();
        Self { ring }
    }

    fn post(&mut self, job: Job) {
        let mut bytes = [0u8; JOB_BYTES];
        let mut writer = Writer::new(&mut bytes);
        job.encode(&mut writer).unwrap();
        let used = writer.len();
        assert!(self.ring.jobs().post(&bytes[..used]).unwrap());
    }

    fn want_event(&mut self, identity: u64, buffer: u32) {
        self.post(Job {
            identity,
            kind: JobKind::Event,
            endpoint: 0,
            buffer,
            length: 0,
        });
    }

    fn receive(&mut self, identity: u64, buffer: u32) {
        self.post(Job {
            identity,
            kind: JobKind::Receive,
            endpoint: OUT,
            buffer,
            length: 512,
        });
    }

    fn send(&mut self, identity: u64, buffer: u32, data: &[u8]) {
        assert!(self.ring.write(buffer, DATA_OFFSET, data));
        self.post(Job {
            identity,
            kind: JobKind::Send,
            endpoint: IN,
            buffer,
            length: data.len() as u32,
        });
    }

    /// The next completion: its frame, its record and the bytes after the record's part. The
    /// buffer goes back to the stream.
    fn completion(&mut self) -> Option<(FrameSlot, Completion, Vec<u8>)> {
        let frame = self.ring.pop_frame().unwrap()?;
        let mut record = [0u8; COMPLETION_BYTES];
        assert!(self.ring.read(frame.buffer, 0, &mut record));
        let record = Completion::decode(&mut Reader::new(&record)).unwrap();
        let mut data = vec![0u8; (frame.bytes as usize).saturating_sub(DATA_OFFSET)];
        assert!(self.ring.read(frame.buffer, DATA_OFFSET, &mut data));
        assert!(self.ring.release(frame.buffer));
        Some((frame, record, data))
    }

    fn done(&mut self, identity: u64) -> Vec<u8> {
        let (frame, record, data) = self.completion().expect("a completion");
        assert_eq!((frame.identity, record.status), (identity, JobStatus::Done));
        data
    }

    fn event(&mut self, identity: u64) -> Event {
        let data = self.done(identity);
        assert_eq!(data.len(), EVENT_BYTES);
        Event::decode(&mut Reader::new(&data)).unwrap()
    }
}

fn vendor_in() -> ControlSetup {
    ControlSetup {
        request_type: 0xc0,
        request: 0x51,
        value: 2,
        index: 0,
    }
}

#[test]
fn a_caller_answers_a_control_request_and_echoes_bytes_on_an_endpoint_pair() {
    let rig = Rig::start();
    let mut device = rig.device();

    // The caller finds out what the device is and where its bytes go.
    let info = device.info().unwrap();
    assert_eq!(info.endpoints, 2);
    assert!(info.max_transfer as usize >= BUFFER_BYTES - DATA_OFFSET);
    assert_eq!(device.endpoint(0).unwrap().address, IN);
    assert_eq!(device.endpoint(1).unwrap().address, OUT);
    assert_eq!(device.endpoint(2), Err(Status::NOT_FOUND));
    let mut name = [0u8; 32];
    let len = device.data_stream(&mut name).unwrap();
    assert_eq!(&name[..len], b"data");
    let mut caller = Serving::attach(&rig, 7);
    rig.ticks(2);

    // The USB host configures the device: the caller is told.
    caller.want_event(1, 0);
    rig.usb.event(UsbFunctionEvent::Configured);
    rig.ticks(1);
    assert_eq!(caller.event(1).kind, EventKind::Configured);
    let state = device.state().unwrap();
    assert!(state.up && state.configured && !state.request_pending);

    // It makes a vendor request: the caller is told, and answers with the data stage.
    caller.want_event(2, 0);
    rig.usb.request(vendor_in(), 16, &[]);
    rig.ticks(1);
    let asked = caller.event(2);
    assert_eq!(asked.kind, EventKind::Setup);
    assert_eq!(
        (
            asked.request.request_type,
            asked.request.request,
            asked.request.value,
            asked.request.length
        ),
        (0xc0, 0x51, 2, 16)
    );
    assert!(device.state().unwrap().request_pending);
    assert_eq!(device.control_in(b"lemnos"), Ok(6));
    assert_eq!(
        rig.usb.answers(),
        [(vendor_in(), ControlAnswer::In(b"lemnos".to_vec()))]
    );

    // It sends bytes to the OUT endpoint: the caller receives them and sends them back on
    // the IN endpoint, three times over.
    for round in 0..3u64 {
        let sent: Vec<u8> = (0..200)
            .map(|i| (i as u8).wrapping_add(round as u8))
            .collect();
        caller.receive(10 + round, 1);
        rig.usb.host_sends(OUT, &sent);
        rig.ticks(1);
        let came = caller.done(10 + round);
        assert_eq!(came, sent);
        caller.send(20 + round, 2, &came);
        rig.ticks(1);
        assert_eq!(caller.done(20 + round), b"");
        assert_eq!(rig.usb.host_received()[round as usize], (IN, sent));
    }

    // It halts an endpoint, and takes a request it has no answer for.
    assert_eq!(device.set_halt(OUT, true), Ok(()));
    assert!(rig.usb.is_halted(OUT));
    caller.want_event(3, 0);
    rig.usb.request(vendor_in(), 0, &[]);
    rig.ticks(1);
    assert_eq!(caller.event(3).kind, EventKind::Setup);
    assert_eq!(device.control_stall(), Ok(()));
    assert_eq!(rig.usb.answers()[1].1, ControlAnswer::Stalled);

    let counters = device.counters().unwrap();
    assert_eq!(
        (counters.jobs, counters.errors, counters.requests_unanswered),
        (9, 0, 0)
    );
    let stats = rig.host.stream_stats(0, "data").unwrap();
    assert_eq!(stats.faults, 0);
}

#[test]
fn a_caller_that_leaves_is_replaced_and_the_device_refuses_requests_meanwhile() {
    let rig = Rig::start();
    let mut device = rig.device();
    rig.usb.event(UsbFunctionEvent::Configured);
    rig.ticks(1);

    // Nobody has the stream: the device is on the bus and refuses what it is asked.
    rig.usb.request(vendor_in(), 4, &[]);
    rig.ticks(1);
    assert_eq!(rig.usb.answers(), [(vendor_in(), ControlAnswer::Stalled)]);

    let mut first = Serving::attach(&rig, 1);
    rig.ticks(2);
    first.receive(1, 0);
    first.want_event(2, 1);
    rig.usb.request(vendor_in(), 4, &[]);
    rig.ticks(1);
    assert_eq!(first.event(2).kind, EventKind::Setup);
    assert!(device.state().unwrap().request_pending);

    // It leaves with a receive waiting and the request unanswered; another takes its place
    // before the driver looks again.
    rig.host.stream_detach(0, "data", CallerId(1)).unwrap();
    let mut second = Serving::attach(&rig, 2);
    rig.usb.host_sends(OUT, b"for the second");
    rig.ticks(2);
    assert_eq!(rig.usb.answers().len(), 2);
    assert_eq!(rig.usb.answers()[1].1, ControlAnswer::Stalled);
    assert!(!device.state().unwrap().request_pending);
    assert!(
        second.completion().is_none(),
        "nothing of the first's reaches it"
    );

    // The second's own job, naming the buffer the first's had, is served, and what the USB
    // host sent meanwhile was not lost to the job that went.
    second.receive(9, 0);
    rig.ticks(1);
    assert_eq!(second.done(9), b"for the second");
    assert_eq!(device.counters().unwrap().requests_unanswered, 2);
    assert_eq!(rig.host.device("gadget").unwrap().health, Health::Ok);
}

#[test]
fn a_caller_that_posts_what_is_no_job_loses_the_stream_and_the_next_is_served() {
    let rig = Rig::start();
    let mut device = rig.device();
    rig.usb.event(UsbFunctionEvent::Configured);
    let mut broken = Serving::attach(&rig, 1);
    rig.ticks(2);
    broken.receive(1, 0);
    rig.ticks(1);
    // A job naming the buffer the waiting receive has: nothing to complete it on.
    broken.receive(2, 0);
    rig.ticks(1);
    assert_eq!(broken.ring.state(), STATE_FAULT);
    assert_eq!(rig.host.stream_stats(0, "data").unwrap().faults, 1);
    assert_eq!(device.counters().unwrap().errors, 1);

    // The stream is free for the next caller, which is served from a clean start.
    let mut next = Serving::attach(&rig, 2);
    rig.ticks(2);
    next.receive(3, 0);
    rig.usb.host_sends(OUT, b"still here");
    rig.ticks(1);
    assert_eq!(next.done(3), b"still here");
    assert_eq!(rig.host.device("gadget").unwrap().health, Health::Ok);
}
