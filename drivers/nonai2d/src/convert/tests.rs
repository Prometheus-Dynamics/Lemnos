use std::cell::RefCell;
use std::os::fd::{AsRawFd, OwnedFd};
use std::rc::Rc;

use lemnos_core::{Encode, Writer};
use lemnos_hal::ErrorKind;
use lemnos_hal::mock::MockStream;

use super::*;
use crate::engine::tests::{Plan, buffer, engine};
use crate::layout::MAX_WIDTH;

const ARGB: u32 = code(b"AR24");
const XRGB: u32 = code(b"XR24");
/// A buffer holds a 64 by 64 frame of four bytes a pixel.
const BYTES: u64 = 4 * 64 * 64;

/// The descriptors and layouts of each conversion an engine was given.
type Seen = Rc<RefCell<Vec<(i32, i32, Layout, Layout)>>>;

/// An engine that keeps the descriptors and layouts of each conversion it was given.
struct Recording {
    seen: Seen,
    layouts: (Layout, Layout),
}

impl Engine for Recording {
    fn layouts(&self) -> (Layout, Layout) {
        self.layouts
    }

    fn convert(&mut self, src: &Frame<'_>, dst: &Frame<'_>) -> io::Result<()> {
        let fd = |frame: &Frame<'_>| frame.plane(0).map_or(-1, |(fd, _)| fd.as_raw_fd());
        self.seen
            .borrow_mut()
            .push((fd(src), fd(dst), src.layout(), dst.layout()));
        Ok(())
    }
}

fn recording(
    seen: &Seen,
) -> Converter<Recording, impl FnMut(Layout, Layout) -> io::Result<Recording> + use<>> {
    let seen = Rc::clone(seen);
    Converter::new(Nonai2d::new(move |src, dst| {
        Ok(Recording {
            seen: Rc::clone(&seen),
            layouts: (src, dst),
        })
    }))
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
    let seen = Seen::default();
    let mut converter = recording(&seen);
    post(&consumer, &job(0xABCD, 1, 2));
    assert_eq!(converter.serve(&mut stream, 77), Ok(1));
    // The engine was given the dma-bufs behind buffers 1 and 2, in the layouts the job named.
    let argb = Layout::new(PixelFormat::Abgr32, 32, 32).expect("layout");
    let xrgb = Layout::new(PixelFormat::Xbgr32, 32, 32).expect("layout");
    assert_eq!(*seen.borrow(), [(raw[1], raw[2], argb, xrgb)]);
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
    // The last buffer's dma-buf is shorter than the others: one page.
    let (mut stream, _) = backed(&[BYTES, BYTES, 4096]);
    // 128 by 64 at four bytes is twice what the input holds.
    let wide = ConvertJob {
        identity: 1,
        input: image(0, 128, 64, ARGB),
        output: image(1, 128, 64, XRGB),
    };
    let (frame, counters, failure, runs) = one(&mut stream, &wide);
    assert_eq!((counters, failure, runs), (FAILED, Some(Failure::Frame), 0));
    failed_on(frame, 1, 1);
    // A frame that fits the stream's other buffers but not this one's real size.
    let (mut stream, _) = backed(&[BYTES, BYTES, 4096]);
    let full = ConvertJob {
        identity: 2,
        input: image(0, 64, 64, ARGB),
        output: image(2, 64, 64, XRGB),
    };
    let (frame, counters, failure, runs) = one(&mut stream, &full);
    assert_eq!((counters, failure, runs), (FAILED, Some(Failure::Frame), 0));
    failed_on(frame, 2, 2);
    // One row more than the page holds, read from it.
    let (mut stream, _) = backed(&[BYTES, BYTES, 4096]);
    let tall = ConvertJob {
        identity: 3,
        input: image(2, 32, 33, ARGB),
        output: image(0, 32, 33, XRGB),
    };
    let (frame, counters, failure, runs) = one(&mut stream, &tall);
    assert_eq!((counters, failure, runs), (FAILED, Some(Failure::Frame), 0));
    failed_on(frame, 0, 3);
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
    let check = |bad: &ConvertJob, why: Failure| {
        let (mut stream, _) = backed(&[BYTES; 2]);
        let (frame, counters, failure, runs) = one(&mut stream, bad);
        assert_eq!((counters, failure, runs), (FAILED, Some(why), 0), "{bad:?}");
        failed_on(frame, 1, bad.identity);
    };
    let mut odd = job(1, 0, 1);
    odd.output.width = 16;
    odd.output.stride = 64;
    check(&odd, Failure::Size);
    let mut huge = job(1, 0, 1);
    for image in [&mut huge.input, &mut huge.output] {
        image.width = MAX_WIDTH + 2;
        image.stride = image.width * 4;
    }
    check(&huge, Failure::Size);
    let mut empty = job(1, 0, 1);
    empty.input.height = 0;
    empty.output.height = 0;
    check(&empty, Failure::Size);
    let formats = [
        (code(b"NV12"), 128), // a format of more planes than a job names
        (code(b"YU12"), 128),
        (ARGB, 132), // a padded row: the driver can't be told one
        (ARGB, 124), // a stride narrower than the image
        (ARGB, 126),
        (0, 128),
    ];
    for (fourcc, stride) in formats {
        let mut bad = job(1, 0, 1);
        bad.input.fourcc = fourcc;
        bad.input.stride = stride;
        check(&bad, Failure::Format);
    }
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
    let seen = Seen::default();
    let mut converter = recording(&seen);
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
    let first = seen.borrow()[0];
    assert_eq!((first.0, first.1), (raw[0], raw[1]));
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
    odd.output.stride = 64;
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
    plan.convert_error.set(Some(io::ErrorKind::TimedOut));
    post(&consumer, &job(1, 0, 1));
    assert_eq!(converter.serve(&mut stream, 0), Ok(1));
    assert_eq!(converter.last_failure(), Some(Failure::Engine));
    failed_on(consumer.take_frame(), 1, 1);
    // Backing off: the job is answered, the engine not touched.
    plan.convert_error.set(None);
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
    // A conversion the device refuses fails the job and starts no backoff.
    consumer.return_buffer(1);
    plan.open_refused.set(true);
    let mut swapped = job(4, 0, 1);
    (swapped.input.fourcc, swapped.output.fourcc) = (XRGB, ARGB);
    post(&consumer, &swapped);
    assert_eq!(converter.serve(&mut stream, 250_000_001), Ok(1));
    assert_eq!(converter.last_failure(), Some(Failure::Refused));
    failed_on(consumer.take_frame(), 1, 4);
    consumer.return_buffer(1);
    plan.open_refused.set(false);
    post(&consumer, &job(5, 0, 1));
    assert_eq!(converter.serve(&mut stream, 250_000_002), Ok(1));
    assert_eq!(consumer.take_frame().expect("a completion").bytes, 4096);
}

#[test]
fn bytes_that_are_not_a_job_fault_the_consumer_and_the_next_one_is_served() {
    let (mut stream, _) = backed(&[BYTES; 2]);
    let consumer = stream.clone();
    let plan = Rc::new(Plan::default());
    let mut converter = Converter::new(engine(&plan));
    consumer.post_job(&[1, 2, 3]);
    post(&consumer, &job(4, 0, 1));
    // The consumer is faulted at the bytes that are no job, and its job behind them runs nowhere.
    assert_eq!(converter.serve(&mut stream, 0), Ok(1));
    assert_eq!(consumer.faults(), 1);
    assert_eq!(converter.counters(), Counters { jobs: 0, errors: 1 });
    assert_eq!(converter.last_failure(), Some(Failure::Undecodable));
    assert_eq!(consumer.take_frame(), None);
    // What was held under it is given up, and the next consumer's job runs on buffers taken anew.
    assert!(!converter.holds(0) && !converter.holds(1));
    post(&consumer, &job(5, 0, 1));
    assert_eq!(converter.serve(&mut stream, 0), Ok(1));
    assert_eq!(consumer.take_frame().expect("a completion").identity, 5);
    // A job longer than any the contract has is no job either.
    consumer.return_buffer(1);
    consumer.post_job(&[0; JOB_BYTES + 1]);
    assert_eq!(converter.serve(&mut stream, 0), Ok(1));
    assert_eq!(consumer.faults(), 2);
    assert_eq!(converter.counters(), Counters { jobs: 1, errors: 2 });
    // One the stream itself can't hand over is the stream's error.
    consumer.post_job(&[0; LONGEST_JOB + 1]);
    assert_eq!(
        converter.serve(&mut stream, 0),
        Err(ErrorKind::InvalidInput)
    );
}

#[test]
fn every_format_has_a_distinct_code_one_plane_and_the_same_byte_order() {
    for (i, (fourcc, format)) in FORMATS.iter().enumerate() {
        assert_eq!(super::format(*fourcc), Some(*format));
        assert_eq!(format.plane_count(), 1);
        assert!(FORMATS[..i].iter().all(|(other, _)| other != fourcc));
    }
    // The four-byte formats whose DRM and V4L2 codes are spelled alike are the same in memory.
    assert_eq!(super::format(ARGB).map(PixelFormat::fourcc), Some(ARGB));
    assert_eq!(super::format(XRGB).map(PixelFormat::fourcc), Some(XRGB));
    // DRM BG24 is R, G, B in memory: V4L2 RGB3.
    assert_eq!(super::format(code(b"BG24")), Some(PixelFormat::Rgb24));
    assert_eq!(super::format(code(b"NV12")), None);
}

#[test]
fn every_pair_of_formats_is_a_conversion() {
    let n = FORMATS.len();
    assert_eq!(usize::from(CONVERSIONS), n * n);
    let all: Vec<_> = (0..CONVERSIONS).filter_map(conversion).collect();
    assert_eq!(all.len(), n * n);
    for (i, pair) in all.iter().enumerate() {
        assert!(all[..i].iter().all(|other| other != pair));
    }
    assert_eq!(conversion(CONVERSIONS), None);
    assert_eq!(conversion(u16::MAX), None);
}
