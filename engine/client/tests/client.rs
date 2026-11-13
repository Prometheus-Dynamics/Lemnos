mod common;

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use common::{FakeHost, temp_dir};
use lemnos_client::{
    Client, DeviceEntry, DeviceHealth, DeviceState, Error, Incoming, PackageEntry, PackageMode,
    Session, follow,
};
use lemnos_core::{
    Contract, ContractInfo, MethodInfo, Param, Result, Shape, Status, Tag, contract,
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
            ours: lemnos_wire::LAYOUT_HASH,
            theirs: 0xdead_beef_0bad_f00d,
            theirs_lemnos: lemnos_wire::LEMNOS.to_string(),
        }
    );
    let text = error.to_string();
    assert!(text.contains("0xdeadbeef0badf00d"), "{text}");
    assert!(
        text.contains(&format!("{:#018x}", lemnos_wire::LAYOUT_HASH)),
        "{text}"
    );
    assert!(text.contains("same Lemnos revision"), "{text}");
    assert_eq!(lemnos_core::Status::from(error), Status::VERSION_MISMATCH);
}

#[test]
fn a_typed_contract_whose_encoding_differs_on_the_device_is_refused() {
    const ECHO: ContractInfo = <dyn Echo as Contract>::INFO;
    // The same contract with `add` taking a u64: the device's hash moves, and the typed client
    // must refuse it before anything is called.
    const WIDER: ContractInfo = ContractInfo {
        tag: Tag::new(TAG),
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

#[contract("example.counter", version = 1)]
pub trait Jobbing {
    #[method(2, job)]
    fn run(&mut self, how: u8) -> Result<lemnos_core::JobId>;
}

fn jobs_host() -> FakeHost {
    let host = FakeHost::start();
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
