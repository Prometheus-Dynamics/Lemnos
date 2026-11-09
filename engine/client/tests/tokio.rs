#![cfg(feature = "tokio")]

mod common;

use std::sync::atomic::Ordering;
use std::time::Duration;

use common::FakeHost;
use lemnos_client::{AsyncClient, Error};

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
