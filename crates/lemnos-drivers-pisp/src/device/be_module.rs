//! The back end through Lemnos's own module, `/dev/lemnos-pisp-be` (feature `be-module`;
//! `kernel-modules/lemnos`), instead of the `pispbe` nodes. Opt-in: the stock V4L2
//! path ([`super::BeImageDevice`], [`super::BackEndStream`]) stays the default, and the module
//! binds only where the device's `driver_override` names it, after `pispbe` is unbound.
//!
//! A job is a [`BeTilesConfig`] and one dma-buf per image; the module maps them, queues the job
//! and returns a [`Done`] record per finished job with the interrupt's `CLOCK_MONOTONIC` time.
//! Several jobs can be in flight ([`Info::queue_depth`]). Measured on a board for one op, an
//! NV12 downscale one job at a time (docs/kernel-module.md, section 5); the Bayer path, TDN and
//! stitch buffers and several jobs in flight are untested on board.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::time::Duration;

use lemnos_linux_sys::lemnos_pisp_be::{self as sys, PLANES};
pub use lemnos_linux_sys::lemnos_pisp_be::{Done, Info, Recover, RecoverMode};

use super::{DeviceError, OUTPUT_STRIDE_ALIGN, Result};
use crate::be::ops::BeOp;
use crate::format::{compute_stride_align, num_planes, plane_size};
use crate::uapi::{BeTilesConfig, ImageFormatConfig};

/// The module's device node.
pub const PATH: &str = "/dev/lemnos-pisp-be";

/// Input rows at a 64-byte stride: the back end reads any 16-byte-aligned one.
const INPUT_STRIDE_ALIGN: u32 = 64;

/// An open `/dev/lemnos-pisp-be`; one open file at a time per block.
pub struct BeModule {
    file: File,
    info: Info,
}

/// An op's config ready to submit, and the sizes and plane offsets of its images (all planes
/// of an image in one dma-buf, one after another).
pub struct BeModuleJob {
    cfg: Box<BeTilesConfig>,
    input: ImageFormatConfig,
    outputs: [Option<ImageFormatConfig>; 2],
}

/// The bytes of `f`'s planes and each plane's offset: planes back to back.
fn layout(f: &ImageFormatConfig) -> (usize, [u32; 3]) {
    let mut offsets = [0; 3];
    let mut at = 0;
    for (p, off) in offsets.iter_mut().enumerate().take(num_planes(f.format)) {
        *off = at as u32;
        at += plane_size(f, p);
    }
    (at, offsets)
}

impl BeModuleJob {
    /// Prepares `op` for the module: strides chosen here (input 64 bytes, outputs
    /// [`OUTPUT_STRIDE_ALIGN`]), written into it, and the config built.
    pub fn new(op: &mut BeOp) -> Result<Self> {
        let mut input = op.input;
        compute_stride_align(&mut input, INPUT_STRIDE_ALIGN);
        let mut out = op.out;
        compute_stride_align(&mut out, OUTPUT_STRIDE_ALIGN);
        op.set_formats(input, out);
        let mut outputs = [None; 2];
        outputs[op.output] = Some(out);
        if let Some(mut out1) = op.out1 {
            compute_stride_align(&mut out1, OUTPUT_STRIDE_ALIGN);
            op.set_out1_format(out1);
            outputs[1] = Some(out1);
        }
        let cfg = op
            .backend
            .prepare()
            .map_err(|e| DeviceError::Config(e.to_string()))?;
        Ok(Self {
            cfg,
            input,
            outputs,
        })
    }

    /// The bytes an input dma-buf must hold.
    pub fn input_len(&self) -> usize {
        layout(&self.input).0
    }

    /// The bytes output `n`'s dma-buf must hold, or `None` when the op does not write it.
    pub fn output_len(&self, n: usize) -> Option<usize> {
        self.outputs.get(n)?.as_ref().map(|f| layout(f).0)
    }

    /// The config as built.
    pub fn config(&self) -> &BeTilesConfig {
        &self.cfg
    }

    /// The config to submit, for a test that changes it (the module's checks then apply).
    pub fn config_mut(&mut self) -> &mut BeTilesConfig {
        &mut self.cfg
    }
}

impl BeModule {
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
            return Err(DeviceError::Setup(format!(
                "lemnos-pisp-be speaks ABI {}, this crate {}",
                info.abi,
                sys::ABI
            )));
        }
        Ok(Self { file, info })
    }

    /// The module's info as read at open, with `dead` read now.
    pub fn info(&self) -> Result<Info> {
        Ok(sys::info(self.file.as_fd())?)
    }

    /// Jobs that may be in flight at once.
    pub fn queue_depth(&self) -> usize {
        self.info.queue_depth as usize
    }

    /// Submits `job` reading `input` and writing `outputs` (one dma-buf per output the op
    /// writes, `None` for the other), returning at once; its [`Done`] carries `tag`.
    pub fn submit(
        &self,
        job: &BeModuleJob,
        tag: u64,
        input: BorrowedFd<'_>,
        outputs: [Option<BorrowedFd<'_>>; 2],
    ) -> Result<()> {
        let mut planes = [None; PLANES];
        let (_, offs) = layout(&job.input);
        for p in 0..num_planes(job.input.format) {
            planes[sys::PLANE_INPUT + p] = Some((input, offs[p]));
        }
        for (n, fd) in outputs.into_iter().enumerate() {
            let (Some(fd), Some(f)) = (fd, &job.outputs[n]) else {
                continue;
            };
            let (_, offs) = layout(f);
            for p in 0..num_planes(f.format) {
                planes[sys::plane_output(n) + p] = Some((fd, offs[p]));
            }
        }
        Ok(sys::submit(
            self.file.as_fd(),
            job.cfg.as_bytes(),
            tag,
            &planes,
        )?)
    }

    /// Waits up to `timeout` for finished jobs and returns those that are (in completion order);
    /// empty when none finished in time.
    pub fn wait(&self, timeout: Duration) -> Result<Vec<Done>> {
        let fd = self.file.as_fd();
        let mut out = vec![Done::default(); self.queue_depth().max(1)];
        let mut n = read_some(fd, &mut out)?;
        if n == 0 {
            lemnos_linux_sys::poll::poll_one(fd, lemnos_linux_sys::poll::POLLIN, Some(timeout))?;
            n = read_some(fd, &mut out)?;
        }
        out.truncate(n);
        Ok(out)
    }

    /// Tries one soft recovery of a stopped block.
    pub fn recover(&self, mode: RecoverMode) -> Result<Recover> {
        Ok(sys::recover(self.file.as_fd(), mode)?)
    }
}

impl AsFd for BeModule {
    /// The node, for a caller's own poll loop (readable when a job finished).
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.file.as_fd()
    }
}

fn read_some(fd: BorrowedFd<'_>, out: &mut [Done]) -> Result<usize> {
    match sys::read_done(fd, out) {
        Ok(n) => Ok(n),
        Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(0),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::be::ops::{Pixels, downscale};

    #[test]
    fn a_downscale_job_lays_out_its_images_at_the_module_strides() {
        let mut op = downscale(Pixels::Grey8, (640, 480), 2).unwrap();
        let job = BeModuleJob::new(&mut op).unwrap();
        assert_eq!(job.input.stride, 640);
        assert_eq!(job.input_len(), 640 * 480 * 3 / 2); // NV12: the chroma after the luma
        assert_eq!(job.output_len(1), Some(512 * 240));
        assert_eq!(job.output_len(0), None);
        assert_eq!(job.config().config.output_format[1].image.stride, 512);
    }

    #[test]
    fn a_missing_node_is_an_error_not_a_panic() {
        assert!(BeModule::open_path("/nonexistent/lemnos-pisp-be").is_err());
    }
}
