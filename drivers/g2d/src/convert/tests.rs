use std::cell::RefCell;
use std::os::fd::{AsRawFd, OwnedFd};
use std::rc::Rc;

use lemnos_core::{Encode, Writer};
use lemnos_hal::ErrorKind;
use lemnos_hal::mock::MockStream;

use super::*;
use crate::HardwareVersion;
use crate::engine::tests::{Plan, buffer, engine};

const ARGB: u32 = code(b"AR24");
const XRGB: u32 = code(b"XR24");
/// A buffer holds a 64 by 64 frame of four bytes a pixel.
const BYTES: u64 = 4 * 64 * 64;

/// An engine that keeps each request it was given, as text.
struct Recording(Rc<RefCell<Vec<String>>>);

impl Engine for Recording {
    fn version(&self) -> io::Result<HardwareVersion> {
        Ok(HardwareVersion {
            g2d_version: 1,
            chip_version: 1,
        })
    }

    fn run(&self, job: &Job<'_>) -> io::Result<()> {
        self.0.borrow_mut().push(format!("{job:?}"));
        Ok(())
    }
}

/// The descriptors a recorded request names, in the order it names them.
fn descriptors(request: &str) -> Vec<i32> {
    request
        .split("fd: ")
        .skip(1)
        .map(|rest| {
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            digits.parse().expect("a descriptor")
        })
        .collect()
}

/// A stream of buffers behind handles, of the given lengths, and each buffer's descriptor.
fn backed(lengths: &[u64]) -> (MockStream, Vec<i32>) {
    let buffers: Vec<OwnedFd> = lengths.iter().map(|len| buffer(*len)).collect();
    let raw = buffers.iter().map(AsRawFd::as_raw_fd).collect();
    (MockStream::with_handles(buffers), raw)
}

fn image(buffer: u32, width: u32, height: u32, fourcc: u32) -> ImageRef {
    ImageRef {
        buffer,
        width,
        height,
        fourcc,
        stride: width * 4,
    }
}

/// A 32 by 32 conversion from `input` to `output`.
fn job(identity: u64, input: u32, output: u32) -> ConvertJob {
    ConvertJob {
        identity,
        input: image(input, 32, 32, ARGB),
        output: image(output, 32, 32, XRGB),
    }
}

fn post(consumer: &MockStream, job: &ConvertJob) {
    let mut bytes = [0u8; JOB_BYTES];
    let mut writer = Writer::new(&mut bytes);
    job.encode(&mut writer).expect("encode");
    let used = writer.len();
    consumer.post_job(&bytes[..used]);
}

#[test]
fn a_job_runs_on_the_handles_of_the_buffers_it_names_and_completes_on_its_output() {
    let (mut stream, raw) = backed(&[BYTES; 4]);
    let consumer = stream.clone();
    let requests = Rc::new(RefCell::new(Vec::new()));
    let seen = Rc::clone(&requests);
    let mut converter = Converter::new(G2d::new(move || Ok(Recording(Rc::clone(&seen)))));
    post(&consumer, &job(0xABCD, 1, 2));
    assert_eq!(converter.serve(&mut stream, 77), Ok(1));
    // The engine was given the dma-bufs behind buffers 1 and 2, and nothing else.
    let requests = requests.borrow();
    assert_eq!(requests.len(), 1);
    assert_eq!(descriptors(&requests[0]), [raw[1], raw[2]], "{requests:?}");
    // The completion is the output buffer, with the job's identity and the bytes written.
    let frame = consumer.take_frame().expect("a completion");
    assert_eq!(
        (
            frame.buffer,
            frame.identity,
            frame.bytes,
            frame.timestamp_ns
        ),
        (2, 0xABCD, 32 * 32 * 4, 77)
    );
    assert_eq!(consumer.take_frame(), None);
    assert_eq!(converter.counters(), Counters { jobs: 1, errors: 0 });
    assert_eq!(converter.last_failure(), None);
    // The input stays the driver's; the output is the consumer's until it comes back.
    assert!(converter.holds(1));
    assert!(!converter.holds(2));
    consumer.return_buffer(2);
    assert_eq!(converter.serve(&mut stream, 78), Ok(0));
    assert!(converter.holds(2));
}

/// Serves one job on an engine that only counts, and returns its completion and how it went.
fn one(
    stream: &mut MockStream,
    job: &ConvertJob,
) -> (Option<StreamFrame>, Counters, Option<Failure>, u32) {
    let plan = Rc::new(Plan::default());
    let mut converter = Converter::new(engine(&plan));
    let consumer = stream.clone();
    post(&consumer, job);
    assert_eq!(converter.serve(stream, 5), Ok(1));
    (
        consumer.take_frame(),
        converter.counters(),
        converter.last_failure(),
        plan.runs.get(),
    )
}

const FAILED: Counters = Counters { jobs: 0, errors: 1 };

/// What a failed job leaves: an empty completion on `output` carrying its identity.
fn failed_on(frame: Option<StreamFrame>, output: u32, identity: u64) {
    let frame = frame.expect("a completion");
    assert_eq!(
        (frame.buffer, frame.identity, frame.bytes),
        (output, identity, 0)
    );
}

#[test]
fn a_frame_larger_than_the_dma_buf_behind_its_buffer_never_reaches_the_engine() {
    // The output buffer's dma-buf is shorter than the others: one page.
    let (mut stream, _) = backed(&[BYTES, BYTES, 4096]);
    // 128 by 64 at four bytes is twice what the input holds.
    let wide = ConvertJob {
        identity: 1,
        input: image(0, 128, 64, ARGB),
        output: image(1, 128, 64, XRGB),
    };
    let (frame, counters, failure, runs) = one(&mut stream, &wide);
    assert_eq!(
        (counters, failure, runs),
        (FAILED, Some(Failure::Surface), 0)
    );
    failed_on(frame, 1, 1);
    // A frame that fits the stream's other buffers but not this one's real size.
    let (mut stream, _) = backed(&[BYTES, BYTES, 4096]);
    let full = ConvertJob {
        identity: 2,
        input: image(0, 64, 64, ARGB),
        output: image(2, 64, 64, XRGB),
    };
    let (frame, counters, failure, runs) = one(&mut stream, &full);
    assert_eq!(
        (counters, failure, runs),
        (FAILED, Some(Failure::Surface), 0)
    );
    failed_on(frame, 2, 2);
    // One row more than the page holds.
    let (mut stream, _) = backed(&[BYTES, BYTES, 4096]);
    let tall = ConvertJob {
        identity: 3,
        input: image(0, 32, 33, ARGB),
        output: image(2, 32, 33, XRGB),
    };
    let (frame, counters, failure, runs) = one(&mut stream, &tall);
    assert_eq!(
        (counters, failure, runs),
        (FAILED, Some(Failure::Surface), 0)
    );
    failed_on(frame, 2, 3);
    // Exactly the page does run: 32 rows of 32 pixels.
    let (mut stream, _) = backed(&[BYTES, BYTES, 4096]);
    let (frame, counters, failure, runs) = one(&mut stream, &job(4, 0, 2));
    assert_eq!(
        (counters, failure, runs),
        (Counters { jobs: 1, errors: 0 }, None, 1)
    );
    assert_eq!(frame.expect("a completion").bytes, 4096);
}

#[test]
fn a_job_can_only_name_buffers_the_driver_holds() {
    let refused = |job: &ConvertJob, completes: bool| {
        let (mut stream, _) = backed(&[BYTES; 3]);
        let (frame, counters, failure, runs) = one(&mut stream, job);
        assert_eq!(
            (counters, failure, runs),
            (FAILED, Some(Failure::Buffers), 0),
            "{job:?}"
        );
        assert_eq!(frame.is_some(), completes, "{job:?}");
    };
    refused(&job(1, 1, 1), false); // the same buffer twice
    refused(&job(1, 3, 0), true); // an input past the stream
    refused(&job(1, 0, 3), false); // an output past the stream
    refused(&job(1, u32::MAX, 0), true);
    refused(&job(1, 0, MAX_BUFFERS), false);
    // A buffer the consumer still has is not the driver's to use.
    let (mut stream, _) = backed(&[BYTES; 3]);
    let consumer = stream.clone();
    let plan = Rc::new(Plan::default());
    let mut converter = Converter::new(engine(&plan));
    post(&consumer, &job(1, 0, 1));
    assert_eq!(converter.serve(&mut stream, 0), Ok(1));
    assert_eq!(consumer.take_frame().expect("a completion").buffer, 1);
    post(&consumer, &job(2, 1, 2)); // reads the output it was just given
    post(&consumer, &job(3, 0, 1)); // writes it again
    assert_eq!(converter.serve(&mut stream, 0), Ok(2));
    assert_eq!(converter.counters(), Counters { jobs: 1, errors: 2 });
    assert_eq!(converter.last_failure(), Some(Failure::Buffers));
    assert_eq!(plan.runs.get(), 1);
    failed_on(consumer.take_frame(), 2, 2);
    assert_eq!(consumer.take_frame(), None, "nothing to complete job 3 on");
}

#[test]
fn a_job_the_engine_cant_do_fails_before_any_handle_is_asked_for() {
    let (mut stream, _) = backed(&[BYTES; 2]);
    let mut odd = job(1, 0, 1);
    odd.output.width = 16;
    let (frame, counters, failure, runs) = one(&mut stream, &odd);
    assert_eq!((counters, failure, runs), (FAILED, Some(Failure::Size), 0));
    failed_on(frame, 1, 1);
    let formats = [
        (code(b"NV12"), 128), // not a format a job can say enough about
        (ARGB, 126),          // a stride that isn't whole pixels
        (ARGB, 124),          // a stride narrower than the image
        (0, 128),
    ];
    for (fourcc, stride) in formats {
        let (mut stream, _) = backed(&[BYTES; 2]);
        let mut bad = job(1, 0, 1);
        bad.input.fourcc = fourcc;
        bad.input.stride = stride;
        let (frame, counters, failure, runs) = one(&mut stream, &bad);
        assert_eq!(
            (counters, failure, runs),
            (FAILED, Some(Failure::Format), 0),
            "{fourcc:#x} {stride}"
        );
        failed_on(frame, 1, 1);
    }
    // A padded row is a wider surface, and still has to fit the dma-buf.
    let (mut stream, _) = backed(&[BYTES; 2]);
    let mut padded = job(1, 0, 1);
    padded.input.stride = 256;
    padded.input.height = 64;
    padded.output.height = 64;
    let (_, counters, failure, runs) = one(&mut stream, &padded);
    assert_eq!(
        (counters, failure, runs),
        (Counters { jobs: 1, errors: 0 }, None, 1)
    );
}

#[test]
fn a_stream_without_handles_fails_every_job_and_says_why() {
    let mut stream = MockStream::with_bytes(2, BYTES as usize);
    let (frame, counters, failure, runs) = one(&mut stream, &job(9, 0, 1));
    assert_eq!(
        (counters, failure, runs),
        (FAILED, Some(Failure::NoHandle), 0)
    );
    failed_on(frame, 1, 9);
}

#[test]
fn buffers_held_from_before_a_new_consumer_are_given_up_and_taken_again() {
    let (mut stream, raw) = backed(&[BYTES; 3]);
    let consumer = stream.clone();
    let requests = Rc::new(RefCell::new(Vec::new()));
    let seen = Rc::clone(&requests);
    let mut converter = Converter::new(G2d::new(move || Ok(Recording(Rc::clone(&seen)))));
    assert_eq!(converter.serve(&mut stream, 0), Ok(0));
    assert!(converter.holds(0) && converter.holds(1) && converter.holds(2));
    // Nothing is behind what the driver holds now.
    consumer.change_consumer();
    assert_eq!(
        stream.buffer_handle(0).map(|handle| handle.is_some()),
        Ok(false)
    );
    // The new consumer's first job still runs, and completes to it.
    post(&consumer, &job(6, 0, 1));
    assert_eq!(converter.serve(&mut stream, 0), Ok(1));
    assert_eq!(converter.counters(), Counters { jobs: 1, errors: 0 });
    assert_eq!(descriptors(&requests.borrow()[0]), [raw[0], raw[1]]);
    let frame = consumer.take_frame().expect("a completion");
    assert_eq!((frame.buffer, frame.identity), (1, 6));
    assert_eq!(
        consumer.take_frame(),
        None,
        "what was given up reached no one"
    );
    assert!(converter.holds(0) && converter.holds(2));
    // A job that fails still completes to the consumer that posted it.
    consumer.return_buffer(1);
    assert_eq!(converter.serve(&mut stream, 0), Ok(0));
    consumer.change_consumer();
    let mut odd = job(8, 0, 1);
    odd.output.width = 16;
    post(&consumer, &odd);
    assert_eq!(converter.serve(&mut stream, 0), Ok(1));
    assert_eq!(converter.last_failure(), Some(Failure::Size));
    failed_on(consumer.take_frame(), 1, 8);
}

#[test]
fn an_engine_that_fails_completes_the_job_as_failed_and_is_tried_again() {
    let (mut stream, _) = backed(&[BYTES; 3]);
    let consumer = stream.clone();
    let plan = Rc::new(Plan::default());
    let mut converter = Converter::new(engine(&plan));
    plan.run_errno.set(Some(110)); // ETIMEDOUT
    post(&consumer, &job(1, 0, 1));
    assert_eq!(converter.serve(&mut stream, 0), Ok(1));
    assert_eq!(converter.last_failure(), Some(Failure::Engine));
    failed_on(consumer.take_frame(), 1, 1);
    // Backing off: the job is answered, the engine not touched.
    plan.run_errno.set(None);
    post(&consumer, &job(2, 0, 2));
    assert_eq!(converter.serve(&mut stream, 1), Ok(1));
    failed_on(consumer.take_frame(), 2, 2);
    assert_eq!(plan.opens.get(), 1);
    // Due: it reopens and the next job runs.
    consumer.return_buffer(1);
    post(&consumer, &job(3, 0, 1));
    assert_eq!(converter.serve(&mut stream, 250_000_000), Ok(1));
    assert_eq!(consumer.take_frame().expect("a completion").bytes, 4096);
    assert_eq!(converter.counters(), Counters { jobs: 1, errors: 2 });
    assert_eq!(plan.opens.get(), 2);
}

#[test]
fn bytes_that_are_not_a_job_are_counted_and_the_next_job_runs() {
    let (mut stream, _) = backed(&[BYTES; 2]);
    let consumer = stream.clone();
    let plan = Rc::new(Plan::default());
    let mut converter = Converter::new(engine(&plan));
    consumer.post_job(&[1, 2, 3]);
    post(&consumer, &job(4, 0, 1));
    assert_eq!(converter.serve(&mut stream, 0), Ok(2));
    assert_eq!(converter.counters(), Counters { jobs: 1, errors: 1 });
    assert_eq!(converter.last_failure(), Some(Failure::Undecodable));
    assert_eq!(consumer.take_frame().expect("a completion").identity, 4);
    assert_eq!(consumer.take_frame(), None);
    // A job longer than any the contract has is taken off the queue too, not left at its head.
    consumer.return_buffer(1);
    consumer.post_job(&[0; JOB_BYTES + 1]);
    post(&consumer, &job(5, 0, 1));
    assert_eq!(converter.serve(&mut stream, 0), Ok(2));
    assert_eq!(converter.counters(), Counters { jobs: 2, errors: 2 });
    assert_eq!(consumer.take_frame().expect("a completion").identity, 5);
    // One the stream itself can't hand over is the stream's error.
    consumer.post_job(&[0; LONGEST_JOB + 1]);
    assert_eq!(
        converter.serve(&mut stream, 0),
        Err(ErrorKind::InvalidInput)
    );
}

#[test]
fn every_format_has_a_distinct_code_and_whole_pixels() {
    for (i, (fourcc, format)) in FORMATS.iter().enumerate() {
        assert_eq!(super::format(*fourcc), Some(*format));
        assert!(format.is_rgb());
        assert!(FORMATS[..i].iter().all(|(other, _)| other != fourcc));
    }
    assert_eq!(super::format(code(b"NV12")), None);
}
