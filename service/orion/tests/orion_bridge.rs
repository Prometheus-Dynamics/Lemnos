//! The service against a real lemnosd (its service, over the simulated board, with the fake
//! accelerometer and counter) and a real in-process Orion node over its IPC servers.
//!
//! Carried from the reference bridge test, generic: the service has no code for either device.
//! The devices are named only in the board text and in the resource ids the checks read. The
//! reference test's actions (`set`, `restore`, a refused action) are not here: this service only
//! publishes.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use lemnos_fake_devices::{FakeAccelerometer, FakeCounter};
use lemnos_host::board::parse;
use lemnos_host::{Options, entry};
use lemnos_orion::config::Config;
use lemnos_orion::{Shared, publish, source};
use lemnos_sim::SimBoard;
use lemnosd::{Config as LemnosdConfig, Service};
use orion_control_plane::{StatusQuery, StatusSubject, TypedConfigValue};
use orion_core::ResourceId;
use orion_node::{NodeApp, NodeConfig, NodeId};

const BOARD: &str = "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
    [[devices]]\nid = \"imu\"\ndriver = \"fake-accelerometer\"\n\
    [devices.config]\nx = 1.5\ny = 0\nz = 2\n\
    [[devices]]\nid = \"thing\"\ndriver = \"fake-counter\"\n[devices.config]\nstart = 41\n";

static NEXT: AtomicU32 = AtomicU32::new(0);

/// A fresh directory for one test's sockets. Unix socket paths are short, so the name is short.
fn dir(test: &str) -> PathBuf {
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("lo-{test}-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("test directory");
    dir
}

fn resource(name: &str) -> ResourceId {
    ResourceId::new(format!("lemnos.{name}"))
}

/// lemnosd, over the simulated board with the fake devices.
fn start_lemnosd(socket: &Path) -> Service {
    Service::start(LemnosdConfig {
        socket: socket.to_path_buf(),
        name: "t".to_string(),
        board: parse(BOARD).expect("board"),
        drivers: vec![entry::<FakeAccelerometer>(), entry::<FakeCounter>()],
        backend: Box::new(SimBoard::new()),
        options: Options::default(),
    })
    .expect("lemnosd")
}

/// An Orion node on its own IPC sockets.
struct Node {
    app: NodeApp,
    servers: tokio::task::JoinHandle<()>,
    socket: PathBuf,
    stream: PathBuf,
}

async fn start_node(socket: PathBuf, stream: PathBuf, node: &str) -> Node {
    let _ = std::fs::remove_file(&socket);
    let _ = std::fs::remove_file(&stream);
    let app = NodeApp::try_new(
        NodeConfig::for_local_node(NodeId::new(node)).with_ipc_socket_path(socket.clone()),
    )
    .expect("node app");
    let (_, unary) = app
        .start_ipc_server_graceful(&socket)
        .await
        .expect("unary server");
    let (_, streams) = app
        .start_ipc_stream_server_graceful(&stream)
        .await
        .expect("stream server");
    let servers = tokio::spawn(async move {
        let _servers = (unary, streams);
        std::future::pending::<()>().await;
    });
    Node {
        app,
        servers,
        socket,
        stream,
    }
}

impl Node {
    fn stop(self) {
        self.servers.abort();
        let _ = std::fs::remove_file(&self.socket);
        let _ = std::fs::remove_file(&self.stream);
    }
}

/// The service: the lemnosd thread and the publisher, on the test's runtime.
struct Bridge {
    shared: Arc<Shared>,
    source: Option<std::thread::JoinHandle<()>>,
    publish: tokio::task::JoinHandle<()>,
}

fn start_bridge(
    socket: PathBuf,
    orion_socket: PathBuf,
    orion_stream: PathBuf,
    node: &str,
) -> Bridge {
    let shared = Arc::new(Shared::new());
    let config = Config {
        lemnosd_socket: socket.clone(),
        orion_socket,
        orion_stream,
        node_id: node.to_owned(),
    };
    let source = {
        let shared = Arc::clone(&shared);
        std::thread::spawn(move || source::run(&socket, &shared))
    };
    let publish = tokio::spawn(publish::run(config, Arc::clone(&shared)));
    Bridge {
        shared,
        source: Some(source),
        publish,
    }
}

impl Bridge {
    fn stop(mut self) {
        self.publish.abort();
        self.shared.stop();
        if let Some(source) = self.source.take() {
            let _ = source.join();
        }
    }
}

/// Polls `check` until it returns `Some`, or panics after `secs`.
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

fn resource_labels(node: &NodeApp, id: &ResourceId) -> Option<Vec<String>> {
    node.state_snapshot()
        .state
        .observed
        .resources
        .get(id)
        .map(|record| record.labels.clone())
}

fn status_of(node: &NodeApp, id: &ResourceId, key: &str) -> Option<TypedConfigValue> {
    node.query_status(&StatusQuery {
        subject: Some(StatusSubject::Resource(id.clone())),
        key_prefix: None,
    })
    .into_iter()
    .find(|entry| entry.key == key)
    .map(|entry| entry.value)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn devices_are_resources_their_values_are_pushed_and_changes_follow() {
    let dir = dir("pushed");
    let lemnosd_socket = dir.join("lemnosd.sock");
    let orion_socket = dir.join("orion.sock");
    let orion_stream = dir.join("orion-stream.sock");
    let service = start_lemnosd(&lemnosd_socket);
    let node = start_node(orion_socket.clone(), orion_stream.clone(), "node-a").await;
    let bridge = start_bridge(lemnosd_socket, orion_socket, orion_stream, "node-a");

    let imu = resource("imu");
    let thing = resource("thing");
    wait_for(15, "the imu resource", || resource_labels(&node.app, &imu)).await;
    let labels = wait_for(15, "the counter resource", || {
        resource_labels(&node.app, &thing)
    })
    .await;
    assert!(labels.contains(&"lemnos.driver=fake-counter".to_owned()));

    // The accelerometer's fields are its values, published as they are read. Each is published
    // on its own, so wait for all of them rather than for one and then look at the others.
    wait_for(15, "the imu's x, z and status", || {
        let published = status_of(&node.app, &imu, "x") == Some(TypedConfigValue::F64(1.5))
            && status_of(&node.app, &imu, "z") == Some(TypedConfigValue::F64(2.0))
            && status_of(&node.app, &imu, "status")
                == Some(TypedConfigValue::String("running".to_owned()));
        published.then_some(())
    })
    .await;

    // The counter counts on every read, so its value moves with no timer of ours.
    let first = wait_for(15, "the counter's first value", || {
        match status_of(&node.app, &thing, "count") {
            Some(TypedConfigValue::F64(value)) => Some(value),
            _ => None,
        }
    })
    .await;
    wait_for(15, "a later counter value", || {
        match status_of(&node.app, &thing, "count") {
            Some(TypedConfigValue::F64(value)) if value > first + 2.0 => Some(()),
            _ => None,
        }
    })
    .await;

    bridge.stop();
    node.stop();
    drop(service);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_orion_restart_gets_every_device_and_value_again() {
    let dir = dir("restart");
    let lemnosd_socket = dir.join("lemnosd.sock");
    let orion_socket = dir.join("orion.sock");
    let orion_stream = dir.join("orion-stream.sock");
    let service = start_lemnosd(&lemnosd_socket);
    let mut node = start_node(orion_socket.clone(), orion_stream.clone(), "node-a").await;
    let bridge = start_bridge(
        lemnosd_socket,
        orion_socket.clone(),
        orion_stream.clone(),
        "node-a",
    );

    let imu = resource("imu");
    wait_for(15, "the imu resource", || resource_labels(&node.app, &imu)).await;
    wait_for(15, "the imu's x", || {
        (status_of(&node.app, &imu, "x") == Some(TypedConfigValue::F64(1.5))).then_some(())
    })
    .await;

    // Orion goes away and comes back empty. The next publish fails, the session is made again
    // and every device and value is sent again.
    node.stop();
    tokio::time::sleep(Duration::from_millis(300)).await;
    node = start_node(orion_socket, orion_stream, "node-a").await;
    wait_for(20, "the imu resource after the restart", || {
        resource_labels(&node.app, &imu)
    })
    .await;
    wait_for(20, "the imu's x after the restart", || {
        (status_of(&node.app, &imu, "x") == Some(TypedConfigValue::F64(1.5))).then_some(())
    })
    .await;

    bridge.stop();
    node.stop();
    drop(service);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_node_that_is_not_the_configured_one_gets_nothing() {
    let dir = dir("node");
    let lemnosd_socket = dir.join("lemnosd.sock");
    let orion_socket = dir.join("orion.sock");
    let orion_stream = dir.join("orion-stream.sock");
    let service = start_lemnosd(&lemnosd_socket);
    let node = start_node(orion_socket.clone(), orion_stream.clone(), "node-a").await;
    // The service is told to publish on node-b; Orion's welcome says node-a.
    let bridge = start_bridge(lemnosd_socket, orion_socket, orion_stream, "node-b");

    let imu = resource("imu");
    let deadline = Instant::now() + Duration::from_secs(4);
    while Instant::now() < deadline {
        assert!(
            resource_labels(&node.app, &imu).is_none(),
            "nothing is published to a node that is not ORION_NODE_ID"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    bridge.stop();
    node.stop();
    drop(service);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restarted_lemnosd_takes_its_devices_away_and_brings_them_back() {
    let dir = dir("lemnosd");
    let lemnosd_socket = dir.join("lemnosd.sock");
    let orion_socket = dir.join("orion.sock");
    let orion_stream = dir.join("orion-stream.sock");
    let service = start_lemnosd(&lemnosd_socket);
    let node = start_node(orion_socket.clone(), orion_stream.clone(), "node-a").await;
    let bridge = start_bridge(lemnosd_socket.clone(), orion_socket, orion_stream, "node-a");

    let imu = resource("imu");
    wait_for(15, "the imu resource", || resource_labels(&node.app, &imu)).await;

    // lemnosd stops: its devices are taken away from Orion, and the service keeps running.
    drop(service);
    wait_for(20, "the imu resource removed with lemnosd", || {
        resource_labels(&node.app, &imu).is_none().then_some(())
    })
    .await;

    // lemnosd comes back: the devices are published again.
    let service = start_lemnosd(&lemnosd_socket);
    wait_for(30, "the imu resource back with lemnosd", || {
        resource_labels(&node.app, &imu)
    })
    .await;
    wait_for(15, "the imu's x back", || {
        (status_of(&node.app, &imu, "x") == Some(TypedConfigValue::F64(1.5))).then_some(())
    })
    .await;

    bridge.stop();
    node.stop();
    drop(service);
}
