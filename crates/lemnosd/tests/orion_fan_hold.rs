//! The Orion bridge's timed `set` (`for_ms`) against a real in-process `orion-node` and
//! lemnosd's mock service with a fake `pwm-fan`: the hold ends by itself (the fan is
//! released to its governor), a renewal pushes the end back, and a `release` ends it early.

#![cfg(feature = "orion")]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use lemnos_ipc::{ClientEvent, ClientOptions, Event, Update};
use lemnosd::mock::{MockHardware, MockLemnosd};
use lemnosd::orion::bridge::{self, Config};
use lemnosd::orion::mirror::resource_id;
use orion_control_plane::{
    ActionQuery, ActionRequest, ActionState, ActionTarget, TypedConfigValue,
};
use orion_node::{NodeApp, NodeConfig, NodeId};

const BOARD: &str = r#"
format = "lemnos.board"
schema_version = 1

[board]
id = "hold"

[[devices]]
id = "fan"
driver = "hwmon-fan"
match = { name = "pwmfan" }
"#;

fn sock(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("lo-hold-{name}-{}.sock", std::process::id()))
}

fn write(root: &Path, path: &str, contents: &str) {
    let path = root.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

fn link(root: &Path, link: &str, target: &str) {
    let link = root.join(link);
    std::fs::create_dir_all(link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(root.join(target), link).unwrap();
}

/// A `pwm-fan` with its cooling device, under `root` (a sysfs root).
fn fan_tree(root: &Path) {
    let fan = "devices/platform/cooling_fan";
    write(root, &format!("{fan}/hwmon/hwmon2/name"), "pwmfan");
    write(root, &format!("{fan}/hwmon/hwmon2/pwm1"), "77");
    write(root, &format!("{fan}/hwmon/hwmon2/pwm1_enable"), "1");
    link(root, &format!("{fan}/hwmon/hwmon2/device"), fan);
    write(root, "bus/platform/drivers/pwm-fan/bind", "");
    link(
        root,
        &format!("{fan}/driver"),
        "bus/platform/drivers/pwm-fan",
    );
    link(root, "class/hwmon/hwmon2", &format!("{fan}/hwmon/hwmon2"));
    let cdev = "devices/virtual/thermal/cooling_device0";
    write(root, &format!("{cdev}/type"), "pwm-fan");
    write(root, &format!("{cdev}/cur_state"), "1");
    write(root, &format!("{cdev}/max_state"), "4");
    link(root, "class/thermal/cooling_device0", cdev);
}

async fn wait_for<T>(secs: u64, what: &str, mut check: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if let Some(value) = check() {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn run_action(
    node: &NodeApp,
    id: &str,
    name: &str,
    args: BTreeMap<String, TypedConfigValue>,
) -> ActionState {
    let mut request =
        ActionRequest::new(id, ActionTarget::Resource(resource_id("hold", "fan")), name);
    request.args = args;
    node.run_action(request, "app").expect("run action");
    wait_for(10, "action result", || {
        node.query_actions(&ActionQuery::action(id))
            .into_iter()
            .next()
            .filter(|r| !matches!(r.state, ActionState::Accepted | ActionState::Running { .. }))
    })
    .await
    .state
}

fn duty(value: f64, for_ms: Option<u64>) -> BTreeMap<String, TypedConfigValue> {
    let mut args = BTreeMap::from([
        (
            "control".to_owned(),
            TypedConfigValue::String("duty".to_owned()),
        ),
        ("value".to_owned(), TypedConfigValue::F64(value)),
    ]);
    if let Some(ms) = for_ms {
        args.insert("for_ms".to_owned(), TypedConfigValue::UInt(ms));
    }
    args
}

/// The fan's releases by the bridge's caller connection, as lemnosd announces them, each
/// with when it was seen.
fn watch_releases(socket: PathBuf) -> mpsc::Receiver<Instant> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut client = ClientOptions::new(&socket, "watcher").devices().unwrap();
        loop {
            match client.next_event_timeout(Duration::from_millis(100)) {
                Ok(Some(ClientEvent::Data(Update::Event(Event::Control {
                    control, by, ..
                }))))
                    if control == "release" && by.starts_with("orion:") && by.ends_with(":app") =>
                {
                    if tx.send(Instant::now()).is_err() {
                        return;
                    }
                }
                Ok(_) => {}
                Err(_) => return,
            }
        }
    });
    rx
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_timed_set_releases_the_fan_when_it_ends() {
    let (socket, stream) = (sock("unary"), sock("stream"));
    let _ = std::fs::remove_file(&socket);
    let _ = std::fs::remove_file(&stream);
    let root = std::env::temp_dir().join(format!("lo-hold-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    fan_tree(&root.join("sys"));
    let mock = MockLemnosd::start_in(root.clone(), BOARD, MockHardware::new()).expect("mock");
    let node = NodeApp::try_new(
        NodeConfig::for_local_node(NodeId::new("node-a")).with_ipc_socket_path(socket.clone()),
    )
    .expect("node app");
    let (_, unary) = node
        .start_ipc_server_graceful(&socket)
        .await
        .expect("unary");
    let (_, streams) = node
        .start_ipc_stream_server_graceful(&stream)
        .await
        .expect("stream");
    let bridge = tokio::spawn(bridge::run(Config {
        lemnosd_socket: mock.socket().to_path_buf(),
        orion_socket: socket.clone(),
        orion_stream: stream.clone(),
        node_id: "node-a".to_owned(),
        board_file: None,
        rate_hz: 20.0,
        heartbeat: Duration::from_millis(500),
        ttl: Duration::from_secs(5),
        retry: Duration::from_millis(100),
        channels: Vec::new(),
        batch: Duration::from_millis(50),
    }));
    let releases = watch_releases(mock.socket().to_path_buf());
    let rid = resource_id("hold", "fan");
    wait_for(15, "fan resource", || {
        node.state_snapshot()
            .state
            .observed
            .resources
            .contains_key(&rid)
            .then_some(())
    })
    .await;

    // A bad hold is refused before anything is written.
    assert!(matches!(
        run_action(&node, "bad", "set", duty(0.5, Some(0))).await,
        ActionState::Rejected { .. }
    ));

    // Held 600 ms: released by itself, after about that long.
    let start = Instant::now();
    assert_eq!(
        run_action(&node, "hold-1", "set", duty(0.8, Some(600))).await,
        ActionState::Succeeded
    );
    let released = releases
        .recv_timeout(Duration::from_secs(5))
        .expect("released");
    let held = released - start;
    assert!(
        held >= Duration::from_millis(550),
        "released after {held:?}"
    );

    // Renewed before it ends: the release moves to the renewal's end.
    let start = Instant::now();
    assert_eq!(
        run_action(&node, "hold-2", "set", duty(0.8, Some(600))).await,
        ActionState::Succeeded
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        run_action(&node, "hold-3", "set", duty(0.9, Some(600))).await,
        ActionState::Succeeded
    );
    let released = releases
        .recv_timeout(Duration::from_secs(5))
        .expect("released");
    let held = released - start;
    assert!(
        held >= Duration::from_millis(850),
        "renewed hold released after {held:?}"
    );
    assert!(
        releases.recv_timeout(Duration::from_millis(800)).is_err(),
        "released twice"
    );

    // Ended early by `release`: the timer then finds nothing to end.
    assert_eq!(
        run_action(&node, "hold-4", "set", duty(0.8, Some(500))).await,
        ActionState::Succeeded
    );
    assert_eq!(
        run_action(&node, "release-1", "release", BTreeMap::new()).await,
        ActionState::Succeeded
    );
    releases
        .recv_timeout(Duration::from_secs(5))
        .expect("the release");
    assert!(
        releases.recv_timeout(Duration::from_millis(900)).is_err(),
        "the ended hold released again"
    );

    bridge.abort();
    drop((unary, streams));
    mock.stop();
    let _ = std::fs::remove_file(&socket);
    let _ = std::fs::remove_file(&stream);
    let _ = std::fs::remove_dir_all(&root);
}
