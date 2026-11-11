//! The data stream against the mock function and the mock stream, with the test playing the
//! host on one and the consumer on the other. Time is simulated.

extern crate std;

use std::boxed::Box;
use std::vec::Vec;

use lemnos_contract_usb_function::{
    COMPLETION_BYTES, Completion, DATA_OFFSET, EVENT_BYTES, Event, EventKind, JOB_BYTES, Job,
    JobKind, JobStatus, Request, UsbFunction,
};
use lemnos_core::{
    ConfigValue, Decode, Driver, Encode, LocalCaller, Provider, Reader, Resources, StreamHandle,
    Tag, Writer,
};
use lemnos_hal::blocks::{BufferHandle, BufferKind, ControlSetup, StreamFrame, UsbFunctionEvent};
use lemnos_hal::mock::{ControlAnswer, MockStream, MockUsbFunction};
use lemnos_hal::{StreamEndpoint, UsbFunction as HalFunction};
use lemnos_measure::test_alloc::allocations;

use super::*;
use crate::serve::{MAX_EVENTS, MAX_PENDING, TRANSFER_BYTES};

const INTERFACES: &str = "
interface 0xff 0 0 pipe
endpoint in bulk
endpoint out bulk
endpoint in interrupt 16 10
";
/// The addresses the description above gets.
const IN: u8 = 0x81;
const OUT: u8 = 0x02;
const STATUS: u8 = 0x83;

const BUFFERS: u32 = 8;
const BYTES: usize = 4096;

struct Board {
    function: Option<Box<dyn HalFunction<Error = ErrorKind> + Send>>,
    stream: Option<Box<dyn StreamEndpoint<Error = ErrorKind> + Send>>,
}

impl Provider for Board {
    fn provided(&self, need: &str) -> bool {
        need == "function" || (need == "data" && self.stream.is_some())
    }

    fn device(&mut self, _need: &str, _tag: Tag) -> Result<LocalCaller> {
        Err(Status::NOT_SUPPORTED)
    }

    fn usb_function(&mut self, _need: &str) -> Result<UsbFunctionHandle> {
        self.function
            .take()
            .map(lemnos_core::Held::boxed)
            .ok_or(Status::NOT_FOUND)
    }

    fn stream(&mut self, _need: &str) -> Result<StreamHandle> {
        self.stream
            .take()
            .map(lemnos_core::Held::boxed)
            .ok_or(Status::NOT_FOUND)
    }
}

fn bind_on(
    function: Box<dyn HalFunction<Error = ErrorKind> + Send>,
    stream: Option<Box<dyn StreamEndpoint<Error = ErrorKind> + Send>>,
    extra: &[(&'static str, ConfigValue<'static>)],
) -> UsbGadget {
    let mut entries = std::vec![
        ("vendor", ConfigValue::Int(0x1209)),
        ("product", ConfigValue::Int(1)),
        ("interfaces", ConfigValue::Str(INTERFACES)),
    ];
    entries.extend_from_slice(extra);
    let mut board = Board {
        function: Some(function),
        stream,
    };
    let mut resources = Resources::new(&mut board, None);
    UsbGadget::bind(&mut resources, &Config::new(&entries))
        .map_err(|_| ())
        .expect("bind")
}

/// The driver between a host and a consumer the test plays.
struct Rig {
    driver: UsbGadget,
    host: MockUsbFunction,
    consumer: MockStream,
    now: u64,
}

impl Rig {
    fn with(bytes: usize, extra: &[(&'static str, ConfigValue<'static>)]) -> Self {
        let host = MockUsbFunction::new();
        let consumer = MockStream::with_bytes(BUFFERS, bytes);
        let driver = bind_on(
            Box::new(host.clone()),
            Some(Box::new(consumer.clone())),
            extra,
        );
        let mut rig = Self {
            driver,
            host,
            consumer,
            now: 0,
        };
        rig.tick();
        assert_eq!(Driver::health(&rig.driver), Health::Ok);
        rig
    }

    /// Up, with a host that has configured the device.
    fn configured() -> Self {
        let mut rig = Self::with(BYTES, &[]);
        rig.host.event(UsbFunctionEvent::Configured);
        rig.tick();
        assert!(rig.driver.configured());
        rig
    }

    /// One tick, at the time the driver asked for.
    fn tick(&mut self) {
        self.now = Driver::tick(&mut self.driver, self.now)
            .expect("the driver always ticks")
            .max(self.now + 1);
    }

    fn post(&self, job: &Job) {
        let mut bytes = [0u8; JOB_BYTES];
        let mut writer = Writer::new(&mut bytes);
        job.encode(&mut writer).unwrap();
        let used = writer.len();
        self.consumer.post_job(&bytes[..used]);
    }

    fn send(&self, identity: u64, endpoint: u8, buffer: u32, data: &[u8]) {
        let mut filled = std::vec![0u8; DATA_OFFSET];
        filled.extend_from_slice(data);
        self.consumer.fill(buffer, &filled);
        self.post(&Job {
            identity,
            kind: JobKind::Send,
            endpoint,
            buffer,
            length: data.len() as u32,
        });
    }

    fn receive(&self, identity: u64, endpoint: u8, buffer: u32, length: u32) {
        self.post(&Job {
            identity,
            kind: JobKind::Receive,
            endpoint,
            buffer,
            length,
        });
    }

    fn want_event(&self, identity: u64, buffer: u32) {
        self.post(&Job {
            identity,
            kind: JobKind::Event,
            endpoint: 0,
            buffer,
            length: 0,
        });
    }

    /// The next completion: its frame, its record, and the bytes after the record's part.
    fn completion(&self) -> Option<(StreamFrame, Completion, Vec<u8>)> {
        let frame = self.consumer.take_frame()?;
        let contents = self.consumer.contents(frame.buffer);
        let record = Completion::decode(&mut Reader::new(&contents[..COMPLETION_BYTES])).unwrap();
        let data = contents[DATA_OFFSET.min(contents.len())..]
            .iter()
            .take((frame.bytes as usize).saturating_sub(DATA_OFFSET))
            .copied()
            .collect();
        Some((frame, record, data))
    }

    /// The next completion, which must be `identity`'s and done; its bytes.
    fn done(&self, identity: u64) -> Vec<u8> {
        let (frame, record, data) = self.completion().expect("a completion");
        assert_eq!(frame.identity, identity);
        assert_eq!(record.status, JobStatus::Done);
        assert_eq!(frame.bytes as usize, DATA_OFFSET + data.len());
        data
    }

    /// The next completion, which must be `identity`'s and failed; why.
    fn failed(&self, identity: u64) -> JobStatus {
        let (frame, record, _) = self.completion().expect("a completion");
        assert_eq!(frame.identity, identity);
        assert_ne!(record.status, JobStatus::Done);
        assert_eq!((record.count, frame.bytes as usize), (0, DATA_OFFSET));
        record.status
    }

    fn event(&self, identity: u64) -> Event {
        let data = self.done(identity);
        assert_eq!(data.len(), EVENT_BYTES);
        Event::decode(&mut Reader::new(&data)).unwrap()
    }

    fn counters(&mut self) -> lemnos_contract_usb_function::Counters {
        UsbFunction::counters(&mut self.driver).unwrap()
    }
}

fn request(request_type: u8) -> ControlSetup {
    ControlSetup {
        request_type,
        request: 7,
        value: 0x0102,
        index: 3,
    }
}

#[test]
fn the_contract_says_what_the_device_is_and_how_it_stands() {
    let mut rig = Rig::with(BYTES, &[("control_timeout_ms", ConfigValue::Int(250))]);
    let info = UsbFunction::info(&mut rig.driver).unwrap();
    assert_eq!(info.endpoints, 3);
    assert_eq!(info.max_transfer as usize, TRANSFER_BYTES);
    assert_eq!(info.max_pending as usize, MAX_PENDING);
    assert_eq!(info.max_events as usize, MAX_EVENTS);
    assert_eq!(info.control_timeout_ms, 250);
    let endpoint = |rig: &mut Rig, index| UsbFunction::endpoint(&mut rig.driver, index);
    let first = endpoint(&mut rig, 0).unwrap();
    assert_eq!(
        (first.address, first.attributes, first.max_packet),
        (IN, 2, 64)
    );
    assert_eq!(endpoint(&mut rig, 1).unwrap().address, OUT);
    let status = endpoint(&mut rig, 2).unwrap();
    assert_eq!(
        (status.address, status.attributes, status.max_packet),
        (STATUS, 3, 16)
    );
    assert_eq!(endpoint(&mut rig, 3), Err(Status::NOT_FOUND));

    let mut name = [0u8; 32];
    let len = UsbFunction::data_stream(&mut rig.driver, &mut name).unwrap();
    assert_eq!(&name[..len], b"data");
    assert_eq!(
        UsbFunction::data_stream(&mut rig.driver, &mut [0u8; 3]),
        Err(Status::BUFFER_TOO_SMALL)
    );

    let state = UsbFunction::state(&mut rig.driver).unwrap();
    assert!(state.up && !state.configured && !state.suspended && !state.request_pending);
    rig.host.event(UsbFunctionEvent::Configured);
    rig.host.event(UsbFunctionEvent::Suspended);
    rig.tick();
    let state = UsbFunction::state(&mut rig.driver).unwrap();
    assert!(state.up && state.configured && state.suspended);
}

#[test]
fn a_device_without_a_data_stream_is_on_the_bus_and_serves_nothing() {
    let host = MockUsbFunction::new();
    let mut driver = bind_on(Box::new(host.clone()), None, &[]);
    Driver::tick(&mut driver, 0).unwrap();
    assert_eq!(
        UsbFunction::data_stream(&mut driver, &mut [0u8; 32]),
        Err(Status::NOT_SUPPORTED)
    );
    assert_eq!(UsbFunction::info(&mut driver).unwrap().max_transfer, 0);
    host.request(request(0xc0), 4, &[]);
    Driver::tick(&mut driver, POLL_NS).unwrap();
    assert_eq!(host.answers(), [(request(0xc0), ControlAnswer::Stalled)]);
    assert_eq!(
        UsbFunction::counters(&mut driver)
            .unwrap()
            .requests_unanswered,
        1
    );
    assert_eq!(UsbFunction::cancel(&mut driver, 0), Ok(0));
}

#[test]
fn a_caller_answers_a_request_and_echoes_bytes_on_an_endpoint_pair() {
    let mut rig = Rig::with(BYTES, &[]);
    rig.want_event(1, 0);
    rig.want_event(2, 1);
    rig.tick();
    assert!(rig.completion().is_none(), "nothing has happened yet");

    rig.host.event(UsbFunctionEvent::Configured);
    rig.tick();
    assert_eq!(rig.event(1).kind, EventKind::Configured);

    // A request whose data goes to the host: the caller is told, and answers.
    rig.host.request(request(0xc0), 8, &[]);
    rig.tick();
    assert_eq!(
        rig.event(2),
        Event {
            kind: EventKind::Setup,
            request: Request {
                request_type: 0xc0,
                request: 7,
                value: 0x0102,
                index: 3,
                length: 8,
            },
        }
    );
    assert!(UsbFunction::state(&mut rig.driver).unwrap().request_pending);
    assert_eq!(UsbFunction::control_in(&mut rig.driver, b"reply"), Ok(5));
    assert!(!UsbFunction::state(&mut rig.driver).unwrap().request_pending);

    // One whose data comes from the host.
    rig.consumer.return_buffer(0);
    rig.want_event(3, 0);
    rig.host.request(request(0x40), 3, b"abc");
    rig.tick();
    assert_eq!(rig.event(3).request.length, 3);
    let mut taken = [0u8; 16];
    assert_eq!(UsbFunction::control_out(&mut rig.driver, &mut taken), Ok(3));
    assert_eq!(&taken[..3], b"abc");

    // And one the caller refuses.
    rig.consumer.return_buffer(1);
    rig.want_event(4, 1);
    rig.host.request(request(0xc1), 2, &[]);
    rig.tick();
    assert_eq!(rig.event(4).request.request_type, 0xc1);
    assert_eq!(UsbFunction::control_stall(&mut rig.driver), Ok(()));
    assert_eq!(
        rig.host.answers(),
        [
            (request(0xc0), ControlAnswer::In(b"reply".to_vec())),
            (request(0x40), ControlAnswer::Out),
            (request(0xc1), ControlAnswer::Stalled),
        ]
    );
    assert_eq!(rig.counters().requests_unanswered, 0);

    // What the host sends on the OUT endpoint goes back on the IN endpoint.
    rig.receive(10, OUT, 2, 512);
    rig.tick();
    assert!(rig.completion().is_none(), "nothing has come yet");
    rig.host.host_sends(OUT, b"ping");
    rig.tick();
    let came = rig.done(10);
    assert_eq!(came, b"ping");
    rig.send(11, IN, 3, &came);
    rig.tick();
    assert_eq!(rig.done(11), b"", "a send's completion carries no bytes");
    assert_eq!(rig.host.host_received(), [(IN, b"ping".to_vec())]);

    // A zero-length packet each way.
    rig.receive(12, OUT, 4, 64);
    rig.host.host_sends(OUT, b"");
    rig.send(13, IN, 5, b"");
    rig.tick();
    assert_eq!(rig.done(12), b"");
    assert_eq!(rig.done(13), b"");
    assert_eq!(rig.host.host_received()[1], (IN, Vec::new()));
    let counters = rig.counters();
    assert_eq!((counters.jobs, counters.errors), (8, 0));
}

#[test]
fn a_job_that_asks_for_what_it_may_not_is_refused_and_nothing_moves() {
    let mut rig = Rig::configured();
    let record_of = |rig: &Rig, identity| rig.failed(identity);

    // No endpoint of that address, one of the other direction, the control endpoint.
    rig.send(1, 0x85, 0, b"x");
    rig.send(2, OUT, 1, b"x");
    rig.receive(3, IN, 2, 64);
    rig.receive(4, 0, 3, 64);
    rig.tick();
    for identity in 1..=4 {
        assert_eq!(record_of(&rig, identity), JobStatus::Refused);
    }
    for buffer in 0..4 {
        rig.consumer.return_buffer(buffer);
    }

    // A length the driver's own memory can't hold, one past the buffer, a receive of nothing.
    rig.receive(5, OUT, 0, TRANSFER_BYTES as u32 + 1);
    rig.receive(6, OUT, 1, (BYTES - DATA_OFFSET) as u32 + 1);
    rig.post(&Job {
        identity: 7,
        kind: JobKind::Send,
        endpoint: IN,
        buffer: 2,
        length: (BYTES - DATA_OFFSET) as u32 + 1,
    });
    rig.receive(8, OUT, 3, 0);
    rig.receive(9, OUT, 4, u32::MAX);
    rig.tick();
    for identity in 5..=9 {
        assert_eq!(record_of(&rig, identity), JobStatus::Refused);
    }
    // The longest that fits is taken.
    rig.receive(10, OUT, 5, (BYTES - DATA_OFFSET) as u32);
    rig.tick();
    assert!(rig.completion().is_none());
    assert_eq!(rig.driver.serve.waiting(), 1);

    // What the host sends still reaches the one job that was sound, whole.
    rig.host.host_sends(OUT, b"sound");
    rig.tick();
    assert_eq!(rig.done(10), b"sound");
    assert!(rig.host.host_received().is_empty());
    assert_eq!(
        rig.consumer.faults(),
        0,
        "a refused job is no broken consumer"
    );
}

/// What the consumer does with a frame the driver has just published.
type After = Box<dyn FnMut(&MockStream, &StreamFrame) + Send>;

/// A consumer that acts while the driver serves it: `after` is called with each frame the
/// driver publishes, right after it is published.
struct Acting {
    inner: MockStream,
    after: After,
}

impl StreamEndpoint for Acting {
    type Error = ErrorKind;

    fn buffer_count(&self) -> u32 {
        self.inner.buffer_count()
    }

    fn buffer_current(&self, buffer: u32) -> core::result::Result<bool, ErrorKind> {
        self.inner.buffer_current(buffer)
    }

    fn buffer_kind(&self) -> BufferKind {
        self.inner.buffer_kind()
    }

    fn publish(&mut self, frame: StreamFrame) -> core::result::Result<(), ErrorKind> {
        self.inner.publish(frame)?;
        (self.after)(&self.inner, &frame);
        Ok(())
    }

    fn next_buffer(&mut self) -> core::result::Result<Option<u32>, ErrorKind> {
        self.inner.next_buffer()
    }

    fn next_job(&mut self, out: &mut [u8]) -> core::result::Result<Option<usize>, ErrorKind> {
        self.inner.next_job(out)
    }

    fn write(
        &mut self,
        buffer: u32,
        offset: usize,
        data: &[u8],
    ) -> core::result::Result<bool, ErrorKind> {
        self.inner.write(buffer, offset, data)
    }

    fn read(
        &mut self,
        buffer: u32,
        offset: usize,
        out: &mut [u8],
    ) -> core::result::Result<bool, ErrorKind> {
        self.inner.read(buffer, offset, out)
    }

    fn buffer_handle(
        &self,
        buffer: u32,
    ) -> core::result::Result<Option<BufferHandle<'_>>, ErrorKind> {
        self.inner.buffer_handle(buffer)
    }

    fn fault_consumer(&mut self) -> core::result::Result<(), ErrorKind> {
        self.inner.fault_consumer()
    }
}

/// A consumer may take a completion, give its buffer back and name it in its next job while
/// the driver is still reading jobs. The driver holds the buffer by then: that job is served,
/// not taken for a broken consumer's.
#[test]
fn a_buffer_given_back_and_named_again_while_jobs_are_read_is_no_broken_consumer() {
    let again = Job {
        identity: 2,
        kind: JobKind::Send,
        endpoint: 0x85,
        buffer: 0,
        length: 1,
    };
    let host = MockUsbFunction::new();
    let consumer = MockStream::with_bytes(BUFFERS, BYTES);
    let acting = Acting {
        inner: consumer.clone(),
        after: Box::new(move |consumer, frame| {
            if frame.identity != 1 {
                return;
            }
            // The first job's completion: taken, its buffer given back and named again.
            let done = consumer.take_frame().expect("a completion");
            assert_eq!((done.buffer, done.identity), (0, 1));
            consumer.return_buffer(done.buffer);
            let mut bytes = [0u8; JOB_BYTES];
            let mut writer = Writer::new(&mut bytes);
            again.encode(&mut writer).unwrap();
            let used = writer.len();
            consumer.post_job(&bytes[..used]);
        }),
    };
    let driver = bind_on(Box::new(host.clone()), Some(Box::new(acting)), &[]);
    let mut rig = Rig {
        driver,
        host,
        consumer,
        now: 0,
    };
    rig.tick();
    rig.host.event(UsbFunctionEvent::Configured);
    rig.tick();
    // No endpoint of that address: refused, which completes it as the jobs are read.
    rig.send(1, 0x85, 0, b"x");
    rig.tick();
    assert_eq!(rig.consumer.faults(), 0);
    assert_eq!(rig.failed(2), JobStatus::Refused);
    assert!(rig.completion().is_none());
}

/// How a test posts one job.
type Post<'a> = &'a dyn Fn(&Rig);

/// A job with nothing to complete on is a broken consumer's: the stream is faulted, and what
/// waited under that consumer goes with it.
#[test]
fn a_job_with_no_buffer_to_complete_on_faults_its_consumer() {
    let broken: [(&str, Post<'_>); 8] = [
        ("a buffer the stream doesn't have", &|rig| {
            rig.receive(11, OUT, BUFFERS, 64);
        }),
        ("a buffer past what the driver holds", &|rig| {
            rig.receive(12, OUT, 64, 64);
        }),
        ("the largest buffer number", &|rig| {
            rig.receive(13, OUT, u32::MAX, 64);
        }),
        ("a buffer the consumer has", &|rig| {
            rig.receive(14, OUT, 0, 64);
        }),
        ("a buffer a waiting job has", &|rig| {
            rig.receive(15, OUT, 5, 64);
        }),
        ("too few bytes", &|rig| rig.consumer.post_job(&[1, 2, 3])),
        ("too many bytes", &|rig| {
            rig.consumer.post_job(&[0u8; JOB_BYTES + 1]);
        }),
        ("a kind nobody knows", &|rig| {
            let mut unknown = [0u8; JOB_BYTES];
            unknown[8] = 9;
            unknown[10] = 6;
            rig.consumer.post_job(&unknown);
        }),
    ];
    for (what, post) in broken {
        let mut rig = Rig::configured();
        // Buffer 0 completes and stays with the consumer; buffer 5 has a job waiting.
        rig.send(1, IN, 0, b"x");
        rig.receive(10, OUT, 5, 64);
        rig.tick();
        assert_eq!(rig.done(1), b"", "{what}");
        assert_eq!(rig.driver.serve.waiting(), 1, "{what}");
        let before = rig.counters().errors;
        post(&rig);
        // A job behind the broken one is not run.
        rig.send(20, IN, 6, b"after");
        rig.tick();
        assert_eq!(rig.consumer.faults(), 1, "{what}");
        assert_eq!(rig.counters().errors, before + 1, "{what}");
        assert_eq!(rig.driver.serve.waiting(), 0, "{what}");
        assert!(rig.completion().is_none(), "{what}");
        assert_eq!(rig.host.host_received(), [(IN, b"x".to_vec())], "{what}");
    }
}

#[test]
fn an_event_job_whose_buffer_cannot_hold_an_event_is_refused() {
    // Room for the record and one byte less than an event.
    let mut rig = Rig::with(DATA_OFFSET + EVENT_BYTES - 1, &[]);
    rig.want_event(1, 0);
    rig.host.event(UsbFunctionEvent::Configured);
    rig.tick();
    assert_eq!(rig.failed(1), JobStatus::Refused);
    assert_eq!(rig.driver.serve.queued(), 1, "the event waits for a job");

    // A buffer too small for the record itself completes empty.
    let mut tiny = Rig::with(COMPLETION_BYTES - 1, &[]);
    tiny.want_event(2, 0);
    tiny.tick();
    let frame = tiny.consumer.take_frame().unwrap();
    assert_eq!((frame.identity, frame.bytes), (2, 0));
    assert_eq!(tiny.counters().errors, 1);
}

#[test]
fn a_send_waits_for_its_endpoint_and_holds_only_that_endpoint() {
    let mut rig = Rig::configured();
    rig.host.set_busy(IN, true);
    rig.send(1, IN, 0, b"first");
    rig.send(2, IN, 1, b"second");
    rig.send(3, STATUS, 2, b"status");
    rig.receive(4, OUT, 3, 64);
    rig.host.host_sends(OUT, b"in");
    rig.tick();
    // The other endpoints carried on, in the order their jobs came.
    assert_eq!(rig.done(3), b"");
    assert_eq!(rig.done(4), b"in");
    assert!(rig.completion().is_none());
    assert_eq!(rig.host.host_received(), [(STATUS, b"status".to_vec())]);

    // The consumer can't change a send once it waits: it was read when first tried.
    rig.tick();
    rig.host.set_busy(IN, false);
    rig.tick();
    assert_eq!(rig.done(1), b"");
    assert_eq!(rig.done(2), b"");
    assert_eq!(
        rig.host.host_received()[1..],
        [(IN, b"first".to_vec()), (IN, b"second".to_vec())]
    );
}

#[test]
fn receives_on_one_endpoint_are_filled_in_the_order_they_were_posted() {
    let mut rig = Rig::configured();
    rig.receive(1, OUT, 0, 64);
    rig.receive(2, OUT, 1, 64);
    rig.host.host_sends(OUT, b"one");
    rig.host.host_sends(OUT, b"two");
    rig.tick();
    assert_eq!(rig.done(1), b"one");
    assert_eq!(rig.done(2), b"two");
}

#[test]
fn when_the_configuration_goes_transfers_fail_and_event_jobs_wait_on() {
    let mut rig = Rig::configured();
    rig.host.set_busy(IN, true);
    rig.send(1, IN, 0, b"lost");
    rig.receive(2, OUT, 1, 64);
    rig.want_event(3, 2);
    rig.want_event(4, 3);
    rig.tick();
    // The event job took what was queued when the consumer came: the configuration.
    assert_eq!(rig.event(3).kind, EventKind::Configured);
    rig.host.event(UsbFunctionEvent::Deconfigured);
    rig.tick();
    assert_eq!(rig.failed(1), JobStatus::NotConfigured);
    assert_eq!(rig.failed(2), JobStatus::NotConfigured);
    assert_eq!(rig.event(4).kind, EventKind::Deconfigured);

    // Posted with no configuration, a transfer fails the same way.
    rig.receive(5, OUT, 4, 64);
    rig.tick();
    assert_eq!(rig.failed(5), JobStatus::NotConfigured);

    // And when the controller itself lets go.
    rig.host.event(UsbFunctionEvent::Configured);
    rig.tick();
    rig.receive(6, OUT, 5, 64);
    rig.want_event(7, 6);
    rig.tick();
    assert_eq!(rig.event(7).kind, EventKind::Configured);
    rig.consumer.return_buffer(6);
    rig.want_event(8, 6);
    rig.host.release();
    rig.tick();
    assert_eq!(rig.failed(6), JobStatus::NotConfigured);
    assert_eq!(Driver::health(&rig.driver), Health::Failed(LET_GO));
    let state = UsbFunction::state(&mut rig.driver).unwrap();
    assert!(!state.up && !state.configured);
}

#[test]
fn a_request_nobody_answers_in_time_is_stalled_and_nothing_is_read_while_it_waits() {
    let mut rig = Rig::with(BYTES, &[("control_timeout_ms", ConfigValue::Int(5))]);
    rig.want_event(1, 0);
    rig.want_event(2, 1);
    rig.host.request(request(0xc0), 4, &[]);
    rig.host.event(UsbFunctionEvent::Configured);
    rig.tick();
    assert_eq!(rig.event(1).kind, EventKind::Setup);
    // The event behind the request is not read: reading on would stall it.
    for _ in 0..3 {
        rig.tick();
    }
    assert!(rig.completion().is_none());
    assert!(rig.host.answers().is_empty());
    assert!(!rig.driver.configured());

    // An answer of the wrong direction leaves it waiting.
    assert_eq!(
        UsbFunction::control_out(&mut rig.driver, &mut [0u8; 4]),
        Err(Status::INVALID_ARGUMENT)
    );
    assert!(UsbFunction::state(&mut rig.driver).unwrap().request_pending);

    for _ in 0..2 {
        rig.tick();
    }
    assert_eq!(
        rig.host.answers(),
        [(request(0xc0), ControlAnswer::Stalled)]
    );
    assert_eq!(rig.counters().requests_unanswered, 1);
    assert_eq!(rig.event(2).kind, EventKind::Configured);
    // The answer comes too late: there is nothing to answer.
    assert_eq!(
        UsbFunction::control_in(&mut rig.driver, b"late"),
        Err(Status::INVALID_ARGUMENT)
    );
}

#[test]
fn a_consumer_that_goes_takes_its_jobs_and_its_request_with_it() {
    let mut rig = Rig::configured();
    rig.want_event(1, 0);
    rig.receive(2, OUT, 1, 64);
    rig.receive(3, OUT, 2, 64);
    rig.host.request(request(0xc0), 4, &[]);
    rig.tick();
    assert_eq!(rig.event(1).kind, EventKind::Configured);
    rig.tick();
    assert_eq!(rig.driver.serve.waiting(), 2);
    rig.consumer.return_buffer(0);
    rig.want_event(4, 0);
    rig.tick();
    assert_eq!(rig.event(4).kind, EventKind::Setup);

    // Another consumer takes the stream between two ticks, on memory of its own.
    rig.consumer.change_consumer();
    rig.host.host_sends(OUT, b"new");
    rig.receive(20, OUT, 1, 64);
    rig.tick();
    // The request the old consumer was asked is stalled; its jobs are gone.
    assert_eq!(
        rig.host.answers(),
        [(request(0xc0), ControlAnswer::Stalled)]
    );
    assert_eq!(rig.counters().requests_unanswered, 1);
    // The new consumer's job, naming a buffer the old one's job had, is served.
    assert_eq!(rig.done(20), b"new");
    assert!(rig.completion().is_none());
    assert_eq!(rig.driver.serve.waiting(), 0);
    assert_eq!(rig.driver.serve.queued(), 0);
}

#[test]
fn cancel_takes_back_the_jobs_of_one_endpoint_or_the_event_jobs() {
    let mut rig = Rig::configured();
    rig.host.set_busy(IN, true);
    rig.receive(1, OUT, 0, 64);
    rig.send(2, IN, 1, b"held");
    rig.receive(3, OUT, 2, 64);
    rig.want_event(4, 3);
    rig.tick();
    assert_eq!(rig.event(4).kind, EventKind::Configured);
    rig.consumer.return_buffer(3);
    rig.want_event(5, 3);
    rig.tick();

    assert_eq!(UsbFunction::cancel(&mut rig.driver, OUT), Ok(2));
    assert_eq!(rig.failed(1), JobStatus::Cancelled);
    assert_eq!(rig.failed(3), JobStatus::Cancelled);
    assert_eq!(UsbFunction::cancel(&mut rig.driver, OUT), Ok(0));
    assert_eq!(UsbFunction::cancel(&mut rig.driver, 0), Ok(1));
    assert_eq!(rig.failed(5), JobStatus::Cancelled);
    assert_eq!(UsbFunction::cancel(&mut rig.driver, IN), Ok(1));
    assert_eq!(rig.failed(2), JobStatus::Cancelled);
    assert!(rig.host.host_received().is_empty());

    // A cancelled buffer is the consumer's until it returns it, then usable again.
    rig.consumer.return_buffer(0);
    rig.receive(6, OUT, 0, 64);
    rig.host.host_sends(OUT, b"again");
    rig.tick();
    assert_eq!(rig.done(6), b"again");
}

#[test]
fn halting_an_endpoint_goes_to_the_function() {
    let mut rig = Rig::configured();
    assert_eq!(UsbFunction::set_halt(&mut rig.driver, OUT, true), Ok(()));
    assert!(rig.host.is_halted(OUT));
    assert_eq!(UsbFunction::set_halt(&mut rig.driver, OUT, false), Ok(()));
    assert!(!rig.host.is_halted(OUT));
    assert_eq!(
        UsbFunction::set_halt(&mut rig.driver, 0x85, true),
        Err(Status::INVALID_ARGUMENT)
    );
    rig.host.event(UsbFunctionEvent::Deconfigured);
    rig.tick();
    assert_eq!(
        UsbFunction::set_halt(&mut rig.driver, OUT, true),
        Err(Status::NO_DEVICE)
    );
}

#[test]
fn events_nobody_takes_are_kept_in_order_and_the_oldest_dropped_past_the_limit() {
    let mut rig = Rig::configured();
    // The consumer is there (a tick has seen it), and takes no events.
    for round in 0..MAX_EVENTS {
        let event = if round % 2 == 0 {
            UsbFunctionEvent::Suspended
        } else {
            UsbFunctionEvent::Resumed
        };
        rig.host.event(event);
        rig.tick();
    }
    // The queue held the configuration and then these: one too many.
    assert_eq!(rig.driver.serve.queued(), MAX_EVENTS);
    assert_eq!(rig.counters().events_dropped, 1);
    rig.want_event(1, 0);
    rig.tick();
    assert_eq!(rig.event(1).kind, EventKind::Suspended);
}

#[test]
fn at_most_the_limit_wait_and_the_rest_stay_on_the_queue() {
    let host = MockUsbFunction::new();
    let consumer = MockStream::with_bytes(MAX_PENDING as u32 + 4, 256);
    let mut driver = bind_on(
        Box::new(host.clone()),
        Some(Box::new(consumer.clone())),
        &[],
    );
    let mut rig = Rig {
        driver: {
            Driver::tick(&mut driver, 0).unwrap();
            driver
        },
        host,
        consumer,
        now: 0,
    };
    rig.host.event(UsbFunctionEvent::Configured);
    rig.tick();
    for buffer in 0..MAX_PENDING as u32 + 4 {
        rig.receive(u64::from(buffer), OUT, buffer, 64);
    }
    rig.tick();
    rig.tick();
    assert_eq!(rig.driver.serve.waiting(), MAX_PENDING);
    for round in 0..MAX_PENDING as u64 + 4 {
        rig.host.host_sends(OUT, &[round as u8]);
        rig.tick();
        assert_eq!(rig.done(round), [round as u8]);
    }
}

/// A function that takes and gives bytes without allocating: what the host sends is a
/// counter's bytes, and what it is sent is summed.
#[derive(Default)]
struct Quiet {
    enabled: bool,
    configured: bool,
    sent: u64,
    taken: u64,
}

impl HalFunction for Quiet {
    type Error = ErrorKind;

    fn enable(&mut self, _descriptors: &[u8]) -> core::result::Result<(), ErrorKind> {
        self.enabled = true;
        Ok(())
    }

    fn disable(&mut self) -> core::result::Result<(), ErrorKind> {
        self.enabled = false;
        Ok(())
    }

    fn next_event(&mut self) -> core::result::Result<Option<UsbFunctionEvent>, ErrorKind> {
        if self.configured {
            return Ok(None);
        }
        self.configured = true;
        Ok(Some(UsbFunctionEvent::Configured))
    }

    fn control_in(&mut self, _: &[u8], _: Duration) -> core::result::Result<usize, ErrorKind> {
        Err(ErrorKind::InvalidInput)
    }

    fn control_out(&mut self, _: &mut [u8], _: Duration) -> core::result::Result<usize, ErrorKind> {
        Err(ErrorKind::InvalidInput)
    }

    fn control_stall(&mut self) -> core::result::Result<(), ErrorKind> {
        Err(ErrorKind::InvalidInput)
    }

    fn set_halt(&mut self, _: u8, _: bool) -> core::result::Result<(), ErrorKind> {
        Ok(())
    }

    fn read(
        &mut self,
        _endpoint: u8,
        data: &mut [u8],
        _timeout: Duration,
    ) -> core::result::Result<usize, ErrorKind> {
        self.sent += 1;
        data.fill(self.sent as u8);
        Ok(data.len())
    }

    fn write(
        &mut self,
        _endpoint: u8,
        data: &[u8],
        _timeout: Duration,
    ) -> core::result::Result<usize, ErrorKind> {
        self.taken += data.len() as u64;
        Ok(data.len())
    }
}

/// A stream that hands the driver jobs and takes its completions without allocating: buffer 0
/// is asked to receive and buffer 1 to send what came, over and over.
struct Loop {
    memory: [[u8; 1024]; 2],
    /// Which buffers the driver may take.
    free: [bool; 2],
    job: u64,
    published: u64,
}

impl StreamEndpoint for Loop {
    type Error = ErrorKind;

    fn buffer_count(&self) -> u32 {
        2
    }

    fn publish(&mut self, frame: StreamFrame) -> core::result::Result<(), ErrorKind> {
        self.published += 1;
        // The consumer returns it at once.
        self.free[frame.buffer as usize] = true;
        Ok(())
    }

    fn next_buffer(&mut self) -> core::result::Result<Option<u32>, ErrorKind> {
        let at = self.free.iter().position(|free| *free);
        if let Some(at) = at {
            self.free[at] = false;
        }
        Ok(at.map(|at| at as u32))
    }

    fn next_job(&mut self, out: &mut [u8]) -> core::result::Result<Option<usize>, ErrorKind> {
        // One of each per round, and only when its buffer is back with the driver.
        let (kind, endpoint, buffer) = if self.job.is_multiple_of(2) {
            (JobKind::Receive, OUT, 0)
        } else {
            (JobKind::Send, IN, 1)
        };
        if self.free[buffer as usize] || self.job >= self.published + 2 {
            return Ok(None);
        }
        let job = Job {
            identity: self.job,
            kind,
            endpoint,
            buffer,
            length: 512,
        };
        self.job += 1;
        let mut writer = Writer::new(out);
        job.encode(&mut writer).map_err(|_| ErrorKind::Failed)?;
        Ok(Some(writer.len()))
    }

    fn write(
        &mut self,
        buffer: u32,
        offset: usize,
        data: &[u8],
    ) -> core::result::Result<bool, ErrorKind> {
        let to = self.memory[buffer as usize]
            .get_mut(offset..offset + data.len())
            .ok_or(ErrorKind::InvalidInput)?;
        to.copy_from_slice(data);
        Ok(true)
    }

    fn read(
        &mut self,
        buffer: u32,
        offset: usize,
        out: &mut [u8],
    ) -> core::result::Result<bool, ErrorKind> {
        let from = self.memory[buffer as usize]
            .get(offset..offset + out.len())
            .ok_or(ErrorKind::InvalidInput)?;
        out.copy_from_slice(from);
        Ok(true)
    }

    fn buffer_handle(
        &self,
        _buffer: u32,
    ) -> core::result::Result<Option<lemnos_hal::blocks::BufferHandle<'_>>, ErrorKind> {
        Err(ErrorKind::Unsupported)
    }

    fn buffer_kind(&self) -> lemnos_hal::blocks::BufferKind {
        lemnos_hal::blocks::BufferKind::Plain
    }

    fn fault_consumer(&mut self) -> core::result::Result<(), ErrorKind> {
        Ok(())
    }

    fn buffer_current(&self, _buffer: u32) -> core::result::Result<bool, ErrorKind> {
        Ok(true)
    }
}

#[test]
fn carrying_bytes_both_ways_allocates_nothing() {
    let stream = Loop {
        memory: [[0; 1024]; 2],
        free: [true; 2],
        job: 0,
        published: 0,
    };
    let mut driver = bind_on(Box::<Quiet>::default(), Some(Box::new(stream)), &[]);
    let mut now = 0;
    let mut tick = |driver: &mut UsbGadget| {
        now = Driver::tick(driver, now).unwrap().max(now + 1);
    };
    // Up, configured, and the first jobs through.
    for _ in 0..8 {
        tick(&mut driver);
    }
    let warm = UsbFunction::counters(&mut driver).unwrap();
    assert!(warm.jobs > 0 && warm.errors == 0, "{warm:?}");
    let ((), allocated) = allocations(|| {
        for _ in 0..1000 {
            tick(&mut driver);
        }
    });
    let counters = UsbFunction::counters(&mut driver).unwrap();
    assert_eq!(allocated, 0);
    assert!(counters.jobs >= warm.jobs + 1000, "{counters:?}");
    assert_eq!(counters.errors, 0);
}
