//! The Orion bridge against a real in-process `orion-node` (its IPC servers)
//! and lemnosd's mock service: resources appear, readings and control events
//! reach Orion as status, a set/restore round trip works, a bad action is
//! refused, and an Orion restart is repaired by republishing.

#![cfg(feature = "orion")]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use lemnosd::mock::{MockHardware, MockLemnosd};
use lemnosd::orion::bridge::{self, Config};
use lemnosd::orion::mirror::resource_id;
use orion_control_plane::{
    ActionQuery, ActionRequest, ActionState, ActionTarget, StatusQuery, StatusSubject,
    TypedConfigValue,
};
use orion_node::{NodeApp, NodeConfig, NodeId};

const BOARD: &str = r#"
format = "lemnos.board"
schema_version = 1

[board]
id = "test"

[[devices]]
id = "lens"
driver = "vcm"
bus = "i2c-1"
address = 0x0c
config = { chip = "dw9714" }
"#;

fn sock(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("lo-{name}-{}.sock", std::process::id()))
}

struct Node {
    app: NodeApp,
    servers: tokio::task::JoinHandle<()>,
}

async fn start_node(socket: &PathBuf, stream: &PathBuf) -> Node {
    let app = NodeApp::try_new(
        NodeConfig::for_local_node(NodeId::new("node-a")).with_ipc_socket_path(socket.clone()),
    )
    .expect("node app");
    let (_, unary) = app
        .start_ipc_server_graceful(socket)
        .await
        .expect("unary server");
    let (_, streams) = app
        .start_ipc_stream_server_graceful(stream)
        .await
        .expect("stream server");
    let servers = tokio::spawn(async move {
        let _servers = (unary, streams);
        std::future::pending::<()>().await;
    });
    Node { app, servers }
}

impl Node {
    fn stop(self, socket: &PathBuf, stream: &PathBuf) {
        self.servers.abort();
        let _ = std::fs::remove_file(socket);
        let _ = std::fs::remove_file(stream);
    }
}

fn resource_rid() -> orion_core::ResourceId {
    resource_id("test", "lens")
}

/// Polls `check` until it returns `Some`, or panics after `secs`.
async fn wait_for<T>(secs: u64, what: &str, mut check: impl FnMut() -> Option<T>) -> T {
    let deadline = std::time::Instant::now() + Duration::from_secs(secs);
    loop {
        if let Some(value) = check() {
            return value;
        }
        assert!(std::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn resource_present(node: &NodeApp) -> bool {
    node.state_snapshot()
        .state
        .observed
        .resources
        .contains_key(&resource_rid())
}

fn status_of(node: &NodeApp, key: &str) -> Option<TypedConfigValue> {
    node.query_status(&StatusQuery {
        subject: Some(StatusSubject::Resource(resource_rid())),
        key_prefix: None,
    })
    .into_iter()
    .find(|entry| entry.key == key)
    .map(|entry| entry.value)
}

async fn run_action(
    node: &NodeApp,
    id: &str,
    name: &str,
    args: BTreeMap<String, TypedConfigValue>,
) -> ActionState {
    let mut request = ActionRequest::new(id, ActionTarget::Resource(resource_rid()), name);
    request.args = args;
    node.run_action(request, "helios").expect("run action");
    let result = wait_for(10, "action result", || {
        node.query_actions(&ActionQuery {
            action_id: Some(id.to_owned()),
            target: None,
        })
        .into_iter()
        .next()
        .filter(|r| !matches!(r.state, ActionState::Accepted | ActionState::Running { .. }))
    })
    .await;
    result.state
}

fn position(value: f64) -> BTreeMap<String, TypedConfigValue> {
    BTreeMap::from([
        (
            "control".to_owned(),
            TypedConfigValue::String("position".to_owned()),
        ),
        ("value".to_owned(), TypedConfigValue::F64(value)),
    ])
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resources_readings_actions_and_restart() {
    let socket = sock("unary");
    let stream = sock("stream");
    let _ = std::fs::remove_file(&socket);
    let _ = std::fs::remove_file(&stream);
    let hardware = MockHardware::new();
    let _ = hardware
        .i2c(1)
        .clone()
        .with_target(0x0c, lemnos_hal::AddressWidth::Bits8);
    let mock = MockLemnosd::start(BOARD, hardware).expect("lemnosd mock");
    let mut node = start_node(&socket, &stream).await;

    let config = Config {
        lemnosd_socket: mock.socket().to_path_buf(),
        orion_socket: socket.clone(),
        orion_stream: stream.clone(),
        node_id: "node-a".to_owned(),
        board_file: None,
        rate_hz: 20.0,
        heartbeat: Duration::from_millis(500),
        ttl: Duration::from_secs(5),
        retry: Duration::from_millis(100),
    };
    let bridge = tokio::spawn(bridge::run(config));

    // Resources appear, with their status.
    wait_for(15, "resource", || resource_present(&node.app).then_some(())).await;
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        if matches!(status_of(&node.app, "status"), Some(TypedConfigValue::String(s)) if s == "available")
        {
            break;
        }
        if std::time::Instant::now() >= deadline {
            let lane: Vec<_> = node
                .app
                .query_status(&StatusQuery::all())
                .into_iter()
                .map(|e| (e.key, e.value))
                .collect();
            panic!("status not available; lane: {lane:?}");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // A set is applied, and comes back as a control event on the status lane.
    assert_eq!(
        run_action(&node.app, "set-1", "set", position(100.0)).await,
        ActionState::Succeeded
    );
    wait_for(10, "control.position after set", || {
        (status_of(&node.app, "control.position") == Some(TypedConfigValue::F64(100.0)))
            .then_some(())
    })
    .await;

    // A refused action (unknown name) is rejected, not failed.
    assert!(matches!(
        run_action(&node.app, "bogus-1", "calibrate", BTreeMap::new()).await,
        ActionState::Rejected { .. }
    ));

    // Restore succeeds. lemnosd can only undo a write when it can read the
    // control first, and the VCM lens cannot be read back in the mock, so the
    // value stays where the write put it; the bridge still tracks lemnosd.
    assert_eq!(
        run_action(&node.app, "restore-1", "restore", BTreeMap::new()).await,
        ActionState::Succeeded
    );
    wait_for(10, "control.position after restore", || {
        (status_of(&node.app, "control.position") == Some(TypedConfigValue::F64(100.0)))
            .then_some(())
    })
    .await;

    // Orion restarts: the bridge reconnects and republishes.
    node.stop(&socket, &stream);
    tokio::time::sleep(Duration::from_millis(300)).await;
    node = start_node(&socket, &stream).await;
    wait_for(20, "resource after restart", || {
        resource_present(&node.app).then_some(())
    })
    .await;
    wait_for(10, "status after restart", || {
        status_of(&node.app, "status").map(|_| ())
    })
    .await;

    bridge.abort();
    node.stop(&socket, &stream);
    mock.stop();
}
