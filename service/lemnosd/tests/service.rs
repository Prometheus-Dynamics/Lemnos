//! The real service, through the real socket and the real client.

mod common;

use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use common::{COUNTER, start, start_with};
use lemnos_client::{Client, DeviceState, Error};
use lemnos_core::Status;
use lemnos_fake_devices::Counter;
use lemnos_wire::{Frame, VERSION, append, decode};

const GET: u16 = 1;
const PEEK: u16 = 7;

fn eventually(mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(8);
    while !done() {
        assert!(Instant::now() < deadline, "timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// A client that sends whatever it is told to.
struct Raw {
    stream: UnixStream,
    buf: Vec<u8>,
}

impl Raw {
    fn connect(running: &common::Running) -> Self {
        let stream = UnixStream::connect(&running.socket).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        Self {
            stream,
            buf: Vec::new(),
        }
    }

    fn send(&mut self, frame: Frame<'_>) {
        let mut out = Vec::new();
        append(&frame, &mut out).unwrap();
        self.stream.write_all(&out).unwrap();
    }

    fn greet(&mut self) {
        self.send(Frame::Hello {
            layout: lemnos_wire::LAYOUT_HASH,
            lemnos: lemnos_wire::LemnosName(lemnos_wire::LEMNOS),
            version: VERSION,
            name: "raw",
            events: false,
        });
        self.with_next(|frame| assert!(matches!(frame, Some(Frame::Welcome { .. }))));
    }

    /// Hands the next frame to `check`, or `None` if the socket closed.
    fn with_next(&mut self, check: impl FnOnce(Option<Frame<'_>>)) {
        loop {
            if let Some((_, used)) = decode(&self.buf).unwrap() {
                check(decode(&self.buf).unwrap().map(|(frame, _)| frame));
                self.buf.drain(..used);
                return;
            }
            let mut chunk = [0; 4096];
            match self.stream.read(&mut chunk) {
                Ok(0) | Err(_) => {
                    check(None);
                    return;
                }
                Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
            }
        }
    }

    fn closes(&mut self) {
        self.with_next(|frame| assert_eq!(frame, None));
    }
}

#[test]
fn the_socket_is_group_only() {
    let running = start();
    let mode = std::fs::metadata(&running.socket)
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o660);
}

#[test]
fn devices_are_listed_read_and_set() {
    let running = start();
    let client = Client::connect(&running.socket, "t").unwrap();
    let devices = client.list().unwrap();
    assert_eq!(devices.len(), 2);
    assert_eq!(devices[0].name, "a");
    assert_eq!(devices[0].tags, [COUNTER]);
    assert_eq!(devices[0].state, DeviceState::Running);

    let mut counter = client.open::<dyn Counter>("b").unwrap();
    assert_eq!(counter.get().unwrap(), 101);
    counter.set(7).unwrap();
    assert_eq!(counter.peek().unwrap(), 7);
}

#[test]
fn a_device_is_opened_by_tag_alone() {
    let running = start();
    let client = Client::connect(&running.socket, "t").unwrap();
    let mut counter = client.open::<dyn Counter>("").unwrap();
    counter.get().unwrap();
}

#[test]
fn an_unknown_device_or_contract_is_refused() {
    let running = start();
    let client = Client::connect(&running.socket, "t").unwrap();
    for (name, tag) in [("nope", COUNTER), ("a", "other/1"), ("", "other/1")] {
        assert!(matches!(
            client.open_tag(name, tag),
            Err(Error::Refused {
                status: Status::NOT_FOUND,
                ..
            })
        ));
    }
}

#[test]
fn a_device_describes_itself() {
    let running = start();
    let client = Client::connect(&running.socket, "t").unwrap();
    let description = client.describe(0).unwrap();
    assert_eq!(description.contracts[0].tag, COUNTER);
    assert!(
        description.contracts[0]
            .methods
            .iter()
            .any(|m| m.name == "get")
    );
    assert!(description.items.iter().any(|i| i.name == "count"));
}

#[test]
fn describing_nothing_is_refused() {
    let running = start();
    let client = Client::connect(&running.socket, "t").unwrap();
    assert!(matches!(client.describe(9), Err(Error::Refused { .. })));
}

#[test]
fn a_panicking_driver_is_reported_and_the_service_carries_on() {
    let running = start();
    let client = Client::connect(&running.socket, "t").unwrap();
    let mut bad = client.open::<dyn Counter>("a").unwrap();
    let mut good = client.open::<dyn Counter>("b").unwrap();
    assert_eq!(bad.crash(), Err(Status::PANIC));
    assert_eq!(good.get().unwrap(), 101);
    assert_eq!(client.list().unwrap()[0].state, DeviceState::BackingOff);
    // It comes back by itself.
    eventually(|| client.list().unwrap()[0].state == DeviceState::Running);
    bad.get().unwrap();
}

#[test]
fn a_failing_call_says_why() {
    let running = start();
    let client = Client::connect(&running.socket, "t").unwrap();
    let mut counter = client.open::<dyn Counter>("a").unwrap();
    assert_eq!(counter.fail(), Err(Status::IO));
}

#[test]
fn switching_a_device_off_and_on_is_seen_by_everyone() {
    let running = start();
    let client = Client::connect(&running.socket, "t").unwrap();
    let watcher = Client::connect(&running.socket, "w").unwrap();
    client.set_power(0, false).unwrap();
    assert_eq!(client.list().unwrap()[0].state, DeviceState::Off);
    eventually(|| {
        // Nothing else drives this client's socket.
        let mut into = [0; 8];
        watcher
            .next_value(&mut into, Duration::from_millis(5))
            .unwrap();
        std::iter::from_fn(|| watcher.next_event()).any(|e| e.state == DeviceState::Off)
    });
    client.set_power(0, true).unwrap();
    assert_eq!(client.list().unwrap()[0].state, DeviceState::Running);
}

#[test]
fn switching_on_nothing_is_refused() {
    let running = start();
    let client = Client::connect(&running.socket, "t").unwrap();
    assert!(matches!(
        client.set_power(9, true),
        Err(Error::Refused { .. })
    ));
}

#[test]
fn values_arrive_at_about_the_period_asked_for() {
    let running = start();
    let client = Client::connect(&running.socket, "t").unwrap();
    let mut handle = client.open_tag("a", COUNTER).unwrap();
    let sub = client.subscribe(&mut handle, GET, 2000, false).unwrap();
    let mut into = [0; 8];
    let mut last = 0;
    for _ in 0..50 {
        let value = client
            .next_value(&mut into, Duration::from_secs(2))
            .unwrap()
            .unwrap();
        assert_eq!(value.subscription, sub.id);
        let now = u32::from_le_bytes(into[..4].try_into().unwrap());
        assert!(now > last);
        last = now;
    }
}

#[test]
fn only_changes_stays_quiet_until_something_changes() {
    let running = start();
    let client = Client::connect(&running.socket, "t").unwrap();
    let mut handle = client.open_tag("b", COUNTER).unwrap();
    client.subscribe(&mut handle, PEEK, 1000, true).unwrap();
    let mut into = [0; 8];
    assert!(
        client
            .next_value(&mut into, Duration::from_secs(2))
            .unwrap()
            .is_some()
    );
    assert!(
        client
            .next_value(&mut into, Duration::from_millis(40))
            .unwrap()
            .is_none()
    );
    let mut counter = client.open::<dyn Counter>("b").unwrap();
    counter.set(55).unwrap();
    client
        .next_value(&mut into, Duration::from_secs(2))
        .unwrap()
        .unwrap();
    assert_eq!(&into[..4], &55u32.to_le_bytes());
}

#[test]
fn a_method_with_arguments_cannot_be_subscribed_to() {
    let running = start();
    let client = Client::connect(&running.socket, "t").unwrap();
    let mut handle = client.open_tag("a", COUNTER).unwrap();
    assert!(matches!(
        client.subscribe(&mut handle, 2, 1000, false),
        Err(Error::Refused {
            status: Status::INVALID_ARGUMENT,
            ..
        })
    ));
}

#[test]
fn unsubscribing_stops_the_values() {
    let running = start();
    let client = Client::connect(&running.socket, "t").unwrap();
    let mut handle = client.open_tag("a", COUNTER).unwrap();
    let sub = client.subscribe(&mut handle, GET, 1000, false).unwrap();
    let mut into = [0; 8];
    client
        .next_value(&mut into, Duration::from_secs(2))
        .unwrap()
        .unwrap();
    client.unsubscribe(sub.id).unwrap();
    // What was already in flight drains, then nothing.
    eventually(|| {
        client
            .next_value(&mut into, Duration::from_millis(30))
            .unwrap()
            .is_none()
    });
}

#[test]
fn a_subscription_cannot_be_ended_by_someone_else() {
    let running = start();
    let mut owner = Raw::connect(&running);
    owner.greet();
    owner.send(Frame::Subscribe {
        id: 1,
        device: 0,
        contract: 0,
        method: GET,
        period_us: 100_000,
        only_changes: false,
    });
    let mut subscription = 0;
    owner.with_next(|frame| match frame {
        Some(Frame::Subscribed {
            subscription: s, ..
        }) => subscription = s,
        other => panic!("{other:?}"),
    });
    let mut other = Raw::connect(&running);
    other.greet();
    other.send(Frame::Unsubscribe {
        id: 2,
        subscription,
    });
    other.with_next(|frame| {
        assert!(matches!(
            frame,
            Some(Frame::Refusal {
                id: 2,
                status: -2,
                ..
            })
        ));
    });
}

#[test]
fn another_protocol_version_is_refused_and_closed() {
    let running = start();
    let mut raw = Raw::connect(&running);
    raw.send(Frame::Hello {
        layout: lemnos_wire::LAYOUT_HASH,
        lemnos: lemnos_wire::LemnosName(lemnos_wire::LEMNOS),
        version: VERSION + 1,
        name: "future",
        events: false,
    });
    raw.with_next(|frame| {
        assert!(matches!(frame, Some(Frame::Refusal { status, .. }) if status == Status::NOT_SUPPORTED.0));
    });
    raw.closes();
}

#[test]
fn a_request_before_hello_closes_the_connection() {
    let running = start();
    let mut raw = Raw::connect(&running);
    raw.send(Frame::List { id: 1 });
    raw.closes();
}

#[test]
fn a_repeated_hello_changes_nothing_and_is_welcomed() {
    let running = start();
    let mut raw = Raw::connect(&running);
    raw.greet();
    raw.greet();
}

#[test]
fn garbage_closes_that_connection_and_no_other() {
    let running = start();
    let client = Client::connect(&running.socket, "t").unwrap();
    let mut raw = Raw::connect(&running);
    raw.greet();
    raw.stream.write_all(&[2, 0, 0, 0, 0xff, 0xff]).unwrap();
    raw.closes();
    assert_eq!(client.list().unwrap().len(), 2);
}

#[test]
fn an_impossible_length_closes_the_connection() {
    let running = start();
    let mut raw = Raw::connect(&running);
    raw.stream.write_all(&u32::MAX.to_le_bytes()).unwrap();
    raw.closes();
}

#[test]
fn a_client_sending_service_frames_is_closed() {
    let running = start();
    let mut raw = Raw::connect(&running);
    raw.greet();
    raw.send(Frame::Reply { id: 1, bytes: &[] });
    raw.closes();
}

#[test]
fn every_refused_request_is_answered_with_its_id() {
    let running = start();
    let mut raw = Raw::connect(&running);
    raw.greet();
    let requests = [
        Frame::Open {
            id: 11,
            device: "x",
            tag: "y/1",
        },
        Frame::Describe { id: 12, device: 99 },
        Frame::Call {
            id: 13,
            device: 99,
            contract: 0,
            method: 1,
            reply_capacity: 4,
            args: &[],
        },
        Frame::Subscribe {
            id: 14,
            device: 99,
            contract: 0,
            method: 1,
            period_us: 1000,
            only_changes: false,
        },
        Frame::Unsubscribe {
            id: 15,
            subscription: 99,
        },
        Frame::SetPower {
            id: 16,
            device: 99,
            on: true,
        },
        Frame::StreamOpen {
            id: 17,
            device: 0,
            name: "s",
        },
        Frame::Call {
            id: 18,
            device: 0,
            contract: 0,
            method: 1,
            reply_capacity: u32::MAX,
            args: &[],
        },
    ];
    for request in requests {
        let Some(id) = (match request {
            Frame::Open { id, .. }
            | Frame::Describe { id, .. }
            | Frame::Call { id, .. }
            | Frame::Subscribe { id, .. }
            | Frame::Unsubscribe { id, .. }
            | Frame::SetPower { id, .. }
            | Frame::StreamOpen { id, .. } => Some(id),
            _ => None,
        }) else {
            continue;
        };
        raw.send(request);
        raw.with_next(|frame| match frame {
            Some(Frame::Refusal { id: got, .. }) => assert_eq!(got, id),
            other => panic!("{other:?}"),
        });
    }
}

#[test]
fn closing_ends_what_the_connection_held() {
    let running = start();
    let mut raw = Raw::connect(&running);
    raw.greet();
    raw.send(Frame::Subscribe {
        id: 1,
        device: 0,
        contract: 0,
        method: GET,
        period_us: 1000,
        only_changes: false,
    });
    raw.with_next(|frame| assert!(matches!(frame, Some(Frame::Subscribed { .. }))));
    drop(raw);
    // A new reader of the same counter sees it stop being driven by the old
    // subscription: the count no longer moves between two reads.
    let client = Client::connect(&running.socket, "t").unwrap();
    let mut counter = client.open::<dyn Counter>("a").unwrap();
    let mut read_twice = || {
        let first = counter.peek().unwrap();
        std::thread::sleep(Duration::from_millis(20));
        first == counter.peek().unwrap()
    };
    eventually(&mut read_twice);
}

#[test]
fn a_connection_that_never_greets_is_cleaned_up() {
    let running = start();
    for _ in 0..50 {
        drop(UnixStream::connect(&running.socket).unwrap());
    }
    let client = Client::connect(&running.socket, "t").unwrap();
    assert_eq!(client.list().unwrap().len(), 2);
}

#[test]
fn a_client_that_stops_reading_does_not_slow_the_others() {
    let running = start();
    let mut stuck = Raw::connect(&running);
    stuck.greet();
    stuck.send(Frame::Subscribe {
        id: 1,
        device: 0,
        contract: 0,
        method: GET,
        period_us: 100,
        only_changes: false,
    });
    let client = Client::connect(&running.socket, "t").unwrap();
    let mut counter = client.open::<dyn Counter>("b").unwrap();
    for _ in 0..200 {
        counter.peek().unwrap();
    }
}

#[test]
fn a_hung_device_answers_on_time_and_others_keep_working() {
    let running = start_with(
        "[[devices]]\nid = \"a\"\ndriver = \"fake-counter\"\n\
         [[devices]]\nid = \"b\"\ndriver = \"fake-counter\"\n",
    );
    let client = Client::connect(&running.socket, "t").unwrap();
    let mut hung = client.open::<dyn Counter>("a").unwrap();
    let started = Instant::now();
    assert_eq!(hung.hang(2_000), Err(Status::TIMED_OUT));
    assert!(started.elapsed() < Duration::from_secs(1));
    let mut good = client.open::<dyn Counter>("b").unwrap();
    good.get().unwrap();
}

#[test]
fn a_second_service_cannot_take_a_live_socket() {
    let running = start();
    let text = "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n";
    let second = lemnosd::Service::start(lemnosd::Config {
        socket: running.socket.clone(),
        name: "second".to_string(),
        board: lemnos_host::board::parse(text).unwrap(),
        drivers: Vec::new(),
        backend: Box::new(lemnos_sim::SimBoard::new()),
        options: lemnos_host::Options::default(),
    });
    assert!(second.is_err());
    Client::connect(&running.socket, "t")
        .unwrap()
        .list()
        .unwrap();
}

#[test]
fn a_client_survives_the_service_restarting() {
    let mut running = start();
    let socket = running.socket.clone();
    let client = Client::connect(&socket, "t").unwrap();
    let mut counter = client.open::<dyn Counter>("a").unwrap();
    counter.get().unwrap();
    running.service.take().unwrap().stop();
    assert!(!socket.exists());
    assert!(counter.get().is_err());
}

#[test]
fn the_board_files_host_settings_reach_the_service_and_the_rest_keep_its_own() {
    let running = start_with(
        "[host]\nretry_first_ms = 300\nretry_cap_ms = 5000\n\
         [[devices]]\nid = \"counter\"\ndriver = \"fake-counter\"\n",
    );
    let options = running.service.as_ref().unwrap().host().options();
    assert_eq!(options.retry_first, Duration::from_millis(300));
    assert_eq!(options.retry_cap, Duration::from_secs(5));
    // Not set in the board file: what the service was started with.
    assert_eq!(options.call_deadline, Duration::from_millis(200));
}

#[test]
fn a_client_from_another_lemnos_revision_is_answered_with_the_hosts_layout_and_closed() {
    let running = start();
    let mut raw = Raw::connect(&running);
    raw.send(Frame::Hello {
        layout: 0x1122_3344_5566_7788,
        lemnos: lemnos_wire::LemnosName("0.0.1"),
        // A complete Hello from another revision: the daemon refuses on its layout alone.
        version: VERSION,
        name: "old",
        events: false,
    });
    raw.with_next(|frame| {
        assert!(matches!(
            frame,
            Some(Frame::Welcome { layout, lemnos, .. })
                if layout == lemnos_wire::LAYOUT_HASH && lemnos.0 == lemnos_wire::LEMNOS
        ));
    });
    raw.closes();
}
