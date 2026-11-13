//! Services on the simulated board for the surface's tests, and a producer for
//! their streams standing in for a driver.

#![allow(dead_code)]

use std::io;
use std::os::fd::{AsFd, OwnedFd};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use lemnos_client::{PackageEntry, PackageList};
use lemnos_fake_devices::FakeCounter;
use lemnos_hal::StreamEndpoint;
use lemnos_hal::blocks::StreamFrame;
use lemnos_host::board::parse;
use lemnos_host::stream::ProducerEndpoint;
use lemnos_host::{Options, entry};
use lemnos_linux_sys::dma_buf::{self, sealed_memfd};
use lemnos_sim::SimBoard;
use lemnosd::packages::{PackageFile, PackageMode, PackageReport};
use lemnosd::{BufferSource, Config, Service};

/// Bytes of a frame on `frames`, not a whole page.
pub const FRAME_BYTES: usize = 5000;
/// Bytes of a buffer on `input`.
pub const INPUT_BYTES: usize = 256;

/// A service and the directory to remove when it goes.
pub struct Running {
    pub service: Option<Service>,
    pub socket: PathBuf,
    dir: PathBuf,
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(service) = self.service.take() {
            service.stop();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Running {
    pub fn service(&self) -> &Service {
        self.service.as_ref().unwrap()
    }
}

fn place() -> (PathBuf, PathBuf) {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "lemnos-capi-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    (dir.join("sock"), dir)
}

fn config(socket: PathBuf, devices: &str) -> Config {
    let text =
        format!("format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"c\"\n{devices}");
    Config {
        socket,
        name: "c".to_string(),
        board: parse(&text).unwrap(),
        drivers: vec![entry::<FakeCounter>()],
        backend: Box::new(SimBoard::new()),
        options: Options::default(),
    }
}

/// One counter, nothing else.
pub fn service() -> (Service, PathBuf, PathBuf) {
    let (socket, dir) = place();
    let devices = "[[devices]]\nid = \"thing\"\ndriver = \"fake-counter\"\n";
    let service = Service::start(config(socket.clone(), devices)).unwrap();
    (service, socket, dir)
}

/// Sealed memfds standing in for a heap's dma-bufs, so no test opens a device
/// node.
struct StandIn;

impl BufferSource for StandIn {
    fn allocate(&self, _heap: &str, bytes: u64, count: u32) -> io::Result<Vec<OwnedFd>> {
        (0..count)
            .map(|_| sealed_memfd(c"lemnos-capi-test-buffer", bytes))
            .collect()
    }
}

/// A device with four streams: `frames` (dma-bufs the producer fills),
/// `input` (dma-bufs the consumer fills, for jobs), `plain` (shared memory)
/// and `planar` (dma-bufs of two planes, 200 and 100 bytes, a test fills).
pub fn stream_service() -> Running {
    let (socket, dir) = place();
    let devices = format!(
        "[[devices]]\nid = \"cam\"\ndriver = \"fake-counter\"\n\
         [devices.streams.frames]\nbuffers = 4\nbuffer_bytes = {FRAME_BYTES}\n\
         memory = \"dma-buf\"\n\
         [devices.streams.input]\nbuffers = 2\nbuffer_bytes = {INPUT_BYTES}\n\
         memory = \"dma-buf\"\n\
         [devices.streams.plain]\nbuffers = 2\nbuffer_bytes = 128\n\
         [devices.streams.planar]\nbuffers = 2\nbuffer_bytes = 300\n\
         memory = \"dma-buf\"\nplanes = [200, 100]\n"
    );
    let service =
        Service::start_with_buffers(config(socket.clone(), &devices), Arc::new(StandIn)).unwrap();
    Running {
        service: Some(service),
        socket,
        dir,
    }
}

/// What [`package_service`] says of its packages, as the client gives it.
pub fn package_list() -> PackageList {
    PackageList {
        mode: lemnos_client::PackageMode::Signed,
        note: "one key file line was not a key".to_string(),
        packages: vec![
            PackageEntry {
                path: "pkgs/fan.lpkg".to_string(),
                driver: Some("fan".to_string()),
                reason: String::new(),
            },
            PackageEntry {
                path: "pkgs/odd.lpkg".to_string(),
                driver: None,
                reason: "not signed by a trusted key".to_string(),
            },
        ],
    }
}

/// A service that looked at two packages, loaded one and refused the other.
pub fn package_service() -> Running {
    let (socket, dir) = place();
    let report = PackageReport {
        mode: PackageMode::Signed,
        note: "one key file line was not a key".to_string(),
        files: vec![
            PackageFile {
                path: "pkgs/fan.lpkg".into(),
                driver: Some("fan".to_string()),
                reason: String::new(),
            },
            PackageFile {
                path: "pkgs/odd.lpkg".into(),
                driver: None,
                reason: "not signed by a trusted key".to_string(),
            },
        ],
    };
    let devices = "[[devices]]\nid = \"thing\"\ndriver = \"fake-counter\"\n";
    let service = Service::start_with_packages(config(socket.clone(), devices), report).unwrap();
    Running {
        service: Some(service),
        socket,
        dir,
    }
}

/// What the producer writes into frame `sequence`.
pub fn fill(sequence: u64, out: &mut [u8]) {
    for (at, byte) in out.iter_mut().enumerate() {
        *byte = sequence.wrapping_mul(7).wrapping_add(at as u64) as u8;
    }
}

pub fn frame_is_right(bytes: &[u8], sequence: u64) -> bool {
    bytes.len() == FRAME_BYTES
        && bytes
            .iter()
            .enumerate()
            .all(|(at, byte)| *byte == sequence.wrapping_mul(7).wrapping_add(at as u64) as u8)
}

/// Publishes a filled frame on `frames` for every buffer that comes back, and
/// answers each job on `input`, which names a buffer, with that buffer's bytes
/// inverted, as a frame on the same buffer.
pub struct Producing {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    /// The dma-buf behind each buffer of `frames` in the attachment that stands.
    identities: Arc<Mutex<[(u64, u64); 4]>>,
}

impl Producing {
    pub fn start(running: &Running) -> Self {
        let host = running.service().host();
        let frames = host.stream_producer(0, "frames").unwrap();
        let input = host.stream_producer(0, "input").unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let identities = Arc::new(Mutex::new([(0, 0); 4]));
        let thread = {
            let (stop, identities) = (Arc::clone(&stop), Arc::clone(&identities));
            std::thread::spawn(move || produce(frames, input, &stop, &identities))
        };
        Self {
            stop,
            thread: Some(thread),
            identities,
        }
    }

    pub fn identity(&self, buffer: u32) -> (u64, u64) {
        self.identities.lock().unwrap()[buffer as usize]
    }

    pub fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.thread.take().unwrap().join().unwrap();
    }
}

impl Drop for Producing {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn frame(buffer: u32, bytes: usize, sequence: u64) -> StreamFrame {
    StreamFrame {
        buffer,
        bytes: bytes as u32,
        sequence,
        identity: 0,
        timestamp_ns: 0,
    }
}

fn produce(
    mut frames: ProducerEndpoint,
    mut input: ProducerEndpoint,
    stop: &AtomicBool,
    identities: &Mutex<[(u64, u64); 4]>,
) {
    let mut pattern = vec![0u8; FRAME_BYTES];
    let mut sequence = 0;
    let mut held = [false; 2];
    let mut job = [0u8; 16];
    let mut bytes = [0u8; INPUT_BYTES];
    while !stop.load(Ordering::Relaxed) {
        let mut busy = false;
        // At most a stream's worth at a time: once no consumer is attached, a
        // buffer published comes straight back.
        for _ in 0..4 {
            let Ok(Some(buffer)) = frames.next_buffer() else {
                break;
            };
            busy = true;
            if let Ok(Some(handle)) = frames.buffer_handle(buffer) {
                identities.lock().unwrap()[buffer as usize] =
                    dma_buf::identity(handle.fd().as_fd()).unwrap();
            }
            fill(sequence, &mut pattern);
            let _ = frames.write(buffer, 0, &pattern);
            let _ = frames.publish(frame(buffer, FRAME_BYTES, sequence));
            sequence += 1;
        }
        let take = |input: &mut ProducerEndpoint, held: &mut [bool; 2]| {
            for _ in 0..2 {
                let Ok(Some(buffer)) = input.next_buffer() else {
                    break;
                };
                held[buffer as usize] = true;
            }
        };
        take(&mut input, &mut held);
        if let Ok(Some(4)) = input.next_job(&mut job) {
            busy = true;
            let buffer = u32::from_le_bytes([job[0], job[1], job[2], job[3]]);
            let Some(place) = held.get(buffer as usize) else {
                continue;
            };
            // The consumer gives the buffer back before posting the job that
            // names it, and the two may be seen here in either order.
            let until = Instant::now() + Duration::from_secs(5);
            let mut ours = *place;
            while !ours && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(1));
                take(&mut input, &mut held);
                ours = held[buffer as usize];
            }
            // A buffer held since before this consumer came has nothing behind
            // it: it is given up and taken again, as the image drivers do.
            if ours && matches!(input.buffer_handle(buffer), Ok(None)) {
                let _ = input.publish(frame(buffer, 0, 0));
                held[buffer as usize] = false;
                take(&mut input, &mut held);
                ours = held[buffer as usize];
            }
            if ours && input.read(buffer, 0, &mut bytes) == Ok(true) {
                for byte in &mut bytes {
                    *byte = !*byte;
                }
                let _ = input.write(buffer, 0, &bytes);
                let _ = input.publish(frame(buffer, INPUT_BYTES, 0));
                held[buffer as usize] = false;
            }
        }
        if !busy {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}
