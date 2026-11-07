//! The RP1 CSI-2 receiver through Lemnos's own module, `/dev/lemnos-rp1-csi2` (feature
//! `csi2-module`; `kernel-modules/lemnos`), beside the stock `rp1-cfe` nodes, which stay
//! the default. The module binds only where the device's `driver_override` names it, after
//! `rp1-cfe` is unbound, and it only receives: the sensor is set up from user space
//! ([`super::cci`]).
//!
//! [`Receiver`] is what a capture loop needs; [`Csi2Module`] is the board's and [`MockReceiver`]
//! a simulator's, which writes frames from a function into the queued buffers at the frame rate.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::time::{Duration, Instant};

use lemnos_linux_sys::lemnos_rp1_csi2 as sys;
pub use lemnos_linux_sys::lemnos_rp1_csi2::{DEPTH, Done, Info, PACK_BYTES, Start, UNPACK16};
use lemnos_linux_sys::mmap::Mapping;
use lemnos_linux_sys::poll::poll_one;
use lemnos_linux_sys::time::monotonic_now;

use super::{DeviceError, Result};

/// The module's device node.
pub const PATH: &str = "/dev/lemnos-rp1-csi2";

/// A CSI-2 receiver: started with the sensor's link and stream, fed one buffer per frame.
pub trait Receiver {
    /// The receiver's info.
    fn info(&mut self) -> Result<Info>;
    /// Starts receiving.
    fn start(&mut self, start: &Start) -> Result<()>;
    /// Lends `buf` (from byte `offset`) for one frame; `tag` comes back in its [`Done`].
    fn queue(&mut self, buf: BorrowedFd<'_>, offset: u32, tag: u64) -> Result<()>;
    /// Stops; buffers not yet read come back with `-ECANCELED`.
    fn stop(&mut self) -> Result<()>;
    /// Waits up to `timeout` for finished buffers and reads them into `out`; 0 on timeout.
    fn wait_done(&mut self, out: &mut [Done], timeout: Duration) -> Result<usize>;
}

/// An open `/dev/lemnos-rp1-csi2`; one open file at a time.
pub struct Csi2Module {
    file: File,
    info: Info,
}

impl Csi2Module {
    /// Opens [`PATH`].
    pub fn open() -> Result<Self> {
        Self::open_path(PATH)
    }

    /// Opens the module's node at `path`, non-blocking, and checks it speaks [`sys::ABI`].
    pub fn open_path(path: impl AsRef<Path>) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path)?;
        let info = sys::info(file.as_fd())?;
        if info.abi != sys::ABI {
            return Err(DeviceError::Config(format!(
                "lemnos-rp1-csi2 speaks ABI {}, this crate {}",
                info.abi,
                sys::ABI
            )));
        }
        Ok(Self { file, info })
    }

    /// The info read at open.
    pub fn opened_info(&self) -> Info {
        self.info
    }
}

impl AsFd for Csi2Module {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.file.as_fd()
    }
}

impl Receiver for Csi2Module {
    fn info(&mut self) -> Result<Info> {
        Ok(sys::info(self.file.as_fd())?)
    }

    fn start(&mut self, start: &Start) -> Result<()> {
        Ok(sys::start(self.file.as_fd(), start)?)
    }

    fn queue(&mut self, buf: BorrowedFd<'_>, offset: u32, tag: u64) -> Result<()> {
        Ok(sys::queue(self.file.as_fd(), buf, offset, tag)?)
    }

    fn stop(&mut self) -> Result<()> {
        Ok(sys::stop(self.file.as_fd())?)
    }

    fn wait_done(&mut self, out: &mut [Done], timeout: Duration) -> Result<usize> {
        match sys::read_done(self.file.as_fd(), out) {
            Ok(n) => return Ok(n),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
            Err(e) => return Err(e.into()),
        }
        if poll_one(self.file.as_fd(), libc::POLLIN, Some(timeout))? == 0 {
            return Ok(0);
        }
        match sys::read_done(self.file.as_fd(), out) {
            Ok(n) => Ok(n),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(0),
            Err(e) => Err(e.into()),
        }
    }
}

/// Writes frame `sequence` into `frame` (the start's `size` bytes).
pub type FrameFn = Box<dyn FnMut(u32, &mut [u8]) + Send>;

struct Queued {
    map: Mapping,
    tag: u64,
}

/// A receiver without hardware: after [`Receiver::start`], one frame per `period`, written by
/// the frame function into the oldest queued buffer (any mappable descriptor: a dma-buf, a
/// memfd). A frame start with no buffer queued is counted in [`Info::dropped`], as the module
/// does. Time stamps are `CLOCK_MONOTONIC`, the frame end `exposure` after the start.
pub struct MockReceiver {
    period: Duration,
    fill: FrameFn,
    /// The stream, when it started, and the monotonic clock then (ns): frame times are taken
    /// from that one pair, so they keep their spacing however late `advance` runs.
    start: Option<(Start, Instant)>,
    mono0: Duration,
    queued: Vec<Queued>,
    done: Vec<Done>,
    frames: u32,
    dropped: u32,
}

impl MockReceiver {
    /// A receiver delivering a frame every `period`, each written by `fill`.
    pub fn new(period: Duration, fill: FrameFn) -> Self {
        Self {
            period,
            fill,
            start: None,
            mono0: Duration::ZERO,
            queued: Vec::new(),
            done: Vec::new(),
            frames: 0,
            dropped: 0,
        }
    }

    /// Frames that fill every buffer with `byte`.
    pub fn constant(period: Duration, byte: u8) -> Self {
        Self::new(period, Box::new(move |_, f| f.fill(byte)))
    }

    /// Runs the frames due by now.
    fn advance(&mut self) {
        let Some((start, t0)) = self.start else {
            return;
        };
        let height = start.height.max(1);
        let read_out = self.period.mul_f64(0.9) / height;
        let now = Instant::now();
        while t0 + self.period * (self.frames + 1) <= now {
            let fs = t0 + self.period * (self.frames + 1);
            let fs_ns = (self.mono0 + (fs - t0)).as_nanos() as u64;
            let sequence = self.frames;
            self.frames += 1;
            if self.queued.is_empty() {
                self.dropped += 1;
                continue;
            }
            let mut q = self.queued.remove(0);
            (self.fill)(sequence, q.map.as_mut_slice());
            self.done.push(Done {
                tag: q.tag,
                fs_ns,
                fe_ns: fs_ns + (read_out * height).as_nanos() as u64,
                sequence,
                status: 0,
            });
        }
    }

    fn next_frame(&self) -> Option<Instant> {
        self.start
            .map(|(_, t0)| t0 + self.period * (self.frames + 1))
    }
}

impl Receiver for MockReceiver {
    fn info(&mut self) -> Result<Info> {
        self.advance();
        let mut info = Info {
            abi: sys::ABI,
            max_lanes: 2,
            depth: DEPTH as u32,
            frames: self.frames,
            dropped: self.dropped,
            ..Info::default()
        };
        info.streaming = u32::from(self.start.is_some());
        Ok(info)
    }

    fn start(&mut self, start: &Start) -> Result<()> {
        if self.start.is_some() {
            return Err(io::Error::from_raw_os_error(libc::EBUSY).into());
        }
        if start.size == 0 || !start.size.is_multiple_of(16) || !start.stride.is_multiple_of(16) {
            return Err(io::Error::from_raw_os_error(libc::EINVAL).into());
        }
        self.start = Some((*start, Instant::now()));
        self.mono0 = monotonic_now();
        self.frames = 0;
        self.dropped = 0;
        Ok(())
    }

    fn queue(&mut self, buf: BorrowedFd<'_>, offset: u32, tag: u64) -> Result<()> {
        let Some((start, _)) = self.start else {
            return Err(io::Error::from_raw_os_error(libc::ENODATA).into());
        };
        self.advance();
        if self.queued.len() + self.done.len() >= DEPTH {
            return Err(io::Error::from_raw_os_error(libc::EBUSY).into());
        }
        if !offset.is_multiple_of(16) {
            return Err(io::Error::from_raw_os_error(libc::EINVAL).into());
        }
        let owned: OwnedFd = buf.try_clone_to_owned()?;
        let map = Mapping::new(owned.as_fd(), start.size as usize, u64::from(offset), true)?;
        self.queued.push(Queued { map, tag });
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        self.advance();
        let now = monotonic_now().as_nanos() as u64;
        for q in self.queued.drain(..) {
            self.done.push(Done {
                tag: q.tag,
                fs_ns: now,
                fe_ns: 0,
                sequence: 0,
                status: -libc::ECANCELED,
            });
        }
        self.start = None;
        Ok(())
    }

    fn wait_done(&mut self, out: &mut [Done], timeout: Duration) -> Result<usize> {
        let deadline = Instant::now() + timeout;
        loop {
            self.advance();
            if !self.done.is_empty() || out.is_empty() {
                let n = self.done.len().min(out.len());
                for (o, d) in out.iter_mut().zip(self.done.drain(..n)) {
                    *o = d;
                }
                return Ok(n);
            }
            let now = Instant::now();
            let wake = match self.next_frame() {
                Some(t) if !self.queued.is_empty() => t.min(deadline),
                _ => deadline,
            };
            if now >= deadline {
                return Ok(0);
            }
            std::thread::sleep(wake.saturating_duration_since(now));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An unnamed temporary file of `len` bytes: mappable as a dma-buf is.
    fn memfd(len: usize) -> OwnedFd {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_TMPFILE | libc::O_CLOEXEC)
            .open(std::env::temp_dir())
            .unwrap();
        file.set_len(len as u64).unwrap();
        file.into()
    }

    fn start() -> Start {
        Start {
            lanes: 2,
            mbps: 800,
            dt: 0x2b,
            width: 16,
            height: 4,
            stride: 32,
            size: 128,
            ..Start::default()
        }
    }

    #[test]
    fn the_mock_writes_queued_buffers_in_order_at_its_rate() {
        let mut rx = MockReceiver::new(
            Duration::from_millis(5),
            Box::new(|seq, f| f.fill(seq as u8 + 1)),
        );
        assert!(
            rx.queue(memfd(128).as_fd(), 0, 1).is_err(),
            "queue before start"
        );
        rx.start(&start()).unwrap();
        let bufs: Vec<OwnedFd> = (0..3).map(|_| memfd(128)).collect();
        for (i, b) in bufs.iter().enumerate() {
            rx.queue(b.as_fd(), 0, i as u64 + 10).unwrap();
        }
        let mut done = [Done::default(); 4];
        let mut got = Vec::new();
        while got.len() < 3 {
            let n = rx.wait_done(&mut done, Duration::from_secs(1)).unwrap();
            assert!(n > 0, "timed out");
            got.extend_from_slice(&done[..n]);
        }
        assert_eq!(got.iter().map(|d| d.tag).collect::<Vec<_>>(), [10, 11, 12]);
        assert!(got.windows(2).all(|w| w[1].fs_ns > w[0].fs_ns));
        assert!(got.iter().all(|d| d.status == 0 && d.fe_ns > d.fs_ns));
        for (d, b) in got.iter().zip(&bufs) {
            let map = Mapping::new(b.as_fd(), 128, 0, false).unwrap();
            assert!(map.as_slice().iter().all(|v| *v == d.sequence as u8 + 1));
        }
        std::thread::sleep(Duration::from_millis(12));
        let info = rx.info().unwrap();
        assert!(info.dropped >= 1 && info.frames >= 4, "{info:?}");
    }

    #[test]
    fn stop_returns_the_queued_buffers_cancelled() {
        let mut rx = MockReceiver::constant(Duration::from_secs(10), 0);
        rx.start(&start()).unwrap();
        let b = memfd(128);
        rx.queue(b.as_fd(), 0, 7).unwrap();
        rx.stop().unwrap();
        let mut done = [Done::default(); 2];
        assert_eq!(rx.wait_done(&mut done, Duration::ZERO).unwrap(), 1);
        assert_eq!((done[0].tag, done[0].status), (7, -libc::ECANCELED));
        assert_eq!(rx.info().unwrap().streaming, 0);
    }
}
