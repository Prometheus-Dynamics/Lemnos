mod common;

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use common::{FakeHost, temp_dir};
use lemnos_client::{Client, DeviceHealth, DeviceState, Error, Incoming, Session};
use lemnos_core::{Result, Status, contract};

#[contract("example.counter", version = 1)]
pub trait Echo {
    #[method(1)]
    fn add(&mut self, value: u32) -> Result<u32>;
}

const TAG: &str = "example.counter/1";

#[test]
fn it_says_who_it_is() {
    let host = FakeHost::start();
    let client = Client::connect(&host.path, "the-tool").unwrap();
    client.list().unwrap();
    assert_eq!(*host.control.names.lock().unwrap(), ["the-tool"]);
}

#[test]
fn listing_reports_every_device_as_the_service_does() {
    let host = FakeHost::start();
    let devices = Client::connect(&host.path, "t").unwrap().list().unwrap();
    assert_eq!(devices.len(), 2);
    assert_eq!(devices[0].name, "one");
    assert_eq!(devices[0].tags, [TAG, "other/1"]);
    assert_eq!(devices[0].state, DeviceState::Running);
    assert_eq!(devices[1].state, DeviceState::BackingOff);
    assert_eq!(devices[1].health, DeviceHealth::Failed);
    assert_eq!(devices[1].reason, "bad");
}

#[test]
fn a_device_is_found_by_tag() {
    let host = FakeHost::start();
    let client = Client::connect(&host.path, "t").unwrap();
    assert_eq!(client.find("x/1").unwrap().unwrap().name, "two");
    assert_eq!(client.find("nothing/1").unwrap(), None);
}

#[test]
fn no_service_is_an_error_from_connect() {
    let dir = temp_dir("none");
    assert_eq!(
        Client::connect(dir.0.join("sock"), "t").unwrap_err(),
        Error::Disconnected
    );
}

#[test]
fn another_protocol_version_is_refused() {
    let host = FakeHost::start();
    host.control.version.store(99, Ordering::SeqCst);
    assert_eq!(
        Client::connect(&host.path, "t").unwrap_err(),
        Error::Version {
            ours: lemnos_wire::VERSION,
            theirs: 99
        }
    );
}

#[test]
fn a_typed_contract_works_over_the_socket() {
    let host = FakeHost::start();
    let client = Client::connect(&host.path, "t").unwrap();
    let mut echo = client.open::<dyn Echo>("").unwrap();
    // The stand-in echoes its arguments, and the reply decodes the same way.
    assert_eq!(echo.add(41).unwrap(), 41);
}

#[test]
fn a_refused_open_says_why() {
    let host = FakeHost::start();
    let client = Client::connect(&host.path, "t").unwrap();
    assert_eq!(
        client.open_tag("", "missing/1").unwrap_err(),
        Error::Refused {
            status: Status::NOT_FOUND,
            text: "no such device".to_string()
        }
    );
}

#[test]
fn a_description_arrives_whole() {
    let host = FakeHost::start();
    let description = Client::connect(&host.path, "t")
        .unwrap()
        .describe(10)
        .unwrap();
    assert_eq!(description.contracts[0].methods[0].name, "m");
}

#[test]
fn a_reply_too_big_for_the_buffer_is_an_error() {
    let host = FakeHost::start();
    let client = Client::connect(&host.path, "t").unwrap();
    let mut handle = client.open_tag("", TAG).unwrap();
    let mut reply = [0; 1];
    assert_eq!(
        client.call(&mut handle, 1, &[1, 2], &mut reply),
        Err(Error::TooSmall)
    );
}

#[test]
fn power_requests_are_answered() {
    let host = FakeHost::start();
    let client = Client::connect(&host.path, "t").unwrap();
    client.set_power(10, false).unwrap();
    assert_eq!(host.control.power_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn values_arrive_with_stable_identity() {
    let host = FakeHost::start();
    let client = Client::connect(&host.path, "t").unwrap();
    let mut handle = client.open_tag("", TAG).unwrap();
    let sub = client.subscribe(&mut handle, 1, 5000, false).unwrap();
    assert_eq!(sub.period_us, 5000);
    let mut into = [0; 8];
    let value = client
        .next_value(&mut into, Duration::from_secs(2))
        .unwrap()
        .unwrap();
    assert_eq!(value.subscription, sub.id);
    assert_eq!(value.len, 8);
}

#[test]
fn values_that_arrive_during_a_call_are_kept() {
    let host = FakeHost::start();
    let client = Client::connect(&host.path, "t").unwrap();
    let mut handle = client.open_tag("", TAG).unwrap();
    let sub = client.subscribe(&mut handle, 1, 5000, false).unwrap();
    host.control.values_before_reply.store(3, Ordering::SeqCst);
    let mut reply = [0; 4];
    client.call(&mut handle, 1, &[7], &mut reply).unwrap();
    let mut into = [0; 8];
    let value = client
        .next_value(&mut into, Duration::from_secs(1))
        .unwrap()
        .unwrap();
    assert_eq!(value.subscription, sub.id);
}

#[test]
fn a_refused_first_subscription_is_an_error_and_is_not_retried() {
    let host = FakeHost::start();
    host.control.refuse_subscribe.store(true, Ordering::SeqCst);
    let client = Client::connect(&host.path, "t").unwrap();
    let mut handle = client.open_tag("", TAG).unwrap();
    assert!(matches!(
        client.subscribe(&mut handle, 1, 5000, false),
        Err(Error::Refused { .. })
    ));
}

#[test]
fn unsubscribing_tells_the_service() {
    let host = FakeHost::start();
    let client = Client::connect(&host.path, "t").unwrap();
    let mut handle = client.open_tag("", TAG).unwrap();
    let sub = client.subscribe(&mut handle, 1, 5000, false).unwrap();
    client.unsubscribe(sub.id).unwrap();
    assert_eq!(host.control.unsubscribed.load(Ordering::SeqCst), 1);
}

fn eventually(mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(8);
    while !done() {
        assert!(Instant::now() < deadline, "timed out");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_handle_finds_its_device_again_after_a_restart() {
    let host = FakeHost::start();
    let client = Client::connect(&host.path, "t").unwrap();
    let mut echo = client.open::<dyn Echo>("").unwrap();
    assert_eq!(echo.add(1).unwrap(), 1);
    host.restart();
    // The call in flight may fail. It comes back, with a new device number.
    eventually(|| echo.add(2) == Ok(2));
    assert!(host.control.connections.load(Ordering::SeqCst) >= 2);
}

#[test]
fn a_subscription_comes_back_after_a_restart_with_the_same_identity() {
    let host = FakeHost::start();
    let client = Client::connect(&host.path, "t").unwrap();
    let mut handle = client.open_tag("", TAG).unwrap();
    let sub = client.subscribe(&mut handle, 1, 5000, false).unwrap();
    host.restart();
    let mut into = [0; 8];
    eventually(|| {
        let value = client
            .next_value(&mut into, Duration::from_millis(100))
            .unwrap();
        host.control.connections.load(Ordering::SeqCst) >= 2
            && value.is_some_and(|value| value.subscription == sub.id)
    });
}

#[test]
fn a_program_can_start_before_the_service() {
    let dir = temp_dir("late");
    let client = Client::connecting(dir.0.join("sock"), "t");
    assert_eq!(client.list().unwrap_err(), Error::Disconnected);
    assert!(!client.is_connected());
}

#[test]
fn polling_returns_at_once_when_nothing_has_arrived() {
    let host = FakeHost::start();
    let mut session = Session::connect_polled(&host.path, "t").unwrap();
    let started = Instant::now();
    let mut seen = 0;
    session.poll(&mut |_| seen += 1);
    assert!(started.elapsed() < Duration::from_millis(50));
    assert_eq!(seen, 0);
}

#[test]
fn a_polled_call_is_answered_by_id() {
    let host = FakeHost::start();
    let mut session = Session::connect_polled(&host.path, "t").unwrap();
    let handle = session.open("", TAG).unwrap();
    let id = session.send_call(&handle, 1, &[9], 8).unwrap();
    let mut answer = None;
    eventually(|| {
        session.poll(&mut |incoming| {
            if let Incoming::Reply { id: got, bytes } = incoming {
                answer = Some((got, bytes.to_vec()));
            }
        });
        answer.is_some()
    });
    assert_eq!(answer, Some((id, vec![9])));
}

#[test]
fn a_polled_call_on_an_old_handle_is_stale() {
    let host = FakeHost::start();
    let mut session = Session::connect_polled(&host.path, "t").unwrap();
    let handle = session.open("", TAG).unwrap();
    let generation = session.generation();
    host.restart();
    eventually(|| {
        session.poll(&mut |_| {});
        session.generation() != generation
    });
    assert_eq!(session.send_call(&handle, 1, &[], 8), Err(Error::Stale));
}
