//! The LED test layer end to end: a selftest's look shows over another
//! client's status, and falls back when its lease runs out or its client
//! disconnects.

use lemnos_board::{BoardDefinition, BoardError, Buses, DynI2c};
use lemnos_drivers_linux::SysRoot;
use lemnos_ipc::{ClientOptions, LedShow, LedStatus};
use lemnosd::{Service, ServiceConfig};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

struct SysBuses(PathBuf);

impl Buses for SysBuses {
    fn i2c(&mut self, bus: u32) -> Result<DynI2c, BoardError> {
        Err(BoardError::Device {
            device: format!("i2c-{bus}"),
            kind: lemnos_hal::ErrorKind::NotFound,
            reason: "no I2C in this test".into(),
        })
    }

    fn sys(&self) -> SysRoot {
        SysRoot::new(&self.0)
    }
}

fn board(root: &Path) -> BoardDefinition {
    BoardDefinition::from_toml_str(&format!(
        r#"
format = "lemnos.board"
schema_version = 1

[board]
id = "raze"

[[devices]]
id = "ring"
driver = "ws2812"
path = "{root}/dev/leds0"
config = {{ count = 4, fade_ms = 0, status_effect = "solid", ok = 0x00ff00 }}
"#,
        root = root.display()
    ))
    .unwrap()
}

/// The ring's first LED as written (`r | g << 8 | b << 16 | w << 24`).
fn first(root: &Path) -> Option<[u8; 4]> {
    let bytes = fs::read(root.join("dev/leds0")).ok()?;
    bytes.get(..4).map(|b| [b[0], b[1], b[2], b[3]])
}

fn eventually(mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !check() {
        assert!(Instant::now() < deadline, "timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn test_layer_shows_over_status_and_falls_back() {
    let root = std::env::temp_dir().join(format!("lemnosd-led-layers-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("dev")).unwrap();
    fs::write(root.join("dev/leds0"), "").unwrap();
    let socket = root.join("lemnosd.sock");
    let stop = Arc::new(AtomicBool::new(false));
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (thread_stop, thread_root, thread_socket) =
        (Arc::clone(&stop), root.clone(), socket.clone());
    let handle = std::thread::spawn(move || {
        let config = ServiceConfig::new(board(&thread_root), thread_socket);
        let mut service = Service::new(config, Box::new(SysBuses(thread_root))).unwrap();
        ready_tx.send(()).unwrap();
        service.run(&thread_stop).unwrap();
        service.shutdown(false);
    });
    ready_rx.recv_timeout(Duration::from_secs(30)).unwrap();

    // Another client's high-priority status.
    let mut app = ClientOptions::new(&socket, "helios")
        .priority(250)
        .leds()
        .unwrap();
    app.status(LedStatus::Ok).unwrap();
    app.sync().unwrap();
    eventually(|| first(&root) == Some([0, 255, 0, 0]));

    // A low-priority selftest's frame wins over it.
    let mut selftest = ClientOptions::new(&socket, "atlas-selftest")
        .priority(0)
        .leds()
        .unwrap();
    selftest
        .test(LedShow::Frame(vec![0xff0000; 4]), None)
        .unwrap();
    selftest.sync().unwrap();
    eventually(|| first(&root) == Some([255, 0, 0, 0]));
    // The app's status changes underneath, still hidden.
    app.status(LedStatus::Error).unwrap();
    app.sync().unwrap();
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(first(&root), Some([255, 0, 0, 0]));
    app.status(LedStatus::Ok).unwrap();
    app.sync().unwrap();

    // Clearing only the test intent: back to the status.
    selftest.clear_test().unwrap();
    selftest.sync().unwrap();
    eventually(|| first(&root) == Some([0, 255, 0, 0]));

    // The selftest disappears: its intent goes with it.
    selftest.test(LedShow::Color(0x0000ff), None).unwrap();
    selftest.sync().unwrap();
    eventually(|| first(&root) == Some([0, 0, 255, 0]));
    drop(selftest);
    eventually(|| first(&root) == Some([0, 255, 0, 0]));

    // A client that keeps its intents (lemnos-ctl) gets a lease instead.
    let mut ctl = ClientOptions::new(&socket, "lemnos-ctl")
        .keep_intents()
        .leds()
        .unwrap();
    ctl.test(LedShow::Color(0xffffff), Some(Duration::from_millis(300)))
        .unwrap();
    ctl.sync().unwrap();
    eventually(|| first(&root) == Some([255, 255, 255, 0]));
    drop(ctl);
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(first(&root), Some([255, 255, 255, 0]));
    eventually(|| first(&root) == Some([0, 255, 0, 0]));

    drop(app);
    stop.store(true, Ordering::Relaxed);
    handle.join().unwrap();
    let _ = fs::remove_dir_all(&root);
}
