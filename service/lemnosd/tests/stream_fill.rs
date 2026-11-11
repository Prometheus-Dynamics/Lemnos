//! A consumer fills buffers for a driver, through the real service and the real socket: a
//! configuration on a stream of shared memory and an image on a stream of dma-bufs, each
//! written through the client's own mapping, and read by the driver with `StreamEndpoint::read`.
//!
//! Sealed memfds stand in for the dma-bufs, so nothing here opens a device node. With
//! `LEMNOS_TEST_DMA_HEAP` set to a heap (`/dev/dma_heap/system`), one test runs the same on real
//! dma-bufs from it, where each access by the client is bracketed for the kernel; without, that
//! test says it was skipped.

#![allow(clippy::print_stderr)]

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use common::filled::{App, BUFFERS, DEVICE, Filled, IMAGE, Job, KEY, made, start};
use common::{Running, StandIn};
use lemnos_client::{Client, Error};
use lemnos_linux_stream::LinuxStreams;
use lemnosd::DmaHeaps;

fn service() -> Running {
    start(Arc::new(StandIn::default()), "")
}

fn image(seed: u8) -> Vec<u8> {
    (0..IMAGE)
        .map(|at| (at as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

fn key(seed: u8) -> [u8; KEY] {
    std::array::from_fn(|at| seed.wrapping_add(at as u8 * 7))
}

fn job(identity: u64, config: u32, input: u32, output: u32) -> Job {
    Job {
        identity,
        config,
        input,
        output,
    }
}

/// Jobs on every buffer in turn, each with a configuration and an image of its own.
fn jobs_run_on_what_the_client_wrote(app: &mut App) {
    for round in 0..3 * BUFFERS {
        let buffer = round % BUFFERS;
        let (key, image) = (key(round as u8), image(round as u8 + 100));
        let expected = made(&image, &key);
        // The configuration buffer is not the image's number, so each is read from its own.
        let job = job(u64::from(round), (buffer + 1) % BUFFERS, buffer, buffer);
        app.run(job, &key, &image, &expected);
    }
}

#[test]
fn a_driver_reads_the_configuration_and_the_image_its_client_wrote() {
    let running = service();
    let mut app = App::attach(&running);
    assert!(app.config.is_writable() && !app.config.has_buffer_handles());
    assert!(app.input.is_writable() && app.input.has_buffer_handles());
    assert!(!app.output.is_writable());
    jobs_run_on_what_the_client_wrote(&mut app);

    // The driver read every buffer: none of the jobs came back empty.
    let client = Client::connect(&running.socket, "counts").unwrap();
    let mut counts = client.open::<dyn Filled>("f").unwrap();
    assert_eq!(counts.done().unwrap(), 3 * BUFFERS);
    assert_eq!(counts.unread().unwrap(), 0);
}

#[test]
fn a_buffer_written_again_is_what_the_next_job_runs_on() {
    let running = service();
    let mut app = App::attach(&running);
    let (first, second) = (image(1), image(2));
    app.run(job(1, 0, 0, 0), &key(1), &first, &made(&first, &key(1)));
    // The same buffers, other bytes: part of the image, and the whole key.
    app.input.write(0, 1000, &second[1000..3000]).unwrap();
    let mut mixed = first.clone();
    mixed[1000..3000].copy_from_slice(&second[1000..3000]);
    app.run_posted(job(2, 0, 0, 1), &made(&mixed, &key(1)));
    app.config.write(0, 0, &key(9)).unwrap();
    app.run_posted(job(3, 0, 0, 2), &made(&mixed, &key(9)));
}

#[test]
fn writes_the_stream_has_no_room_for_are_refused_and_the_stream_carries_on() {
    let running = service();
    let mut app = App::attach(&running);
    let image = image(5);
    app.run(job(1, 0, 0, 0), &key(5), &image, &made(&image, &key(5)));
    // Past the buffer's own bytes (the dma-buf behind it is longer: a heap rounds up), past
    // the last buffer, and the stream the driver fills.
    assert!(matches!(
        app.input.write(0, IMAGE - 1, &[0xEE; 2]),
        Err(Error::Stream(_))
    ));
    assert!(matches!(
        app.input.write(BUFFERS, 0, &[0xEE]),
        Err(Error::Stream(_))
    ));
    assert!(matches!(
        app.config.write(0, KEY, &[0xEE]),
        Err(Error::Stream(_))
    ));
    assert_eq!(app.output.write(0, 0, &[0xEE]), Err(Error::StreamReadOnly));
    // Nothing of them landed, and nothing is broken: the same buffers give the same result.
    app.run_posted(job(2, 0, 0, 1), &made(&image, &key(5)));
    assert!(app.session.is_connected());
    assert!(app.input.is_open() && app.config.is_open() && app.output.is_open());
}

#[test]
fn a_consumer_that_takes_the_stream_read_only_reads_it_as_before_and_cannot_fill_it() {
    let running = service();
    let mut session = lemnos_client::Session::connect(&running.socket, "reader").unwrap();
    let mut input = session
        .open_stream(DEVICE, "input", Arc::new(LinuxStreams))
        .unwrap();
    assert_eq!(input.write(0, 0, &[1]), Err(Error::StreamReadOnly));
    let mut out = [0xFFu8; 4];
    input.read(0, 0, &mut out).unwrap();
    assert_eq!(out, [0; 4], "fresh memory, untouched");
    assert_eq!(input.next_frame(Duration::from_millis(5)).unwrap(), None);
}

#[test]
fn the_same_on_dma_bufs_from_a_real_heap_when_one_is_named() {
    let Some(path) = std::env::var_os("LEMNOS_TEST_DMA_HEAP") else {
        eprintln!("skipped: LEMNOS_TEST_DMA_HEAP names no heap");
        return;
    };
    let path = Path::new(&path);
    let (dir, heap) = (path.parent().unwrap(), path.file_name().unwrap());
    let heap = format!("heap = \"{}\"\n", heap.to_str().unwrap());
    let running = start(Arc::new(DmaHeaps::at(dir)), &heap);
    let mut app = App::attach(&running);
    jobs_run_on_what_the_client_wrote(&mut app);
}
