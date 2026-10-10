//! Stopping for a reboot leaves the ring on the static reboot ember (12% of
//! the amber), faded to over about 1.2 s, and never delays the stop by more
//! than 1.5 s.

use lemnos_board::{BoardDefinition, BoardError, Buses, DynI2c};
use lemnos_drivers_linux::SysRoot;
use lemnos_ipc::{ClientOptions, LedStatus};
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
config = {{ count = 4, fade_ms = 0, status_effect = "solid", ok = 0x00ff00, rebooting = 0xff8000, look_brightness = 1.0 }}
"#,
        root = root.display()
    ))
    .unwrap()
}

/// Every LED's bytes (`r, g, b, w`).
fn ring(root: &Path) -> Vec<u8> {
    fs::read(root.join("dev/leds0")).unwrap_or_default()
}

fn eventually(mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !check() {
        assert!(Instant::now() < deadline, "timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn a_reboot_leaves_the_ring_on_a_static_ember() {
    let root = std::env::temp_dir().join(format!("lemnosd-reboot-ember-{}", std::process::id()));
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
        let started = Instant::now();
        service.shutdown(true);
        started.elapsed()
    });
    ready_rx.recv_timeout(Duration::from_secs(30)).unwrap();

    // The ring is lit green when the stop comes.
    let mut app = ClientOptions::new(&socket, "helios").leds().unwrap();
    app.status(LedStatus::Ok).unwrap();
    app.sync().unwrap();
    eventually(|| ring(&root).chunks(4).all(|p| p == [0, 178, 0, 0]));

    stop.store(true, Ordering::Relaxed);
    let took = handle.join().unwrap();
    // Faded to the ember (ff8000 at 12%: 31, 16, 0), not one orange frame.
    assert_eq!(ring(&root).len(), 16);
    assert!(
        ring(&root).chunks(4).all(|p| p == [31, 16, 0, 0]),
        "{:?}",
        ring(&root)
    );
    assert!(took >= Duration::from_millis(1_000), "{took:?}");
    assert!(took < Duration::from_millis(2_500), "{took:?}");
    assert!(!socket.exists());
    drop(app);
    let _ = fs::remove_dir_all(&root);
}
