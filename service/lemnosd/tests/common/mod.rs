#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use lemnos_fake_devices::FakeCounter;
use lemnos_host::board::parse;
use lemnos_host::{Options, entry};
use lemnos_sim::SimBoard;
use lemnosd::{Config, Service};

pub const COUNTER: &str = "example.counter/1";

pub struct Running {
    pub service: Option<Service>,
    pub socket: PathBuf,
    dir: PathBuf,
}

impl Drop for Running {
    fn drop(&mut self) {
        drop(self.service.take());
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

pub fn start() -> Running {
    start_with(
        "[[devices]]\nid = \"a\"\ndriver = \"fake-counter\"\n\
         [[devices]]\nid = \"b\"\ndriver = \"fake-counter\"\n[devices.config]\nstart = 100\n",
    )
}

pub fn start_with(devices: &str) -> Running {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "lemnosd-test-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("sock");
    let text =
        format!("format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n{devices}");
    let service = Service::start(Config {
        socket: socket.clone(),
        name: "test-host".to_string(),
        board: parse(&text).unwrap(),
        drivers: vec![entry::<FakeCounter>()],
        backend: Box::new(SimBoard::new()),
        options: Options {
            call_deadline: Duration::from_millis(200),
            ..Options::default()
        },
    })
    .unwrap();
    Running {
        service: Some(service),
        socket,
        dir,
    }
}
