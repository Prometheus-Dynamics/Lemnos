#![cfg(feature = "tokio")]

mod common;

use std::sync::atomic::Ordering;
use std::time::Duration;

use common::{FakeHost, StreamScript};
use lemnos_client::{AsyncClient, Error, StreamPlatform};
use lemnos_core::Status;
use lemnos_linux_stream::LinuxStreams;
use lemnos_linux_sys::dma_buf::sealed_memfd;
use lemnos_linux_sys::eventfd;
use lemnos_linux_sys::mmap::Mapping;
use lemnos_wire::ring::{FrameSlot, Layout, Producer, WordMemory};
use std::os::fd::AsFd;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

struct Ring(Mapping);

impl WordMemory for Ring {
    fn words(&self) -> &[AtomicU64] {
        self.0.as_atomic_words().unwrap()
    }
}

fn block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

#[test]
fn the_async_form_calls_lists_and_receives() {
    let host = FakeHost::start();
    block_on(async {
        let mut client = AsyncClient::connect(&host.path, "t").await.unwrap();
        assert_eq!(client.list().await.unwrap().len(), 2);
        let handle = client.open("", "example.counter/1").await.unwrap();
        let mut reply = [0; 8];
        let n = client.call(&handle, 1, &[4, 5], &mut reply).await.unwrap();
        assert_eq!(&reply[..n], [4, 5]);
        let sub = client.subscribe(&handle, 1, 5000, false).await.unwrap();
        let mut into = [0; 8];
        let value = client
            .next_value(&mut into, Duration::from_secs(2))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(value.subscription, sub.id);
        assert_eq!(
            client.describe(10).await.unwrap().contracts[0].methods[0].name,
            "m"
        );
    });
}

#[test]
fn the_async_form_times_out_without_a_value() {
    let host = FakeHost::start();
    block_on(async {
        let mut client = AsyncClient::connect(&host.path, "t").await.unwrap();
        let mut into = [0; 8];
        let value = client
            .next_value(&mut into, Duration::from_millis(30))
            .await
            .unwrap();
        assert_eq!(value, None);
    });
}

#[test]
fn the_async_form_comes_back_after_a_restart() {
    let host = FakeHost::start();
    block_on(async {
        let mut client = AsyncClient::connect(&host.path, "t").await.unwrap();
        client.open("", "example.counter/1").await.unwrap();
        host.restart();
        let mut opened = false;
        for _ in 0..200 {
            if host.control.connections.load(Ordering::SeqCst) >= 2
                && client.open("", "a/1").await.is_ok()
            {
                opened = true;
                break;
            }
            let mut into = [0; 8];
            match client
                .next_value(&mut into, Duration::from_millis(50))
                .await
            {
                Ok(_) | Err(Error::Disconnected) => {}
                Err(other) => panic!("{other}"),
            }
        }
        assert!(opened);
    });
}

#[test]
fn the_async_form_takes_a_stream_and_frames_arrive() {
    let layout = Layout::new(2, 64, 1, 8).unwrap();
    let ring_fd = sealed_memfd(c"ring", layout.ring_bytes() as u64).unwrap();
    let pool_fd = sealed_memfd(c"pool", layout.pool_bytes()).unwrap();
    let ring = Mapping::new(ring_fd.as_fd(), layout.ring_bytes(), 0, true).unwrap();
    let mut producer = Producer::new(Arc::new(Ring(ring)), layout).unwrap();
    let host = FakeHost::start();
    *host.control.stream.lock().unwrap() = Some(StreamScript {
        layout,
        claimed: 3,
        fds: vec![ring_fd, eventfd::create().unwrap(), pool_fd],
    });
    let platform: Arc<dyn StreamPlatform> = Arc::new(LinuxStreams);
    block_on(async {
        let mut client = AsyncClient::connect(&host.path, "t").await.unwrap();
        let mut stream = client
            .open_stream(1, "s", Arc::clone(&platform))
            .await
            .unwrap();
        producer
            .push_frame(FrameSlot {
                buffer: 0,
                bytes: 4,
                sequence: 9,
                identity: 77,
                timestamp_ns: 1,
                fence: 2,
            })
            .unwrap();
        let frame = stream.try_next().unwrap().unwrap();
        assert_eq!((frame.sequence(), frame.identity()), (9, 77));
        stream.release(frame).unwrap();

        let error = client.open_stream(1, "s", platform).await.unwrap_err();
        assert!(
            matches!(&error, Error::Refused { status, .. } if *status == Status::NOT_FOUND),
            "{error:?}"
        );
    });
    assert_eq!(producer.pop_return().unwrap(), Some((0, 2)));
}
