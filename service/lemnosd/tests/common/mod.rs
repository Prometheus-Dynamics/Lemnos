#![allow(dead_code)]

use std::io;
use std::os::fd::OwnedFd;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lemnos_fake_devices::FakeCounter;
use lemnos_host::board::parse;
use lemnos_host::{Options, entry};
use lemnos_linux_sys::dma_buf::sealed_memfd;
use lemnos_sim::SimBoard;
use lemnosd::{BufferSource, Config, Service};

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

/// Sealed memfds standing in for the dma-bufs of a heap, so no test needs a device node. What
/// it does wrong, when told to, is what a heap or a broken source could.
#[derive(Default)]
pub struct StandIn {
    /// Every request: the heap, the bytes of one buffer, and how many buffers.
    pub asked: Mutex<Vec<(String, u64, u32)>>,
    /// Refuse, as a heap that is missing or says no.
    pub fail: Mutex<Option<io::ErrorKind>>,
    /// Hand back one buffer fewer than asked.
    pub fewer: bool,
    /// Hand back buffers one byte long.
    pub short: bool,
}

impl BufferSource for StandIn {
    fn allocate(&self, heap: &str, bytes: u64, count: u32) -> io::Result<Vec<OwnedFd>> {
        self.asked
            .lock()
            .unwrap()
            .push((heap.to_string(), bytes, count));
        if let Some(kind) = *self.fail.lock().unwrap() {
            return Err(kind.into());
        }
        let count = if self.fewer { count - 1 } else { count };
        let bytes = if self.short { 1 } else { bytes };
        (0..count)
            .map(|_| sealed_memfd(c"lemnosd-test-buffer", bytes))
            .collect()
    }
}

pub fn start_with(devices: &str) -> Running {
    start_on(devices, None)
}

/// A service whose streams of dma-bufs come from `buffers`.
pub fn start_with_buffers(devices: &str, buffers: Arc<dyn BufferSource>) -> Running {
    start_on(devices, Some(buffers))
}

fn start_on(devices: &str, buffers: Option<Arc<dyn BufferSource>>) -> Running {
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
    let config = Config {
        socket: socket.clone(),
        name: "test-host".to_string(),
        board: parse(&text).unwrap(),
        drivers: vec![entry::<FakeCounter>()],
        backend: Box::new(SimBoard::new()),
        options: Options {
            call_deadline: Duration::from_millis(200),
            ..Options::default()
        },
    };
    let service = match buffers {
        None => Service::start(config),
        Some(buffers) => Service::start_with_buffers(config, buffers),
    }
    .unwrap();
    Running {
        service: Some(service),
        socket,
        dir,
    }
}
