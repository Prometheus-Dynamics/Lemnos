mod common;

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use common::{FakeHost, temp_dir};
use lemnos_client::{
    Client, DeviceEntry, DeviceHealth, DeviceState, Error, Incoming, PackageEntry, PackageMode,
    Session, SubscriptionState, follow,
};
use lemnos_core::{
    Contract, ContractInfo, Decode, Encode, MethodInfo, Param, Result, Shape, Status, Tag, contract,
};

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

/// A service that sets BOARD_ID says its board is its Welcome's host: the board file's id.
#[test]
fn the_board_id_is_the_board_the_service_says_it_serves() {
    let host = FakeHost::start();
    host.control
        .features
        .store(lemnos_wire::BOARD_ID, Ordering::SeqCst);
    let client = Client::connect(&host.path, "the-tool").unwrap();
    assert_eq!(client.board_id().unwrap(), "fake");
}

/// A service from before BOARD_ID names itself, and the client does not take that name for a
/// board: the answer is refused by name, so nothing is guessed.
#[test]
fn a_service_that_does_not_say_its_board_is_refused_not_guessed() {
    let host = FakeHost::start();
    let client = Client::connect(&host.path, "the-tool").unwrap();
    match client.board_id() {
        Err(Error::Refused { status, text }) => {
            assert_eq!(status, Status::NOT_SUPPORTED);
            assert!(text.contains("board"), "{text}");
        }
        other => panic!("expected NOT_SUPPORTED, got {other:?}"),
    }
    // The service is still usable: the refusal is the call's alone.
    assert_eq!(client.list().unwrap().len(), 2);
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
    // What each is wired to, as the board file's `link` has it.
    assert_eq!(devices[0].link.as_deref(), Some("two"));
    assert_eq!(devices[1].link, None);
    let chain: Vec<&str> = follow(&devices, "one")
        .into_iter()
        .map(|d| d.name.as_str())
        .collect();
    assert_eq!(chain, ["one", "two"]);
}

fn entry(index: u32, name: &str, link: Option<&str>) -> DeviceEntry {
    DeviceEntry {
        index,
        name: name.to_string(),
        driver: "d".to_string(),
        tags: Vec::new(),
        state: DeviceState::Running,
        health: DeviceHealth::Ok,
        reason: String::new(),
        link: link.map(str::to_string),
        jobs: Vec::new(),
        refusal: lemnos_client::Refusal::NONE,
    }
}

#[test]
fn links_are_followed_from_a_sensor_to_its_receiver_and_its_isp_and_always_end() {
    let names = |devices: &[DeviceEntry], from: &str| -> Vec<String> {
        follow(devices, from)
            .into_iter()
            .map(|d| d.name.clone())
            .collect()
    };
    let board = [
        entry(0, "isp0", None),
        entry(1, "cam0", Some("csi0")),
        entry(2, "csi0", Some("isp0")),
        entry(3, "lone", None),
        entry(4, "dangling", Some("nowhere")),
    ];
    assert_eq!(names(&board, "cam0"), ["cam0", "csi0", "isp0"]);
    assert_eq!(names(&board, "csi0"), ["csi0", "isp0"]);
    assert_eq!(names(&board, "lone"), ["lone"]);
    assert_eq!(
        names(&board, "dangling"),
        ["dangling"],
        "ends where the board does"
    );
    assert!(names(&board, "nowhere").is_empty());
    // A board whose links go round ends too, and names each device once.
    let circle = [
        entry(0, "a", Some("b")),
        entry(1, "b", Some("c")),
        entry(2, "c", Some("a")),
        entry(3, "self", Some("self")),
    ];
    assert_eq!(names(&circle, "b"), ["b", "c", "a"]);
    assert_eq!(names(&circle, "self"), ["self"]);
}

#[test]
fn packages_are_listed_as_the_service_reports_them_and_only_asked_of_one_that_can_say() {
    let host = FakeHost::start();
    // A service from before it could say is never sent the question: it would hang up.
    let client = Client::connect(&host.path, "t").unwrap();
    assert!(matches!(
        client.packages(),
        Err(Error::Refused {
            status: Status::NOT_SUPPORTED,
            ..
        })
    ));
    assert_eq!(host.control.package_asks.load(Ordering::SeqCst), 0);
    assert!(client.is_connected());
    assert_eq!(client.list().unwrap().len(), 2);

    host.control
        .features
        .store(lemnos_wire::feature::PACKAGES, Ordering::SeqCst);
    host.control
        .package_mode
        .store(lemnos_wire::package_mode::SIGNED.into(), Ordering::SeqCst);
    let client = Client::connect(&host.path, "t").unwrap();
    let list = client.packages().unwrap();
    assert_eq!(list.mode, PackageMode::Signed);
    assert_eq!(list.note, "half read");
    assert_eq!(
        list.packages,
        [
            PackageEntry {
                path: "/p/a.so".to_string(),
                driver: Some("a".to_string()),
                reason: String::new(),
            },
            PackageEntry {
                path: "/p/b.so".to_string(),
                driver: None,
                reason: "unsigned".to_string(),
            },
        ]
    );
    for (mode, expected) in [
        (lemnos_wire::package_mode::OFF, PackageMode::Off),
        (lemnos_wire::package_mode::OPEN, PackageMode::Open),
    ] {
        host.control
            .package_mode
            .store(mode.into(), Ordering::SeqCst);
        assert_eq!(client.packages().unwrap().mode, expected);
    }
    // A mode nobody defined is not guessed at.
    host.control.package_mode.store(9, Ordering::SeqCst);
    assert_eq!(
        client.packages(),
        Err(Error::Wire(lemnos_wire::WireError::Malformed))
    );
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

/// How long a refused connect may take to say so. The session has no clock of its own to read, so
/// this is wall time: a refused `connect(2)` on a Unix socket returns in microseconds, and the fault
/// this guards against is a whole retry backoff (one second, `RETRY_FIRST`). A quarter of that is
/// far above any scheduling stall seen on a loaded machine and far below the fault.
const REFUSED_AT_ONCE: Duration = Duration::from_millis(250);

/// A socket path nothing listens on: absent (ENOENT), or left by a listener that is gone
/// (ECONNREFUSED).
fn refusing_paths(dir: &std::path::Path) -> [std::path::PathBuf; 2] {
    let absent = dir.join("absent");
    let stale = dir.join("stale");
    drop(std::os::unix::net::UnixListener::bind(&stale).unwrap());
    [absent, stale]
}

#[test]
fn a_refused_connect_fails_at_once_not_a_backoff_later() {
    let dir = temp_dir("refused");
    for path in refusing_paths(&dir.0) {
        let started = Instant::now();
        assert_eq!(
            Client::connect(&path, "t").unwrap_err(),
            Error::Disconnected
        );
        let client = started.elapsed();
        let started = Instant::now();
        assert_eq!(
            Session::connect(&path, "t").unwrap_err(),
            Error::Disconnected
        );
        let session = started.elapsed();
        assert!(
            client < REFUSED_AT_ONCE && session < REFUSED_AT_ONCE,
            "{}: Client::connect took {client:?}, Session::connect {session:?}",
            path.display()
        );
    }
}

/// A caller that reconnects through `connect` with a delay of its own, and reports that delay as
/// how long until it tries again, must be telling the truth: the time between its tries is its
/// delay, with nothing of the session's added.
#[test]
fn a_callers_stated_reconnect_delay_is_the_real_wait_between_its_connects() {
    const STATED: Duration = Duration::from_millis(50);
    let dir = temp_dir("delay");
    for path in refusing_paths(&dir.0) {
        let mut tries = Vec::new();
        for _ in 0..4 {
            tries.push(Instant::now());
            assert_eq!(
                Client::connect(&path, "t").unwrap_err(),
                Error::Disconnected
            );
            std::thread::sleep(STATED);
        }
        for pair in tries.windows(2) {
            let real = pair[1] - pair[0];
            assert!(
                real >= STATED && real < STATED + REFUSED_AT_ONCE,
                "{}: stated {STATED:?}, real {real:?}",
                path.display()
            );
        }
    }
}

#[test]
fn a_service_from_another_lemnos_revision_is_refused_at_connect() {
    let host = FakeHost::start();
    host.control
        .welcome_layout
        .store(0xdead_beef_0bad_f00d, Ordering::SeqCst);
    let error = Client::connect(&host.path, "t").unwrap_err();
    assert_eq!(
        error,
        Error::LayoutMismatch {
            ours: lemnos_wire::CORE_HASH,
            theirs: 0xdead_beef_0bad_f00d,
            theirs_lemnos: lemnos_wire::LEMNOS.to_string(),
        }
    );
    let text = error.to_string();
    assert!(text.contains("0xdeadbeef0badf00d"), "{text}");
    assert!(
        text.contains(&format!("{:#018x}", lemnos_wire::CORE_HASH)),
        "{text}"
    );
    assert!(text.contains("core protocol differs"), "{text}");
    assert_eq!(lemnos_core::Status::from(error), Status::VERSION_MISMATCH);
}

#[test]
fn a_typed_contract_whose_encoding_differs_on_the_device_is_refused() {
    const ECHO: ContractInfo = <dyn Echo as Contract>::INFO;
    // The same contract with `add` taking a u64: the device's hash moves, and the typed client
    // must refuse it before anything is called.
    const WIDER: ContractInfo = ContractInfo {
        tag: Tag::new(TAG),
        kind: "other",
        methods: &[MethodInfo {
            id: 1,
            name: "add",
            params: &[Param {
                name: "value",
                shape: Shape::U64,
            }],
            returns: Shape::U32,
            has_out_buffer: false,
            job: false,
            stall_secs: 0,
            reading: None,
            added: false,
        }],
    };
    let host = FakeHost::start();
    *host.control.provides.lock().unwrap() = Some(&[WIDER]);
    let client = Client::connect(&host.path, "t").unwrap();
    let Err(error) = client.open::<dyn Echo>("") else {
        panic!("a contract laid out differently must not open");
    };
    assert_eq!(
        error,
        Error::ContractMismatch {
            contract: TAG.to_string(),
            ours: <dyn Echo as Contract>::HASH,
            theirs: lemnos_core::layout::contract_hash(&WIDER),
        }
    );
    assert!(error.to_string().contains(TAG), "{error}");
    // The same contract, laid out the same, opens.
    *host.control.provides.lock().unwrap() = Some(&[ECHO]);
    let mut echo = client.open::<dyn Echo>("").unwrap();
    assert_eq!(echo.add(7).unwrap(), 7);
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
    *host.control.provides.lock().unwrap() = Some(&[<dyn Echo as Contract>::INFO]);
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
fn a_change_the_service_tells_of_ends_a_wait_for_events_and_none_is_none() {
    let host = FakeHost::start();
    let client = Client::connect(&host.path, "t").unwrap();
    let started = Instant::now();
    assert!(client.wait_event(Duration::from_millis(100)).is_none());
    assert!(started.elapsed() >= Duration::from_millis(100));
    client.set_power(10, false).unwrap();
    let event = client.wait_event(Duration::from_secs(5)).unwrap();
    assert_eq!(
        (event.device, event.state, event.reason.as_str()),
        (10, DeviceState::Off, "powered off")
    );
    assert!(client.wait_event(Duration::from_millis(50)).is_none());
}

/// How much later than its bound a bounded connect may fail: the time one sleep may overrun.
const BOUND_TOLERANCE: Duration = Duration::from_millis(250);

#[test]
fn a_bounded_connect_connects_when_the_service_appears_inside_the_bound() {
    // The socket appears 300 ms after the connect starts: the first try is refused, and the
    // next one, after the 1 s backoff, finds it.
    let host = FakeHost::start_after(Duration::from_millis(300));
    let bound = Duration::from_secs(5);
    let started = Instant::now();
    let client = Client::connect_within(&host.path, "t", bound).unwrap();
    assert!(started.elapsed() < bound);
    assert!(client.is_connected());
    client.list().unwrap();
}

#[test]
fn a_bounded_connect_fails_at_its_bound_when_the_service_never_appears() {
    let dir = temp_dir("never");
    let path = dir.0.join("sock");
    let bound = Duration::from_millis(1500);
    let started = Instant::now();
    let error = Client::connect_within(&path, "t", bound).unwrap_err();
    let took = started.elapsed();
    assert_eq!(error, Error::TimedOut);
    assert!(
        took >= bound,
        "failed after {took:?}, before the {bound:?} bound"
    );
    assert!(
        took < bound + BOUND_TOLERANCE,
        "failed after {took:?}, more than {BOUND_TOLERANCE:?} past the {bound:?} bound"
    );
}

#[test]
fn a_zero_bound_connects_as_connect_does() {
    let dir = temp_dir("zero");
    let path = dir.0.join("sock");
    let started = Instant::now();
    let bounded = Client::connect_within(&path, "t", Duration::ZERO).unwrap_err();
    let plain = Client::connect(&path, "t").unwrap_err();
    assert_eq!(
        (&bounded, &plain),
        (&Error::Disconnected, &Error::Disconnected)
    );
    assert!(started.elapsed() < BOUND_TOLERANCE, "a zero bound waited");

    let host = FakeHost::start();
    let client = Client::connect_within(&host.path, "t", Duration::ZERO).unwrap();
    client.list().unwrap();
}

#[test]
fn events_the_queue_drops_are_counted_and_the_oldest_are_the_ones_lost() {
    let host = FakeHost::start();
    let client = Client::connect(&host.path, "t").unwrap();
    assert_eq!(client.dropped_events(), 0);
    // Each power-off makes the service send one event for its device, and nothing reads them
    // here: 70 sent, the queue keeps 64.
    for device in 0..70 {
        client.set_power(device, false).unwrap();
    }
    assert_eq!(client.dropped_events(), 6);
    let mut kept = Vec::new();
    while let Some(event) = client.next_event() {
        kept.push(event.device);
    }
    assert_eq!(kept, (6..70).collect::<Vec<u32>>());
    // Reading does not reset the count.
    assert_eq!(client.dropped_events(), 6);
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
    *host.control.provides.lock().unwrap() = Some(&[<dyn Echo as Contract>::INFO]);
    let client = Client::connect(&host.path, "t").unwrap();
    let mut echo = client.open::<dyn Echo>("").unwrap();
    assert_eq!(echo.add(1).unwrap(), 1);
    host.restart();
    // The call in flight may fail. It comes back, with a new device number.
    eventually(|| echo.add(2) == Ok(2));
    assert!(host.control.connections.load(Ordering::SeqCst) >= 2);
}

#[test]
fn a_subscribe_that_times_out_leaves_nothing_live_on_the_service() {
    let host = FakeHost::start();
    // The service answers 400 ms late; the caller gives up at 150 ms.
    host.control.subscribe_delay_ms.store(400, Ordering::SeqCst);
    let client = Client::connect(&host.path, "t").unwrap();
    client.set_timeout(Duration::from_millis(150));
    let mut handle = client.open_tag("", TAG).unwrap();
    assert_eq!(
        client.subscribe(&mut handle, 1, 5000, false),
        Err(Error::TimedOut)
    );
    // The late answer is read, and unsubscribed: no value reaches the caller, and the service
    // ends with nothing live.
    let mut into = [0; 8];
    eventually(|| {
        let value = client
            .next_value(&mut into, Duration::from_millis(50))
            .unwrap();
        assert!(
            value.is_none(),
            "a value arrived for a subscribe that was cancelled"
        );
        host.control.answers.load(Ordering::SeqCst) >= 1 && host.control.live_subscriptions() == 0
    });
    assert_eq!(host.control.unsubscribed.load(Ordering::SeqCst), 1);
}

#[test]
fn a_pending_subscribe_returns_before_its_answer_and_its_values_come_under_its_id() {
    let host = FakeHost::start();
    host.control.subscribe_delay_ms.store(400, Ordering::SeqCst);
    let client = Client::connect(&host.path, "t").unwrap();
    let mut handle = client.open_tag("", TAG).unwrap();
    let started = Instant::now();
    let id = client
        .subscribe_pending(&mut handle, 1, 5000, false)
        .unwrap();
    assert!(
        started.elapsed() < Duration::from_millis(200),
        "subscribe_pending waited for the answer"
    );
    assert_eq!(client.subscription_state(id), SubscriptionState::Pending);
    let mut into = [0; 8];
    let value = client
        .next_value(&mut into, Duration::from_secs(5))
        .unwrap()
        .expect("no value after the answer");
    assert_eq!(value.subscription, id);
    assert_eq!(
        client.subscription_state(id),
        SubscriptionState::Live { period_us: 5000 }
    );
    assert_eq!(host.control.live_subscriptions(), 1);
}

#[test]
fn a_refused_pending_subscribe_reports_refused_once_and_is_not_sent_again() {
    let host = FakeHost::start();
    host.control.refuse_subscribe.store(true, Ordering::SeqCst);
    let client = Client::connect(&host.path, "t").unwrap();
    let mut handle = client.open_tag("", TAG).unwrap();
    let id = client
        .subscribe_pending(&mut handle, 1, 5000, false)
        .unwrap();
    let mut into = [0; 8];
    let mut seen = None;
    eventually(|| {
        client
            .next_value(&mut into, Duration::from_millis(20))
            .unwrap();
        if seen.is_none() {
            let state = client.subscription_state(id);
            if state != SubscriptionState::Pending {
                seen = Some(state);
            }
        }
        seen.is_some()
    });
    assert_eq!(seen, Some(SubscriptionState::Refused(Status(-16))));
    assert_eq!(client.subscription_state(id), SubscriptionState::Unknown);
    host.control.refuse_subscribe.store(false, Ordering::SeqCst);
    host.restart();
    eventually(|| client.list().is_ok());
    assert_eq!(host.control.subscribes.load(Ordering::SeqCst), 1);
    assert_eq!(host.control.live_subscriptions(), 0);
}

#[test]
fn unsubscribing_a_pending_subscribe_then_a_late_answer_leaves_nothing_live() {
    let host = FakeHost::start();
    host.control.subscribe_delay_ms.store(400, Ordering::SeqCst);
    let client = Client::connect(&host.path, "t").unwrap();
    let mut handle = client.open_tag("", TAG).unwrap();
    let id = client
        .subscribe_pending(&mut handle, 1, 5000, false)
        .unwrap();
    client.unsubscribe(id).unwrap();
    assert_eq!(client.subscription_state(id), SubscriptionState::Unknown);
    let mut into = [0; 8];
    eventually(|| {
        let value = client
            .next_value(&mut into, Duration::from_millis(50))
            .unwrap();
        assert!(
            value.is_none(),
            "a value arrived for an unsubscribed subscribe"
        );
        host.control.answers.load(Ordering::SeqCst) >= 1 && host.control.live_subscriptions() == 0
    });
    assert_eq!(host.control.unsubscribed.load(Ordering::SeqCst), 1);
}

#[test]
fn a_pending_subscription_is_sent_again_after_a_reconnect_and_is_live_once() {
    let host = FakeHost::start();
    host.control.subscribe_delay_ms.store(400, Ordering::SeqCst);
    let client = Client::connect(&host.path, "t").unwrap();
    let mut handle = client.open_tag("", TAG).unwrap();
    let id = client
        .subscribe_pending(&mut handle, 1, 5000, false)
        .unwrap();
    // The service has the subscribe and is still answering it when the connection ends.
    let mut into = [0; 8];
    eventually(|| {
        let _ = client.next_value(&mut into, Duration::from_millis(20));
        host.control.subscribes.load(Ordering::SeqCst) >= 1
    });
    assert_eq!(client.subscription_state(id), SubscriptionState::Pending);
    host.restart();
    let mut into = [0; 8];
    let mut got = None;
    eventually(|| {
        if let Some(value) = client
            .next_value(&mut into, Duration::from_millis(50))
            .unwrap()
        {
            got = Some(value.subscription);
        }
        // Done once the service has it sent again, and live, after the reconnect.
        got == Some(id)
            && host.control.subscribes.load(Ordering::SeqCst) >= 2
            && host.control.live_subscriptions() == 1
    });
    assert_eq!(got, Some(id));
    assert_eq!(host.control.subscribes.load(Ordering::SeqCst), 2);
    assert_eq!(host.control.live_subscriptions(), 1);
}

#[test]
fn a_refused_subscribe_is_dropped_and_is_not_sent_again_after_a_reconnect() {
    let host = FakeHost::start();
    host.control.refuse_subscribe.store(true, Ordering::SeqCst);
    let client = Client::connect(&host.path, "t").unwrap();
    let mut handle = client.open_tag("", TAG).unwrap();
    let error = client.subscribe(&mut handle, 1, 5000, false).unwrap_err();
    assert!(matches!(error, Error::Refused { .. }), "{error:?}");
    host.control.refuse_subscribe.store(false, Ordering::SeqCst);
    host.restart();
    eventually(|| client.list().is_ok());
    assert_eq!(host.control.subscribes.load(Ordering::SeqCst), 1);
}

#[test]
fn a_subscribe_that_timed_out_and_is_refused_late_is_dropped() {
    let host = FakeHost::start();
    host.control.subscribe_delay_ms.store(400, Ordering::SeqCst);
    host.control.refuse_subscribe.store(true, Ordering::SeqCst);
    let client = Client::connect(&host.path, "t").unwrap();
    client.set_timeout(Duration::from_millis(150));
    let mut handle = client.open_tag("", TAG).unwrap();
    assert_eq!(
        client.subscribe(&mut handle, 1, 5000, false),
        Err(Error::TimedOut)
    );
    // The refusal arrives with the next answer; it drops the subscription.
    eventually(|| client.list().is_ok());
    host.control.refuse_subscribe.store(false, Ordering::SeqCst);
    host.restart();
    eventually(|| client.list().is_ok());
    assert_eq!(host.control.subscribes.load(Ordering::SeqCst), 1);
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
    let started = Instant::now();
    assert_eq!(client.list().unwrap_err(), Error::Disconnected);
    // The first call is the first try, and the refusal is its answer.
    assert!(
        started.elapsed() < REFUSED_AT_ONCE,
        "{:?}",
        started.elapsed()
    );
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

#[contract("example.counter", version = 1)]
pub trait Jobbing {
    #[method(2, job, stall = 60)]
    fn run(&mut self, how: u8) -> Result<lemnos_core::JobId>;
}

fn jobs_host() -> FakeHost {
    let host = FakeHost::start();
    *host.control.provides.lock().unwrap() = Some(&[<dyn Jobbing as Contract>::INFO]);
    host.control
        .features
        .store(lemnos_wire::feature::JOBS, Ordering::SeqCst);
    host
}

#[test]
fn a_job_reports_progress_and_its_result() {
    let host = jobs_host();
    let client = Client::connect(&host.path, "t").unwrap();
    let job = client.open::<dyn Jobbing>("one").unwrap().run(0).unwrap();
    let mut seen = Vec::new();
    let end = client.wait_job(job.0, Duration::from_secs(5), &mut |d, t| seen.push((d, t)));
    assert_eq!(end, Some((Status::OK, vec![0])));
    assert_eq!(seen, [(1, 2)]);
}

#[test]
fn a_job_that_ends_before_it_is_said_to_have_started_still_ends_once() {
    let host = jobs_host();
    let client = Client::connect(&host.path, "t").unwrap();
    let job = client.open::<dyn Jobbing>("one").unwrap().run(1).unwrap();
    let end = client.wait_job(job.0, Duration::from_secs(5), &mut |_, _| {});
    assert_eq!(end, Some((Status::OK, vec![1])));
    // Not left open: its late JobStarted didn't open it again.
    assert_eq!(
        client.wait_job(job.0, Duration::ZERO, &mut |_, _| {}),
        Some((Status::NOT_FOUND, Vec::new()))
    );
}

#[test]
fn a_cancelled_job_ends_as_the_service_says() {
    let host = jobs_host();
    let client = Client::connect(&host.path, "t").unwrap();
    let job = client.open::<dyn Jobbing>("one").unwrap().run(2).unwrap();
    assert_eq!(
        client.wait_job(job.0, Duration::from_millis(20), &mut |_, _| {}),
        None
    );
    client.cancel_job(job.0).unwrap();
    assert_eq!(*host.control.cancels.lock().unwrap(), [job.0]);
    let end = client.wait_job(job.0, Duration::from_secs(5), &mut |_, _| {});
    assert_eq!(end, Some((Status::CANCELLED, Vec::new())));
}

#[test]
fn an_open_job_ends_as_interrupted_when_the_connection_goes() {
    let host = jobs_host();
    let client = Client::connect(&host.path, "t").unwrap();
    let job = client.open::<dyn Jobbing>("one").unwrap().run(2).unwrap();
    host.restart();
    let end = client.wait_job(job.0, Duration::from_secs(5), &mut |_, _| {});
    assert_eq!(end, Some((Status::INTERRUPTED, Vec::new())));
}

#[test]
fn a_service_without_jobs_is_never_sent_one() {
    let host = FakeHost::start();
    *host.control.provides.lock().unwrap() = Some(&[<dyn Jobbing as Contract>::INFO]);
    let client = Client::connect(&host.path, "t").unwrap();
    let err = client
        .open::<dyn Jobbing>("one")
        .unwrap()
        .run(0)
        .unwrap_err();
    assert_eq!(err, Status::NOT_SUPPORTED);
    // The connection is fine.
    client.list().unwrap();
    assert_eq!(host.control.connections.load(Ordering::SeqCst), 1);
}

#[test]
fn a_polled_job_is_started_and_followed_by_poll() {
    let host = jobs_host();
    let mut session = Session::connect_polled(&host.path, "t").unwrap();
    let handle = session.open("", TAG).unwrap();
    let id = session.send_job(&handle, 2, &[0, 7]).unwrap();
    let mut started = None;
    let mut events = Vec::new();
    eventually(|| {
        session.poll(&mut |incoming| match incoming {
            Incoming::JobStarted { id: got, job } if got == id => started = Some(job),
            Incoming::Job(event) => events.push(event.clone()),
            _ => {}
        });
        events.len() == 2
    });
    let job = started.expect("JobStarted came");
    assert_eq!(
        events,
        [
            lemnos_client::JobEvent::Progress {
                job,
                done: 1,
                total: 2
            },
            lemnos_client::JobEvent::Done {
                job,
                status: Status::OK,
                result: vec![0, 7]
            },
        ]
    );
}

/// The argument and result of the counter as an older client has it: one field.
#[derive(Debug, Clone, Copy, PartialEq, Default, Encode, Decode)]
pub struct Sum0 {
    pub value: u32,
}

/// The same struct as a newer driver has it: a field appended after the base.
#[derive(Debug, Clone, Copy, PartialEq, Default, Encode, Decode)]
pub struct Sum {
    pub value: u32,
    #[added]
    pub scale: u32,
}

/// The counter as an older client was built with it.
#[contract("example.counter", version = 1)]
pub trait OldCounter {
    #[method(1)]
    fn add(&mut self, args: Sum0) -> Result<Sum0>;
}

/// The counter as a newer driver provides it: the field and a method appended.
#[contract("example.counter", version = 1)]
pub trait NewCounter {
    #[method(1)]
    fn add(&mut self, args: Sum) -> Result<Sum0>;
    #[added]
    #[method(2)]
    fn twice(&mut self, args: Sum0) -> Result<Sum0>;
}

/// A client that needs a method marked breaking: a change to the base, not an addition.
#[contract("example.counter", version = 1)]
pub trait BreakingCounter {
    #[method(1)]
    fn add(&mut self, args: Sum0) -> Result<Sum0>;
    #[added]
    #[breaking]
    #[method(2)]
    fn twice(&mut self, args: Sum0) -> Result<Sum0>;
}

/// Another contract, changed in its base.
#[contract("example.other", version = 1)]
pub trait Other {
    #[method(1)]
    fn read(&mut self, args: u32) -> Result<u32>;
}

#[contract("example.other", version = 1)]
pub trait OtherChanged {
    #[method(1)]
    fn read(&mut self, args: u64) -> Result<u64>;
}

const OLD: ContractInfo = <dyn OldCounter as Contract>::INFO;
const NEW: ContractInfo = <dyn NewCounter as Contract>::INFO;
const OTHER: ContractInfo = <dyn Other as Contract>::INFO;
const OTHER_CHANGED: ContractInfo = <dyn OtherChanged as Contract>::INFO;

#[test]
fn an_old_client_works_with_a_driver_that_gained_a_method_and_a_field() {
    let host = FakeHost::start();
    *host.control.provides.lock().unwrap() = Some(&[NEW]);
    let client = Client::connect(&host.path, "t").unwrap();
    // Same base shape: the old client binds, and its call goes through with its own argument.
    let mut counter = client.open::<dyn OldCounter>("").unwrap();
    assert_eq!(counter.add(Sum0 { value: 7 }).unwrap(), Sum0 { value: 7 });
}

#[test]
fn a_new_client_is_refused_by_an_old_driver_that_lacks_the_added_method() {
    let host = FakeHost::start();
    *host.control.provides.lock().unwrap() = Some(&[OLD]);
    let client = Client::connect(&host.path, "t").unwrap();
    let Err(error) = client.open::<dyn NewCounter>("") else {
        panic!("a client needing a method the driver lacks must not open");
    };
    assert_eq!(
        error,
        Error::MethodMissing {
            contract: TAG.to_string(),
            method: 2,
            name: "twice".to_string(),
        }
    );
    assert_eq!(
        error.to_string(),
        "contract example.counter/1: this client needs method 2 `twice`; the device's driver does not provide it"
    );
    assert_eq!(lemnos_core::Status::from(error), Status::NOT_SUPPORTED);
}

#[test]
fn a_device_missing_an_added_field_refuses_the_method_that_uses_it() {
    // The device's method 1 takes `Sum0`, with no `scale`; the client's method 1 takes `Sum`.
    const OLD_METHOD_ONLY: ContractInfo = ContractInfo {
        tag: Tag::new(TAG),
        kind: "other",
        methods: &[OLD.methods[0], NEW.methods[1]],
    };
    let host = FakeHost::start();
    *host.control.provides.lock().unwrap() = Some(&[OLD_METHOD_ONLY]);
    let client = Client::connect(&host.path, "t").unwrap();
    let Err(error) = client.open::<dyn NewCounter>("") else {
        panic!("a field the device lacks must refuse the open");
    };
    assert_eq!(
        error,
        Error::FieldMissing {
            contract: TAG.to_string(),
            method: 1,
            name: "add".to_string(),
            field: "scale".to_string(),
        }
    );
    assert_eq!(
        error.to_string(),
        "contract example.counter/1: this client needs field `scale` of method 1 `add`; the device's driver does not provide it"
    );
}

/// The counter's added method 2 as a driver with another shape provides it: same number and name,
/// different argument.
#[contract("example.counter", version = 1)]
pub trait Reshaped {
    #[method(1)]
    fn add(&mut self, args: Sum) -> Result<Sum0>;
    #[added]
    #[method(2)]
    fn twice(&mut self, args: u32) -> Result<Sum0>;
}

#[test]
fn a_method_of_another_shape_is_refused_at_bind() {
    const RESHAPED: ContractInfo = <dyn Reshaped as Contract>::INFO;
    let host = FakeHost::start();
    *host.control.provides.lock().unwrap() = Some(&[RESHAPED]);
    let client = Client::connect(&host.path, "t").unwrap();
    let Err(error) = client.open::<dyn NewCounter>("") else {
        panic!("a method of another shape must not open");
    };
    assert_eq!(
        error,
        Error::MethodShape {
            contract: TAG.to_string(),
            method: 2,
            name: "twice".to_string(),
        }
    );
    assert_eq!(
        error.to_string(),
        "contract example.counter/1: this client needs method 2 `twice`; the device's driver has another shape"
    );
}

#[test]
fn a_breaking_addition_is_refused_at_bind() {
    let host = FakeHost::start();
    *host.control.provides.lock().unwrap() = Some(&[OLD]);
    let client = Client::connect(&host.path, "t").unwrap();
    let Err(error) = client.open::<dyn BreakingCounter>("") else {
        panic!("a breaking addition must not open against a driver without it");
    };
    assert_eq!(
        error,
        Error::ContractMismatch {
            contract: TAG.to_string(),
            ours: <dyn BreakingCounter as Contract>::HASH,
            theirs: lemnos_core::layout::contract_hash(&OLD),
        }
    );
}

#[test]
fn a_client_of_one_contract_is_unaffected_by_a_change_in_another() {
    let host = FakeHost::start();
    *host.control.provides.lock().unwrap() = Some(&[OTHER_CHANGED, OLD]);
    let client = Client::connect(&host.path, "t").unwrap();
    // The counter binds and calls, though `example.other` changed in its base.
    let mut counter = client.open::<dyn OldCounter>("").unwrap();
    assert_eq!(counter.add(Sum0 { value: 11 }).unwrap(), Sum0 { value: 11 });
    // And the changed contract refuses its own old client.
    let Err(error) = client.open::<dyn Other>("") else {
        panic!("the changed contract must refuse the old client");
    };
    assert!(matches!(error, Error::ContractMismatch { .. }), "{error:?}");
    // Unchanged on its own, the same contract opens.
    *host.control.provides.lock().unwrap() = Some(&[OTHER, OLD]);
    let mut other = client.open::<dyn Other>("").unwrap();
    assert_eq!(other.read(5).unwrap(), 5);
}

#[test]
fn the_session_open_runs_the_same_check() {
    let host = FakeHost::start();
    *host.control.provides.lock().unwrap() = Some(&[OLD]);
    let mut session = Session::connect(&host.path, "t").unwrap();
    assert!(matches!(
        session.open_typed::<dyn NewCounter>(""),
        Err(Error::MethodMissing { method: 2, .. })
    ));
    session.open_typed::<dyn OldCounter>("").unwrap();
}
