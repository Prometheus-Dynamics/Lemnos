use std::os::fd::OwnedFd;
use std::rc::Rc;

use lemnos_contract_image_scaler::Filter;
use lemnos_core::{Encode, Writer};
use lemnos_hal::ErrorKind;
use lemnos_hal::mock::MockStream;
use lemnos_measure::test_alloc::allocations;

use super::*;
use crate::engine::tests::{Plan, RGB, block, buffer};

/// A buffer holds a 64 by 64 image of three bytes a pixel.
const BYTES: u64 = 3 * 64 * 64;

/// A stream of buffers behind handles, of the given lengths.
fn backed(lengths: &[u64]) -> MockStream {
    let buffers: Vec<OwnedFd> = lengths.iter().map(|len| buffer(*len)).collect();
    MockStream::with_handles(buffers)
}

fn image(buffer: u32, width: u32, height: u32) -> ImageRef {
    ImageRef {
        buffer,
        width,
        height,
        fourcc: RGB.fourcc,
        stride: width * 3,
    }
}

/// A 64 by 64 to 32 by 32 resize from `input` to `output`.
fn job(identity: u64, input: u32, output: u32) -> ScaleJob {
    ScaleJob {
        identity,
        input: image(input, 64, 64),
        output: image(output, 32, 32),
        filter: Filter::Bilinear,
    }
}

fn post(consumer: &MockStream, job: &ScaleJob) {
    let mut bytes = [0u8; JOB_BYTES];
    let mut writer = Writer::new(&mut bytes);
    job.encode(&mut writer).expect("encode");
    let used = writer.len();
    consumer.post_job(&bytes[..used]);
}

const FAILED: Counters = Counters { jobs: 0, errors: 1 };

/// Serves one job on a fresh block, and returns its completion and how it went.
fn one(
    stream: &mut MockStream,
    job: &ScaleJob,
) -> (Option<StreamFrame>, Counters, Option<Failure>, u32) {
    let plan = Rc::new(Plan::default());
    let mut scaler = Scaler::new(block(&plan));
    let consumer = stream.clone();
    post(&consumer, job);
    assert_eq!(scaler.serve(stream, 5), Ok(1));
    assert!(plan.live.borrow().is_empty(), "every import removed");
    (
        consumer.take_frame(),
        scaler.counters(),
        scaler.last_failure(),
        plan.runs.get(),
    )
}

/// What a failed job leaves: an empty completion on `output` carrying its identity.
fn failed_on(frame: Option<StreamFrame>, output: u32, identity: u64) {
    let frame = frame.expect("a completion");
    assert_eq!(
        (frame.buffer, frame.identity, frame.bytes),
        (output, identity, 0)
    );
}

#[test]
fn a_job_runs_on_the_buffers_it_names_and_completes_on_its_output() {
    let mut stream = backed(&[BYTES; 4]);
    let consumer = stream.clone();
    let plan = Rc::new(Plan::default());
    let mut scaler = Scaler::new(block(&plan));
    post(&consumer, &job(0xABCD, 1, 2));
    assert_eq!(scaler.serve(&mut stream, 77), Ok(1));
    assert_eq!(plan.runs.get(), 1);
    let ran = plan.last.get().expect("a job");
    assert_eq!(ran.input().bytes(), BYTES);
    assert_eq!(ran.output().bytes(), 32 * 32 * 3);
    assert_eq!(ran.filter(), Filter::Bilinear);
    assert!(plan.live.borrow().is_empty());
    let frame = consumer.take_frame().expect("a completion");
    assert_eq!(
        (
            frame.buffer,
            frame.identity,
            frame.bytes,
            frame.timestamp_ns
        ),
        (2, 0xABCD, 32 * 32 * 3, 77)
    );
    assert_eq!(scaler.counters(), Counters { jobs: 1, errors: 0 });
    assert!(scaler.holds(1) && !scaler.holds(2));
    consumer.return_buffer(2);
    assert_eq!(scaler.serve(&mut stream, 78), Ok(0));
    assert!(scaler.holds(2));
}

#[test]
fn an_image_larger_than_the_dma_buf_behind_its_buffer_never_reaches_the_block() {
    // The last buffer's dma-buf is one page.
    let tall_in = ScaleJob {
        input: image(2, 32, 43), // 4128 bytes: one row more than the page
        ..job(1, 0, 1)
    };
    let tall_out = ScaleJob {
        output: image(2, 32, 43),
        ..job(2, 0, 1)
    };
    let mut fits = job(3, 0, 2);
    fits.output = image(2, 32, 42); // 4032 bytes
    let cases = [(tall_in, 1), (tall_out, 2)];
    for (bad, output) in cases {
        let mut stream = backed(&[BYTES, BYTES, 4096]);
        let (frame, counters, failure, runs) = one(&mut stream, &bad);
        assert_eq!((counters, failure, runs), (FAILED, Some(Failure::Frame), 0));
        failed_on(frame, output, bad.identity);
    }
    let mut stream = backed(&[BYTES, BYTES, 4096]);
    let (frame, counters, _, runs) = one(&mut stream, &fits);
    assert_eq!((counters, runs), (Counters { jobs: 1, errors: 0 }, 1));
    assert_eq!(frame.expect("a completion").bytes, 4032);
}

#[test]
fn a_job_can_only_name_buffers_the_driver_holds() {
    let refused = |job: &ScaleJob, completes: bool| {
        let mut stream = backed(&[BYTES; 3]);
        let (frame, counters, failure, runs) = one(&mut stream, job);
        assert_eq!(
            (counters, failure, runs),
            (FAILED, Some(Failure::Buffers), 0),
            "{job:?}"
        );
        assert_eq!(frame.is_some(), completes, "{job:?}");
    };
    refused(&job(1, 1, 1), false);
    refused(&job(1, 3, 0), true);
    refused(&job(1, 0, 3), false);
    refused(&job(1, u32::MAX, 0), true);
    refused(&job(1, 0, MAX_BUFFERS), false);
}

#[test]
fn a_job_the_block_cant_do_fails_before_anything_is_imported() {
    let check = |bad: &ScaleJob, why: Failure| {
        let mut stream = backed(&[BYTES; 2]);
        let (frame, counters, failure, runs) = one(&mut stream, bad);
        assert_eq!((counters, failure, runs), (FAILED, Some(why), 0), "{bad:?}");
        failed_on(frame, 1, bad.identity);
    };
    let mut unknown = job(1, 0, 1);
    unknown.output.fourcc = u32::from_le_bytes(*b"NV12");
    check(&unknown, Failure::Format);
    let mut area = job(2, 0, 1);
    area.filter = Filter::Area;
    check(&area, Failure::Format);
    let mut narrow = job(3, 0, 1);
    narrow.input.stride = 64 * 3 - 1;
    check(&narrow, Failure::Stride);
    let mut empty = job(4, 0, 1);
    empty.output.width = 0;
    check(&empty, Failure::Size);
    let mut huge = job(5, 0, 1);
    huge.input = image(0, 1025, 1);
    check(&huge, Failure::Size);
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
    let mut stream = backed(&[BYTES; 3]);
    let consumer = stream.clone();
    let plan = Rc::new(Plan::default());
    let mut scaler = Scaler::new(block(&plan));
    assert_eq!(scaler.serve(&mut stream, 0), Ok(0));
    consumer.change_consumer();
    post(&consumer, &job(6, 0, 1));
    assert_eq!(scaler.serve(&mut stream, 0), Ok(1));
    assert_eq!(scaler.counters(), Counters { jobs: 1, errors: 0 });
    let frame = consumer.take_frame().expect("a completion");
    assert_eq!((frame.buffer, frame.identity), (1, 6));
    assert_eq!(
        consumer.take_frame(),
        None,
        "what was given up reached no one"
    );
}

#[test]
fn a_block_that_fails_completes_the_job_as_failed_and_is_tried_again() {
    let mut stream = backed(&[BYTES; 3]);
    let consumer = stream.clone();
    let plan = Rc::new(Plan::default());
    let mut scaler = Scaler::new(block(&plan));
    plan.run_errno.set(Some(110));
    post(&consumer, &job(1, 0, 1));
    assert_eq!(scaler.serve(&mut stream, 0), Ok(1));
    assert_eq!(scaler.last_failure(), Some(Failure::Engine));
    failed_on(consumer.take_frame(), 1, 1);
    assert!(plan.live.borrow().is_empty());
    plan.run_errno.set(None);
    post(&consumer, &job(2, 0, 2));
    assert_eq!(scaler.serve(&mut stream, 1), Ok(1));
    failed_on(consumer.take_frame(), 2, 2);
    assert_eq!(plan.opens.get(), 1);
    consumer.return_buffer(1);
    post(&consumer, &job(3, 0, 1));
    assert_eq!(scaler.serve(&mut stream, 250_000_000), Ok(1));
    assert_eq!(
        consumer.take_frame().expect("a completion").bytes,
        32 * 32 * 3
    );
    assert_eq!(scaler.counters(), Counters { jobs: 1, errors: 2 });
    assert_eq!(plan.opens.get(), 2);
}

#[test]
fn bytes_that_are_not_a_job_fault_the_consumer_and_the_next_one_is_served() {
    let mut stream = backed(&[BYTES; 2]);
    let consumer = stream.clone();
    let plan = Rc::new(Plan::default());
    let mut scaler = Scaler::new(block(&plan));
    consumer.post_job(&[1, 2, 3]);
    post(&consumer, &job(4, 0, 1));
    // The consumer is faulted at the bytes that are no job, and its job behind them runs nowhere.
    assert_eq!(scaler.serve(&mut stream, 0), Ok(1));
    assert_eq!(consumer.faults(), 1);
    assert_eq!(scaler.counters(), Counters { jobs: 0, errors: 1 });
    assert_eq!(scaler.last_failure(), Some(Failure::Undecodable));
    assert_eq!(consumer.take_frame(), None);
    // What was held under it is given up, and the next consumer's job runs on buffers taken anew.
    assert!(!scaler.holds(0) && !scaler.holds(1));
    post(&consumer, &job(5, 0, 1));
    assert_eq!(scaler.serve(&mut stream, 0), Ok(1));
    assert_eq!(consumer.take_frame().expect("a completion").identity, 5);
    // A job longer than any the contract has is no job either.
    consumer.return_buffer(1);
    consumer.post_job(&[0; JOB_BYTES + 1]);
    assert_eq!(scaler.serve(&mut stream, 0), Ok(1));
    assert_eq!(consumer.faults(), 2);
    assert_eq!(scaler.counters(), Counters { jobs: 1, errors: 2 });
    // One the stream itself can't hand over is the stream's error.
    consumer.post_job(&[0; LONGEST_JOB + 1]);
    assert_eq!(scaler.serve(&mut stream, 0), Err(ErrorKind::InvalidInput));
}

#[test]
fn serving_jobs_allocates_nothing_in_the_driver() {
    let mut stream = backed(&[BYTES; 4]);
    let consumer = stream.clone();
    let plan = Rc::new(Plan::default());
    let mut scaler = Scaler::new(block(&plan));
    plan.live.borrow_mut().reserve(8);
    // The played-back stream copies each posted job into a new vector, so posting is left out of
    // the count: only `serve` is counted.
    let mut counted = 0;
    for n in 0..1010 {
        let output = 1 + (n % 3) as u32;
        post(&consumer, &job(n, 0, output));
        let (served, count) = allocations(|| scaler.serve(&mut stream, n));
        assert_eq!(served, Ok(1));
        if n >= 10 {
            counted += count;
        }
        let frame = consumer.take_frame().expect("a completion");
        assert_eq!((frame.identity, frame.bytes), (n, 32 * 32 * 3));
        consumer.return_buffer(output);
    }
    assert_eq!(counted, 0, "allocations serving 1000 jobs");
    assert_eq!(plan.runs.get(), 1010);
}
