//! A driver whose consumer fills its buffers, shaped as an ISP is: a job on the `config` stream
//! names a configuration buffer there, an image on `input` and a buffer on `output`. The driver
//! reads the first two with `StreamEndpoint::read`, as the merged ISP and image drivers read
//! theirs, and publishes what they make together on `output`.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use lemnos_client::{FrameStream, Session, StreamPlatform};
use lemnos_core::{Config as DriverConfig, Lifecycle, Result, StreamHandle, contract, driver};
use lemnos_hal::blocks::StreamFrame;
use lemnos_host::board::parse;
use lemnos_host::{Options, entry};
use lemnos_linux_stream::LinuxStreams;
use lemnos_sim::SimBoard;
use lemnosd::{BufferSource, Config, Service};

use super::Running;

pub const DEVICE: u32 = 0;
pub const BUFFERS: u32 = 4;
/// Not a whole page, so a buffer's own bytes and what a heap rounds it to differ.
pub const IMAGE: usize = 5000;
pub const KEY: usize = 16;
const JOB: usize = 20;
const POLL_NS: u64 = 500_000;

#[contract("example.filled", version = 1)]
pub trait Filled {
    /// Jobs run.
    #[method(1)]
    fn done(&mut self) -> Result<u32>;
    /// Jobs whose buffers the driver could not read or write.
    #[method(2)]
    fn unread(&mut self) -> Result<u32>;
}

#[driver("filled", provides = [Filled])]
pub struct FilledDriver {
    #[needs(job)]
    config: StreamHandle,
    #[needs(job)]
    input: StreamHandle,
    #[needs(job)]
    output: StreamHandle,
    key: [u8; KEY],
    image: Box<[u8; IMAGE]>,
    done: u32,
    unread: u32,
}

/// One job: its identity and the buffer it names on each stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Job {
    pub identity: u64,
    pub config: u32,
    pub input: u32,
    pub output: u32,
}

impl Job {
    pub fn encode(&self) -> [u8; JOB] {
        let mut out = [0; JOB];
        out[..8].copy_from_slice(&self.identity.to_le_bytes());
        out[8..12].copy_from_slice(&self.config.to_le_bytes());
        out[12..16].copy_from_slice(&self.input.to_le_bytes());
        out[16..].copy_from_slice(&self.output.to_le_bytes());
        out
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        let word = |at: usize| Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?));
        Some(Self {
            identity: u64::from_le_bytes(bytes.get(..8)?.try_into().ok()?),
            config: word(8)?,
            input: word(12)?,
            output: word(16)?,
        })
    }
}

/// What the driver makes of an image under a key.
pub fn made(image: &[u8], key: &[u8]) -> Vec<u8> {
    image
        .iter()
        .enumerate()
        .map(|(at, byte)| byte ^ key[at % key.len()])
        .collect()
}

impl Lifecycle for FilledDriver {
    type Needs = FilledDriverNeeds;
    const TICKS: bool = true;

    fn start(needs: FilledDriverNeeds, _: &DriverConfig<'_>) -> Result<Self> {
        Ok(Self {
            config: needs.config,
            input: needs.input,
            output: needs.output,
            key: [0; KEY],
            image: Box::new([0; IMAGE]),
            done: 0,
            unread: 0,
        })
    }

    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        self.take_free();
        let mut encoded = [0u8; JOB];
        while let Ok(Some(len)) = self.config.next_job(&mut encoded) {
            let Some(job) = Job::decode(&encoded[..len]) else {
                let _ = self.config.fault_consumer();
                break;
            };
            // Again for each job: its consumer may have given a buffer back and named it in
            // this job since the last look.
            self.take_free();
            self.run(&job, now_ns);
        }
        Some(now_ns.saturating_add(POLL_NS))
    }
}

impl FilledDriver {
    /// Holds every buffer the streams have free, so a job can name any of them.
    fn take_free(&mut self) {
        for stream in [&mut self.config, &mut self.input, &mut self.output] {
            while let Ok(Some(_)) = stream.next_buffer() {}
        }
    }

    fn run(&mut self, job: &Job, now_ns: u64) {
        // A buffer held since before this consumer came has nothing behind it: it is given up
        // and taken again, as the merged image drivers do, so the completion reaches it.
        if matches!(self.output.buffer_handle(job.output), Ok(None)) {
            let _ = self.output.publish(StreamFrame {
                buffer: job.output,
                bytes: 0,
                sequence: 0,
                identity: 0,
                timestamp_ns: 0,
            });
            while let Ok(Some(_)) = self.output.next_buffer() {}
        }
        // One read of each of the consumer's buffers; everything after uses the copies.
        let read = matches!(self.config.read(job.config, 0, &mut self.key), Ok(true))
            && matches!(self.input.read(job.input, 0, &mut self.image[..]), Ok(true));
        let mut bytes = 0;
        if read {
            for (at, byte) in self.image.iter_mut().enumerate() {
                *byte ^= self.key[at % KEY];
            }
            if matches!(self.output.write(job.output, 0, &self.image[..]), Ok(true)) {
                bytes = IMAGE as u32;
            }
        }
        if bytes == 0 {
            self.unread += 1;
        } else {
            self.done += 1;
        }
        let _ = self.output.publish(StreamFrame {
            buffer: job.output,
            bytes,
            sequence: u64::from(self.done),
            identity: job.identity,
            timestamp_ns: now_ns,
        });
    }
}

impl Filled for FilledDriver {
    fn done(&mut self) -> Result<u32> {
        Ok(self.done)
    }

    fn unread(&mut self) -> Result<u32> {
        Ok(self.unread)
    }
}

/// The service with the driver bound: `config` is shared memory, and `input` and `output` are
/// dma-bufs from `buffers` (with `heap` naming where, when it isn't the default).
pub fn start(buffers: Arc<dyn BufferSource>, heap: &str) -> Running {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "lemnosd-filled-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("sock");
    let text = format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
         [[devices]]\nid = \"f\"\ndriver = \"filled\"\n\
         [devices.streams.config]\nbuffers = {BUFFERS}\nbuffer_bytes = {KEY}\n\
         job_slots = 8\njob_bytes = {JOB}\n\
         [devices.streams.input]\nbuffers = {BUFFERS}\nbuffer_bytes = {IMAGE}\n\
         memory = \"dma-buf\"\n{heap}\
         [devices.streams.output]\nbuffers = {BUFFERS}\nbuffer_bytes = {IMAGE}\n\
         memory = \"dma-buf\"\n{heap}"
    );
    let config = Config {
        socket: socket.clone(),
        name: "test-host".to_string(),
        board: parse(&text).unwrap(),
        drivers: vec![entry::<FilledDriver>()],
        backend: Box::new(SimBoard::new()),
        options: Options::default(),
    };
    let service = Service::start_with_buffers(config, buffers).unwrap();
    Running::new(service, socket, dir)
}

fn platform() -> Arc<dyn StreamPlatform> {
    Arc::new(LinuxStreams)
}

/// The consumer of all three streams, as the contract has it take them: the two it fills
/// writable, the one the driver fills read-only.
pub struct App {
    pub session: Session,
    pub config: FrameStream,
    pub input: FrameStream,
    pub output: FrameStream,
}

impl App {
    pub fn attach(running: &Running) -> Self {
        let mut session = Session::connect(&running.socket, "consumer").unwrap();
        let config = session
            .open_stream_writable(DEVICE, "config", platform())
            .unwrap();
        let input = session
            .open_stream_writable(DEVICE, "input", platform())
            .unwrap();
        let output = session.open_stream(DEVICE, "output", platform()).unwrap();
        Self {
            session,
            config,
            input,
            output,
        }
    }

    /// Fills the job's configuration and input buffers, posts it, and checks that what comes
    /// back on `output` is what the driver makes of exactly those bytes.
    pub fn run(&mut self, job: Job, key: &[u8; KEY], image: &[u8], expected: &[u8]) {
        self.config.write(job.config, 0, key).unwrap();
        self.input.write(job.input, 0, image).unwrap();
        self.run_posted(job, expected);
    }

    /// Posts a job on buffers as they stand, and checks its output.
    pub fn run_posted(&mut self, job: Job, expected: &[u8]) {
        assert!(self.config.post_job(&job.encode()).unwrap());
        let frame = self
            .output
            .next_frame(Duration::from_secs(10))
            .unwrap()
            .expect("the job's output");
        assert_eq!(
            (frame.identity(), frame.buffer(), frame.len()),
            (job.identity, job.output, IMAGE)
        );
        assert!(
            self.output.bytes(&frame) == expected,
            "job {}",
            job.identity
        );
        self.output.release(frame).unwrap();
    }
}
