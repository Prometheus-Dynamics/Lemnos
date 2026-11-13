//! Every descriptor a stream passes around is closed, on every path. One test, alone in its
//! binary, because it counts this process's descriptors.

mod common;

use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{StandIn, start_with_buffers};
use lemnos_client::{Session, StreamPlatform};
use lemnos_linux_stream::LinuxStreams;
use lemnos_wire::{Frame, VERSION, append};

fn open_descriptors() -> usize {
    std::fs::read_dir("/proc/self/fd").unwrap().count()
}

fn settle_to(expected: usize) -> usize {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let now = open_descriptors();
        if now <= expected || Instant::now() >= deadline {
            return now;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn no_path_leaks_a_descriptor() {
    let running = start_with_buffers(
        "[[devices]]\nid = \"s\"\ndriver = \"fake-counter\"\n\
         [devices.streams.frames]\nbuffers = 4\nbuffer_bytes = 4096\n\
         [devices.streams.handles]\nbuffers = 16\nbuffer_bytes = 4096\nmemory = \"dma-buf\"\n",
        Arc::new(StandIn::default()),
    );
    let platform: Arc<dyn StreamPlatform> = Arc::new(LinuxStreams);

    // Warm up, so anything created once is counted.
    let mut session = Session::connect(&running.socket, "warm").unwrap();
    let stream = session
        .open_stream(0, "frames", Arc::clone(&platform))
        .unwrap();
    session.close_stream(stream).unwrap();
    drop(session);
    std::thread::sleep(Duration::from_millis(50));
    let baseline = open_descriptors();

    // Open and closed; open and dropped with the connection; and refused.
    for _ in 0..25 {
        let mut session = Session::connect(&running.socket, "cycle").unwrap();
        let stream = session
            .open_stream(0, "frames", Arc::clone(&platform))
            .unwrap();
        session.close_stream(stream).unwrap();
        let held = session
            .open_stream(0, "frames", Arc::clone(&platform))
            .unwrap();
        assert!(
            session
                .open_stream(99, "frames", Arc::clone(&platform))
                .is_err()
        );
        drop(held);
        drop(session);
    }
    assert_eq!(settle_to(baseline), baseline, "open and close");

    // The answer never read: the descriptors die in the service's queue, or in the socket.
    for _ in 0..25 {
        let mut raw = UnixStream::connect(&running.socket).unwrap();
        let mut out = Vec::new();
        for frame in [
            Frame::Hello {
                layout: lemnos_wire::LAYOUT_HASH,
                lemnos: lemnos_wire::LemnosName(lemnos_wire::LEMNOS),
                version: VERSION,
                name: "x",
                events: false,
            },
            Frame::StreamOpen {
                id: 1,
                device: 0,
                name: "frames",
            },
        ] {
            append(&frame, &mut out).unwrap();
        }
        std::io::Write::write_all(&mut raw, &out).unwrap();
        // Let the service answer so the descriptors are in flight when this goes.
        let deadline = Instant::now() + Duration::from_secs(5);
        let host = running.service.as_ref().unwrap().host();
        while !host.stream_stats(0, "frames").unwrap().attached && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        drop(raw);
        while host.stream_stats(0, "frames").unwrap().attached && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    assert_eq!(settle_to(baseline), baseline, "answer never read");

    // The same for a stream with a descriptor per buffer: eighteen cross each time, and the
    // service holds sixteen of them while the stream is attached.
    for _ in 0..25 {
        let mut session = Session::connect(&running.socket, "cycle").unwrap();
        let stream = session
            .open_stream(0, "handles", Arc::clone(&platform))
            .unwrap();
        assert!(stream.has_buffer_handles());
        session.close_stream(stream).unwrap();
        let held = session
            .open_stream(0, "handles", Arc::clone(&platform))
            .unwrap();
        drop(held);
        drop(session);
        let host = running.service.as_ref().unwrap().host();
        let deadline = Instant::now() + Duration::from_secs(5);
        while host.stream_stats(0, "handles").unwrap().attached && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    assert_eq!(settle_to(baseline), baseline, "a descriptor per buffer");

    for _ in 0..25 {
        let mut raw = UnixStream::connect(&running.socket).unwrap();
        let mut out = Vec::new();
        for frame in [
            Frame::Hello {
                layout: lemnos_wire::LAYOUT_HASH,
                lemnos: lemnos_wire::LemnosName(lemnos_wire::LEMNOS),
                version: VERSION,
                name: "x",
                events: false,
            },
            Frame::StreamOpenHandles {
                id: 1,
                device: 0,
                name: "handles",
            },
        ] {
            append(&frame, &mut out).unwrap();
        }
        std::io::Write::write_all(&mut raw, &out).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let host = running.service.as_ref().unwrap().host();
        while !host.stream_stats(0, "handles").unwrap().attached && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        drop(raw);
        while host.stream_stats(0, "handles").unwrap().attached && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    assert_eq!(
        settle_to(baseline),
        baseline,
        "a descriptor per buffer, answer never read"
    );
}
