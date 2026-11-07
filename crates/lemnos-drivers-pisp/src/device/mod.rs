//! Running the PiSP nodes through `lemnos-v4l2` (feature `device`).
//!
//! - [`FrontEndDevice`]: the `rp1-cfe` media graph with the sensor → `csi2` → `pisp-fe`
//!   path, per-frame config buffers on `rp1-cfe-fe_config`, statistics from
//!   `rp1-cfe-fe_stats` and raw frames from `rp1-cfe-fe_image0`.
//! - [`BackEndDevice`]: one `pispbe` node group, memory to memory: `pispbe-input` →
//!   `pispbe-output0`, driven by `pispbe-config`.
//! - [`BackEndStream`]: a node group for a stream: dma-buf input (the front end's raw
//!   buffers), two outputs, a config per job, output buffers held until released.

mod be;
mod be_image;
#[cfg(feature = "be-module")]
pub mod be_module;
mod be_output;
mod be_spare;
mod be_stream;
mod be_tdn;
#[cfg(feature = "csi2-module")]
pub mod cci;
mod config_buf;
#[cfg(feature = "debug-hooks")]
mod config_dump;
#[cfg(feature = "csi2-module")]
pub mod csi2_module;
mod slots;
#[cfg(not(feature = "debug-hooks"))]
mod config_dump {
    //! Debug hooks off (feature `debug-hooks`): the config dumps and the patch do nothing.
    use crate::uapi::BeTilesConfig;

    pub(super) fn be_config(_bytes: &[u8]) {}

    pub(super) fn fe_config(_bytes: &[u8]) {}

    pub(super) fn be_patch(_cfg: &BeTilesConfig) -> Option<Box<BeTilesConfig>> {
        None
    }
}
mod fe;
mod line_timer;
pub mod profile;
mod queue;

use std::fmt;
use std::io;
use std::os::fd::BorrowedFd;

pub use be::{BackEndDevice, BeOutput};
pub use be_image::BeImageDevice;
pub use be_output::OutputMemory;
pub use be_stream::{
    BackEndStream, BeFormat, BeJob, BeOutputSetup, OUTPUT_STRIDE_ALIGN, QUEUED_JOBS, QueuedJob,
    output_stride,
};
pub use config_buf::ConfigMemory;
pub use fe::{
    FeFrame, FeHeld, FeHeldRaw, FrontEndDevice, FrontEndSetup, HeldImage, LineEvent, SensorWindow,
};
pub use line_timer::{LATE_BOUNDS_US, LineTimerCounts};
pub use queue::Queue;

/// A device error.
#[derive(Debug)]
#[non_exhaustive]
pub enum DeviceError {
    /// A V4L2 or media call failed.
    Kernel(lemnos_v4l2::Error),
    /// A dma-heap or dma-buf call failed.
    Io(io::Error),
    /// The graph or a format is not what we need.
    Setup(String),
    /// A config was refused by the builder.
    Config(String),
    /// Nothing arrived in time.
    Timeout(&'static str),
    /// Every buffer of this output is held by the caller (frames consumers still hold): the
    /// job was not queued. Releasing one lets the next job run.
    OutputsHeld(usize),
}

impl fmt::Display for DeviceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Kernel(e) => write!(f, "{e}"),
            Self::Io(e) => write!(f, "dma buffer: {e}"),
            Self::Setup(s) => write!(f, "setup: {s}"),
            Self::Config(s) => write!(f, "config: {s}"),
            Self::Timeout(what) => write!(f, "timed out waiting for {what}"),
            Self::OutputsHeld(i) => write!(f, "output {i}: every buffer is held"),
        }
    }
}

impl std::error::Error for DeviceError {}

impl From<lemnos_v4l2::Error> for DeviceError {
    fn from(e: lemnos_v4l2::Error) -> Self {
        Self::Kernel(e)
    }
}

impl From<io::Error> for DeviceError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// Result alias.
pub type Result<T> = std::result::Result<T, DeviceError>;

/// Brackets a CPU access to a dma-buf: `begin` before the CPU touches its mapping, not `begin`
/// after (`DMA_BUF_IOCTL_SYNC`). Invalidates a cached buffer before a read and flushes it after
/// a write.
pub(crate) fn dma_sync(
    fd: BorrowedFd<'_>,
    access: lemnos_linux::dma::Access,
    begin: bool,
) -> io::Result<()> {
    let phase = if begin {
        lemnos_linux::dma::Phase::Begin
    } else {
        lemnos_linux::dma::Phase::End
    };
    lemnos_linux::dma::sync(fd, phase, access)
}

/// Media devices whose driver is `driver` (e.g. `rp1-cfe`, `pispbe`), in path order.
pub use lemnos_v4l2::media::find_media_by_driver as find_media;

/// `V4L2_PIX_FMT_PISP_COMP1_*`: rp1-cfe's 8-bit compressed (mode 1) fourcc for a Bayer order.
pub fn comp1_fourcc(order: crate::uapi::BayerOrder) -> lemnos_v4l2::FourCc {
    use crate::uapi::BayerOrder::*;
    lemnos_v4l2::FourCc::new(match order {
        Rggb => b"PC1R",
        Gbrg => b"PC1g",
        Bggr => b"PC1B",
        Grbg => b"PC1G",
        Greyscale => b"PC1M",
    })
}

/// `V4L2_PIX_FMT_SBGGR16` and friends: 16-bit Bayer fourcc for a PiSP Bayer order.
pub fn bayer16_fourcc(order: crate::uapi::BayerOrder) -> lemnos_v4l2::FourCc {
    use crate::uapi::BayerOrder::*;
    lemnos_v4l2::FourCc::new(match order {
        Rggb => b"RG16",
        Gbrg => b"GB16",
        Bggr => b"BYR2",
        Grbg => b"GR16",
        Greyscale => b"Y16 ",
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::uapi::BayerOrder::*;

    #[test]
    fn compressed_fourccs_match_rp1_cfe() {
        let names = [Rggb, Gbrg, Bggr, Grbg, Greyscale].map(|o| comp1_fourcc(o).to_string());
        assert_eq!(names, ["PC1R", "PC1g", "PC1B", "PC1G", "PC1M"]);
    }
}
