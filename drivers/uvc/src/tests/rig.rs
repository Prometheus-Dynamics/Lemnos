//! The driver bound against the mocks, and the packets to feed it.

use lemnos_contract_camera_frames::CameraFrames;
use lemnos_core::{
    Config, ConfigValue, Description, Driver, Item, LocalCaller, Provider, Resources, Result,
    Status, StreamHandle, Tag, UsbDeviceHandle,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use lemnos_core::ErrorKind;
use lemnos_hal::StreamEndpoint;
use lemnos_hal::blocks::{BufferHandle, BufferKind, StreamFrame};
use lemnos_hal::mock::MockStream;

use super::fixture;
use super::mock::{MockUsb, Packet};
use crate::Uvc;
use crate::session::STREAMING;

pub const FID: u8 = 0x01;
pub const EOF: u8 = 0x02;
pub const PTS_FLAG: u8 = 0x04;
pub const SCR_FLAG: u8 = 0x08;
pub const ERR: u8 = 0x40;

/// A payload with a plain header.
pub fn payload(flags: u8, body: &[u8]) -> Packet {
    let mut data = vec![2, flags];
    data.extend_from_slice(body);
    Packet::good(data)
}

/// A payload whose header carries a presentation time and source clock reference.
pub fn payload_timed(flags: u8, pts: u32, stc: u32, body: &[u8]) -> Packet {
    let mut data = vec![12, flags | PTS_FLAG | SCR_FLAG];
    data.extend_from_slice(&pts.to_le_bytes());
    data.extend_from_slice(&stc.to_le_bytes());
    data.extend_from_slice(&[0x34, 0x12]);
    data.extend_from_slice(body);
    Packet::good(data)
}

struct Board(Option<UsbDeviceHandle>, Option<StreamHandle>);

impl Provider for Board {
    fn provided(&self, _need: &str) -> bool {
        true
    }

    fn device(&mut self, _need: &str, _tag: Tag) -> Result<LocalCaller> {
        Err(Status::NOT_SUPPORTED)
    }

    fn usb_device(&mut self, _need: &str) -> Result<UsbDeviceHandle> {
        self.0.take().ok_or(Status::NOT_FOUND)
    }

    fn stream(&mut self, _need: &str) -> Result<StreamHandle> {
        self.1.take().ok_or(Status::NOT_FOUND)
    }
}

/// Binds the driver, with `frame_bytes` as its only setting.
pub fn bind(usb: UsbDeviceHandle, stream: StreamHandle, frame_bytes: i64) -> Result<Uvc> {
    let mut board = Board(Some(usb), Some(stream));
    let mut resources = Resources::new(&mut board, None);
    let config = [("frame_bytes", ConfigValue::Int(frame_bytes))];
    Uvc::bind(&mut resources, &Config::new(&config))
}

pub fn bind_config(config: &Config<'_>) -> Result<Uvc> {
    let usb = MockUsb::new(fixture::webcam().bytes);
    let mut board = Board(Some(Box::new(usb)), Some(Box::new(MockStream::new(1))));
    let mut resources = Resources::new(&mut board, None);
    Uvc::bind(&mut resources, config)
}

/// The mock stream, with a consumer that can come and go. While none is attached, `write`
/// answers `false` and puts nothing in the buffer, as the host's stream does.
#[derive(Clone)]
pub struct Consumer {
    stream: MockStream,
    attached: Arc<AtomicBool>,
}

impl StreamEndpoint for Consumer {
    type Error = ErrorKind;

    fn buffer_count(&self) -> u32 {
        self.stream.buffer_count()
    }

    fn buffer_kind(&self) -> BufferKind {
        self.stream.buffer_kind()
    }
    fn publish(&mut self, frame: StreamFrame) -> core::result::Result<(), ErrorKind> {
        self.stream.publish(frame)
    }
    fn next_buffer(&mut self) -> core::result::Result<Option<u32>, ErrorKind> {
        self.stream.next_buffer()
    }
    fn next_job(&mut self, out: &mut [u8]) -> core::result::Result<Option<usize>, ErrorKind> {
        self.stream.next_job(out)
    }
    fn write(
        &mut self,
        buffer: u32,
        offset: usize,
        data: &[u8],
    ) -> core::result::Result<bool, ErrorKind> {
        if !self.attached.load(Ordering::Relaxed) {
            return Ok(false);
        }
        self.stream.write(buffer, offset, data)
    }
    fn read(
        &mut self,
        buffer: u32,
        offset: usize,
        out: &mut [u8],
    ) -> core::result::Result<bool, ErrorKind> {
        self.stream.read(buffer, offset, out)
    }
    fn buffer_handle(
        &self,
        buffer: u32,
    ) -> core::result::Result<Option<BufferHandle<'_>>, ErrorKind> {
        if !self.attached.load(Ordering::Relaxed) {
            return Ok(None);
        }
        self.stream.buffer_handle(buffer)
    }

    fn fault_consumer(&mut self) -> core::result::Result<(), ErrorKind> {
        self.stream.fault_consumer()
    }
}

/// The driver, its device and its stream, at a simulated time. The test plays the consumer: it
/// takes published frames, copies what their buffers hold, and hands the buffers back.
pub struct Rig {
    pub uvc: Uvc,
    pub usb: MockUsb,
    pub stream: MockStream,
    /// Whether a consumer is attached to the stream.
    pub attached: Arc<AtomicBool>,
    pub now_ns: u64,
    /// Published frames taken from the stream and not yet looked at.
    taken: Vec<StreamFrame>,
    /// Their bytes, as the stream held them when they were taken.
    taken_bytes: Vec<Vec<u8>>,
}

impl Rig {
    /// The stream's buffers are `frame_bytes` long, as a board file sets them.
    pub fn new(descriptors: Vec<u8>, frame_bytes: i64, buffers: u32) -> Self {
        let buffer_bytes = usize::try_from(frame_bytes).unwrap();
        Self::with_buffers(descriptors, frame_bytes, buffers, buffer_bytes)
    }

    /// Stream buffers of `buffer_bytes`, which a board file can get wrong.
    pub fn with_buffers(
        descriptors: Vec<u8>,
        frame_bytes: i64,
        buffers: u32,
        buffer_bytes: usize,
    ) -> Self {
        let usb = MockUsb::new(descriptors);
        let stream = MockStream::with_bytes(buffers, buffer_bytes);
        let attached = Arc::new(AtomicBool::new(true));
        let consumer = Consumer {
            stream: stream.clone(),
            attached: Arc::clone(&attached),
        };
        let uvc = bind(Box::new(usb.clone()), Box::new(consumer), frame_bytes).unwrap();
        Self {
            uvc,
            usb,
            stream,
            attached,
            now_ns: 1_000,
            taken: Vec::new(),
            taken_bytes: Vec::new(),
        }
    }

    /// The standard camera, with a store that keeps frames.
    pub fn webcam(frame_bytes: i64) -> Self {
        Self::new(fixture::webcam().bytes, frame_bytes, 8)
    }

    /// Ticks at the current time.
    pub fn tick(&mut self) -> Option<u64> {
        lemnos_core::Lifecycle::tick(&mut self.uvc, self.now_ns)
    }

    /// Ticks until the driver goes quiet or waits for later, at most a bounded number of times.
    pub fn settle(&mut self) {
        for _ in 0..16 {
            match self.tick() {
                Some(next) if next <= self.now_ns => {}
                _ => return,
            }
        }
    }

    /// The camera, started on `mode` and streaming.
    pub fn streaming(frame_bytes: i64, mode: u16) -> Self {
        let mut rig = Self::webcam(frame_bytes);
        rig.settle();
        rig.uvc.select_format(mode).unwrap();
        rig.uvc.start().unwrap();
        rig.settle_streaming();
        rig
    }

    /// Runs up to the first isochronous transfer, without running one.
    pub fn settle_streaming(&mut self) {
        for _ in 0..16 {
            if self.uvc.session.state() == STREAMING {
                return;
            }
            self.tick();
        }
        panic!("never reached streaming: {}", self.uvc.session.state());
    }

    /// One isochronous transfer carrying `packets`.
    pub fn feed(&mut self, packets: Vec<Packet>) {
        self.usb.queue(packets);
        let calls = self.usb.state().iso_calls;
        self.tick();
        assert_eq!(self.usb.state().iso_calls, calls + 1, "no transfer ran");
    }

    /// Takes every frame published, copies its bytes out of its buffer and hands the buffer
    /// back.
    fn take_published(&mut self) {
        while let Some(frame) = self.stream.take_frame() {
            let mut bytes = self.stream.contents(frame.buffer);
            bytes.truncate(frame.bytes as usize);
            self.stream.return_buffer(frame.buffer);
            self.taken.push(frame);
            self.taken_bytes.push(bytes);
        }
    }

    /// Every frame published since this was last asked, in order.
    pub fn frames(&mut self) -> Vec<StreamFrame> {
        self.take_published();
        core::mem::take(&mut self.taken)
    }

    /// The bytes of every frame published since this was last asked, in order.
    pub fn contents(&mut self) -> Vec<Vec<u8>> {
        self.take_published();
        core::mem::take(&mut self.taken_bytes)
    }

    pub fn advance(&mut self, ns: u64) {
        self.now_ns += ns;
    }

    pub fn describe(&self) -> Vec<Item> {
        let mut description = Description::new();
        lemnos_core::Lifecycle::describe(&self.uvc, &mut description);
        description.items().copied().collect()
    }
}
