//! The VeriSilicon VGLite 2.5D GPU (GCNanoUltraV) on the Kendryte K230 and K230D, through
//! `/dev/vg_lite` directly: resize, crop, flip and quarter turns, fill, blend and palette
//! expansion, between byte buffers or dmabufs: the fixed-function variant of the image ops.
//!
//! The command streams are the vendor library's (`VGLite/vg_lite.c` of the K230 Linux SDK
//! v0.6.1, MIT), ported to Rust; no vendor library is loaded. Lines are strokes: a width-1
//! square-capped stroke drawn by the GPU.
//!
//! ```no_run
//! # #[cfg(target_os = "linux")] {
//! use lemnos_drivers_vglite::{Filter, Format, Image, Vglite};
//! let gpu = Vglite::load()?;
//! let (src, dst) = (Image::packed(Format::L8, 640, 480), Image::packed(Format::L8, 320, 240));
//! let (from, mut to) = (vec![0u8; src.frame_len()], vec![0u8; dst.frame_len()]);
//! let cost = gpu.resize_bytes(src, &from, dst, &mut to, Filter::Area)?;
//! eprintln!("{} bytes staged", cost.staged_bytes);
//! # }
//! # Ok::<(), std::io::Error>(())
//! ```
//!
//! Status: untested on board. Nothing here has run on a K230 or K230D: the host tests check the
//! command words against the vendor's own output and the device tests skip without
//! `/dev/vg_lite`. Register values and command encodings that were inferred rather than read
//! from a source that states them are marked unverified where they are defined.
//!
//! - **Strict:** what the unit cannot do as asked is [`std::io::ErrorKind::Unsupported`]: area
//!   resizes other than 2x (VGLite has no averaging filter), premultiplied blends, formats the
//!   K230's build leaves out (24-bit RGB, planar YUV input), L8 lines and fills, and strokes with
//!   round caps or joins. No colour key and no ROP exist on this GPU.
//! - **Memory:** the kernel driver maps dmabufs only, and only physically contiguous ones
//!   correctly. Byte buffers, [`Backing::Scattered`] dmabufs and rows whose stride VGLite cannot
//!   take go through a contiguous staging buffer; every call returns the bytes it copied
//!   ([`Transfer`]).
//! - **Capabilities:** `lemnos_drivers_rga::caps` lists the K230D's VGLite route, each operation
//!   `Unmeasured` until a board run.
#![forbid(unsafe_code)]

#[cfg(target_os = "linux")]
mod command;
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod plan;
#[cfg(target_os = "linux")]
mod stroke;
#[cfg(all(test, target_os = "linux"))]
mod stroke_tests;
#[cfg(all(test, target_os = "linux"))]
mod tests;

pub use lemnos_drivers_rga::{Filter, Transform};
pub use plan::{BlendMode, Format, Image};

#[cfg(target_os = "linux")]
mod gpu;
#[cfg(target_os = "linux")]
mod surface;

#[cfg(target_os = "linux")]
pub use gpu::Vglite;
#[cfg(target_os = "linux")]
pub use surface::{Backing, Frame, Transfer};
