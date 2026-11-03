//! A light with nothing asking for a look shows the default look (status.ok
//! here) from the start, not a dark ring: a ring with no owner is resolved
//! once when the light starts.

use lemnos_board::{BoardDefinition, BoardError, Buses, DynI2c};
use lemnos_drivers_linux::SysRoot;
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

/// A 4-LED ring that nothing asks a look of.
fn board(root: &Path) -> BoardDefinition {
    BoardDefinition::from_toml_str(&format!(
        r#"
format = "lemnos.board"
schema_version = 1

[board]
id = "example"

[[devices]]
id = "ring"
driver = "ws2812"
path = "{root}/dev/leds0"
config = {{ count = 4, fade_ms = 0, ok = 0x00ff00, look_brightness = 1.0 }}
"#,
        root = root.display()
    ))
    .unwrap()
}

/// Every LED's bytes (`r, g, b, w`).
fn ring(root: &Path) -> Vec<u8> {
    fs::read(root.join("dev/leds0")).unwrap_or_default()
}

#[test]
fn an_idle_ring_shows_the_default_look_from_the_start() {
    let root = std::env::temp_dir().join(format!("lemnosd-default-look-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("dev")).unwrap();
    fs::write(root.join("dev/leds0"), "").unwrap();
    let socket = root.join("lemnosd.sock");
    let stop = Arc::new(AtomicBool::new(false));
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (thread_stop, thread_root, thread_socket) =
        (Arc::clone(&stop), root.clone(), socket.clone());
    let handle = std::thread::spawn(move || {
        // No look files: the look table holds only the built-ins.
        let config = ServiceConfig::new(board(&thread_root), thread_socket);
        let mut service = Service::new(config, Box::new(SysBuses(thread_root))).unwrap();
        ready_tx.send(()).unwrap();
        service.run(&thread_stop).unwrap();
        service.shutdown(true);
    });
    ready_rx.recv_timeout(Duration::from_secs(30)).unwrap();

    // Nothing asks for a look: status.ok (green) on every LED.
    let deadline = Instant::now() + Duration::from_secs(5);
    while ring(&root).chunks(4).any(|p| p != [0, 178, 0, 0]) {
        assert!(Instant::now() < deadline, "the ring is {:?}", ring(&root));
        std::thread::sleep(Duration::from_millis(10));
    }

    stop.store(true, Ordering::Relaxed);
    handle.join().unwrap();
    let _ = fs::remove_dir_all(&root);
}
