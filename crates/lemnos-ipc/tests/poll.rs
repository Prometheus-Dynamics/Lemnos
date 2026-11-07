//! Polled use of a `DeviceClient` against a fake service: requests sent without
//! waiting, answers and events drained, and nothing allocated in the steady state.

use lemnos_ipc::wire::{Event, Message, RawReading, Request, decode_request};
use lemnos_ipc::{ClientError, ClientOptions, DeviceStatus, FEATURE_READ_ID, Polled};
use lemnos_linux_sys::test_alloc::{Counting, allocations};
use std::io::{Read, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// A fake service for `connections` connections, one after another: answers reads with a
/// reading of the read's id, sets and gets with a reply (a set also with a control
/// event), and closes the connection when asked to read `"close"`.
fn service(name: &str) -> (PathBuf, JoinHandle<()>) {
    services(name, 1)
}

fn services(name: &str, connections: usize) -> (PathBuf, JoinHandle<()>) {
    let dir = std::env::temp_dir().join(format!("lemnos-ipc-poll-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sock");
    let listener = UnixListener::bind(&path).unwrap();
    let handle = std::thread::spawn(move || {
        for _ in 0..connections {
            serve(listener.accept().unwrap().0);
        }
    });
    (path, handle)
}

fn serve(mut stream: std::os::unix::net::UnixStream) {
    {
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            while let Ok(Some((request, used))) = decode_request(&buf) {
                buf.drain(..used);
                let answers = match request {
                    Request::Hello { .. } => vec![Message::Welcome {
                        version: lemnos_ipc::wire::VERSION,
                        board: "fake".into(),
                        client_id: 1,
                        features: FEATURE_READ_ID,
                    }],
                    Request::List => vec![Message::Devices(Vec::new())],
                    Request::Read { device, .. } if device == "close" => return,
                    Request::Read { id, device } => vec![Message::Reading(RawReading {
                        device,
                        timestamp_us: u64::from(id),
                        status: DeviceStatus::Available,
                        values: vec![id as i32, 2, 3],
                        id,
                    })],
                    Request::Set {
                        id,
                        device,
                        control,
                        value,
                    } => vec![
                        Message::Reply {
                            id,
                            result: Ok(value),
                        },
                        Message::Event(Event::Control {
                            device,
                            control,
                            value,
                            by: "poll".into(),
                        }),
                    ],
                    Request::Get { id, .. } => vec![Message::Reply {
                        id,
                        result: Ok(1.5),
                    }],
                    _ => Vec::new(),
                };
                for answer in answers {
                    stream.write_all(&answer.encode()).unwrap();
                }
            }
            match stream.read(&mut chunk) {
                Ok(0) | Err(_) => return,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
        }
    }
}

/// The next thing `poll` finds, waiting up to 5 s.
fn next(client: &mut lemnos_ipc::DeviceClient, reading: &mut RawReading) -> Polled {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match client.poll(reading) {
            Ok(Some(polled)) => return polled,
            Ok(None) => {
                assert!(Instant::now() < deadline, "nothing arrived");
                std::thread::yield_now();
            }
            Err(e) => panic!("poll failed: {e}"),
        }
    }
}

#[test]
fn requests_are_answered_through_poll() {
    let (path, service) = service("answers");
    let mut client = ClientOptions::new(&path, "poll").devices().unwrap();
    let mut reading = RawReading::default();
    assert!(matches!(
        next(&mut client, &mut reading),
        Polled::Connected { reconnects: 0 }
    ));
    assert!(
        client.poll(&mut reading).unwrap().is_none(),
        "nothing was asked"
    );
    assert!(client.fd().is_some());

    let id = client.send_read("imu").unwrap();
    assert!(matches!(next(&mut client, &mut reading), Polled::Reading));
    assert_eq!((reading.id, reading.device.as_str()), (id, "imu"));
    assert_eq!(reading.values, [id as i32, 2, 3]);

    let set = client.send_set("fan", "duty", 0.25).unwrap();
    let get = client.send_get("fan", "duty").unwrap();
    let mut got = Vec::new();
    for _ in 0..3 {
        got.push(next(&mut client, &mut reading));
    }
    assert!(matches!(got[0], Polled::Reply { id, result: Ok(v) } if id == set && v == 0.25));
    assert!(matches!(&got[1], Polled::Event(Event::Control { control, .. }) if control == "duty"));
    assert!(matches!(got[2], Polled::Reply { id, result: Ok(v) } if id == get && v == 1.5));

    // A blocking call in between still works, and polling resumes after it.
    assert_eq!(client.read("imu").unwrap().device, "imu");
    let id = client.send_read("imu").unwrap();
    assert!(matches!(next(&mut client, &mut reading), Polled::Reading));
    assert_eq!(reading.id, id);

    client.send_read("close").unwrap();
    assert!(matches!(
        next(&mut client, &mut reading),
        Polled::Disconnected { .. }
    ));
    assert!(matches!(
        client.poll(&mut reading),
        Err(ClientError::Closed)
    ));
    assert!(client.fd().is_none());
    service.join().unwrap();
}

#[test]
fn a_reconnection_is_polled_with_its_new_socket() {
    let (path, service) = services("reconnect", 2);
    let mut client = ClientOptions::new(&path, "poll")
        .reconnecting()
        .devices()
        .unwrap();
    let mut reading = RawReading::default();
    assert!(matches!(
        next(&mut client, &mut reading),
        Polled::Connected { reconnects: 0 }
    ));
    client.send_read("close").unwrap();
    assert!(matches!(
        next(&mut client, &mut reading),
        Polled::Disconnected { .. }
    ));
    assert!(client.fd().is_none());
    assert!(client.retry_at().is_some());
    // Polling retries once the backoff has passed (`next` keeps polling).
    assert!(matches!(
        next(&mut client, &mut reading),
        Polled::Connected { reconnects: 1 }
    ));
    assert!(client.fd().is_some() && client.retry_at().is_none());
    let id = client.send_read("imu").unwrap();
    assert!(matches!(next(&mut client, &mut reading), Polled::Reading));
    assert_eq!(reading.id, id);
    drop(client);
    service.join().unwrap();
}

#[test]
fn reads_in_the_steady_state_do_not_allocate() {
    let (path, service) = service("alloc");
    let mut client = ClientOptions::new(&path, "poll").devices().unwrap();
    let mut reading = RawReading::default();
    let round = |client: &mut lemnos_ipc::DeviceClient, reading: &mut RawReading| {
        let id = client.send_read("imu").unwrap();
        assert!(matches!(next(client, reading), Polled::Reading));
        assert_eq!(reading.id, id);
    };
    assert!(matches!(
        next(&mut client, &mut reading),
        Polled::Connected { .. }
    ));
    // Warm up: the buffers grow to their size once.
    for _ in 0..50 {
        round(&mut client, &mut reading);
    }
    let ((), allocated) = allocations(|| {
        for _ in 0..1000 {
            round(&mut client, &mut reading);
        }
    });
    assert_eq!(allocated, 0, "allocations over 1000 polled reads");
    client.send_read("close").unwrap();
    service.join().unwrap();
}

#[cfg(feature = "tokio")]
#[tokio::test(flavor = "current_thread")]
async fn the_tokio_adapter_waits_on_the_socket() {
    let (path, service) = service("tokio");
    let client = ClientOptions::new(&path, "poll").devices().unwrap();
    let mut client = lemnos_ipc::AsyncDeviceClient::new(client).unwrap();
    let mut reading = RawReading::default();
    let connected = client.next(&mut reading).await.unwrap();
    assert!(matches!(connected, Polled::Connected { reconnects: 0 }));
    for _ in 0..20 {
        let id = client.get_mut().send_read("imu").unwrap();
        let polled = tokio::time::timeout(Duration::from_secs(5), client.next(&mut reading))
            .await
            .expect("nothing arrived")
            .unwrap();
        assert!(matches!(polled, Polled::Reading));
        assert_eq!(reading.id, id);
    }
    let set = client.get_mut().send_set("fan", "duty", 0.5).unwrap();
    let polled = client.next(&mut reading).await.unwrap();
    assert!(matches!(polled, Polled::Reply { id, result: Ok(v) } if id == set && v == 0.5));
    assert!(matches!(
        client.next(&mut reading).await.unwrap(),
        Polled::Event(_)
    ));
    client.get_mut().send_read("close").unwrap();
    assert!(matches!(
        client.next(&mut reading).await.unwrap(),
        Polled::Disconnected { .. }
    ));
    assert!(matches!(
        client.next(&mut reading).await,
        Err(ClientError::Closed)
    ));
    service.join().unwrap();
}
