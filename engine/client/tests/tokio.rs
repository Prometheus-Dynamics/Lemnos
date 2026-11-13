#![cfg(feature = "tokio")]

mod common;

use std::sync::atomic::Ordering;
use std::time::Duration;

use common::{FakeHost, StreamHost, StreamScript};
use lemnos_client::{AsyncClient, Error, StreamPlatform};
use lemnos_core::Status;
use lemnos_linux_stream::LinuxStreams;
use lemnos_wire::feature;
use lemnos_wire::ring::Layout;
use std::sync::Arc;

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
    let (mut producer, fds) = StreamHost::make(layout, &[], true);
    let host = FakeHost::start();
    host.control
        .features
        .store(feature::SHARED_RINGS, Ordering::SeqCst);
    *host.control.stream.lock().unwrap() = Some(StreamScript {
        layout,
        claimed: fds.len() as u8,
        fds,
    });
    let platform: Arc<dyn StreamPlatform> = Arc::new(LinuxStreams);
    block_on(async {
        let mut client = AsyncClient::connect(&host.path, "t").await.unwrap();
        let mut stream = client
            .open_stream(1, "s", Arc::clone(&platform))
            .await
            .unwrap();
        assert!(producer.push(0, 4, 9, 77, 1));
        let frame = stream.try_next().unwrap().unwrap();
        assert_eq!((frame.sequence(), frame.identity()), (9, 77));
        stream.release(frame).unwrap();

        let error = client.open_stream(1, "s", platform).await.unwrap_err();
        assert!(
            matches!(&error, Error::Refused { status, .. } if *status == Status::NOT_FOUND),
            "{error:?}"
        );
    });
    assert!(
        producer.reclaim(0),
        "the released frame's buffer comes back"
    );
}

#[test]
fn the_async_form_refuses_a_service_from_another_lemnos_revision_at_connect() {
    let host = FakeHost::start();
    host.control
        .welcome_layout
        .store(0xdead_beef_0bad_f00d, Ordering::SeqCst);
    block_on(async {
        let error = AsyncClient::connect(&host.path, "t").await.unwrap_err();
        assert!(
            matches!(
                error,
                Error::LayoutMismatch {
                    theirs: 0xdead_beef_0bad_f00d,
                    ..
                }
            ),
            "{error}"
        );
        assert_eq!(Status::from(error), Status::VERSION_MISMATCH);
    });
}
