//! The VeriSilicon VGLite 2.5D GPU (GCNanoUltraV) through its vendor kernel's `/dev/vg_lite`:
//! resize, crop, flips and quarter turns, fill, blend, palette expansion, strokes of polylines,
//! and one-pixel lines (a width-1 stroke), between byte buffers or dma-bufs.
//!
//! No vendor library: the command streams of the vendor's user library (`VGLite/vg_lite.c`,
//! `vg_lite_path.c` and `vg_lite_stroke.c` of `kendryte/k230_linux_sdk` at `fc6cc21`, Vivante,
//! MIT) are ported to Rust and handed to the kernel through `lemnos-driver-vglite-sys`, which
//! holds the `unsafe` and builds every stream through typed, checked methods. This crate has
//! no `unsafe`.
//!
//! ```no_run
//! use lemnos_driver_vglite::{Filter, Format, Image, Vglite};
//! let mut gpu = Vglite::new(lemnos_driver_vglite::DEVICE);
//! let (src, dst) = (Image::packed(Format::L8, 640, 480), Image::packed(Format::L8, 320, 240));
//! let (from, mut to) = (vec![0u8; src.frame_len()], vec![0u8; dst.frame_len()]);
//! let now_ns = 0; // the caller's clock, for the backoff after a failure
//! let cost = gpu.resize_bytes(now_ns, src, &from, dst, &mut to, Filter::Area)?;
//! assert!(cost.staged_bytes > 0);
//! # Ok::<(), lemnos_driver_vglite::RunError>(())
//! ```
//!
//! - **Strict:** what the unit cannot do as asked is [`std::io::ErrorKind::Unsupported`]: area
//!   resizes other than 2x (VGLite has no averaging filter), premultiplied blends, formats the
//!   vendor's build for this chip leaves out (24-bit RGB, planar YUV input), L8 lines and fills,
//!   and strokes with round caps or joins. No colour key and no ROP exist on this GPU.
//! - **Memory:** the GPU reads and writes only staging buffers its kernel driver allocates. The
//!   kernel can map a dma-buf too, but only by the address of its last scatter-list entry, which
//!   is wrong for any dma-buf that is not one physically contiguous block, and user space cannot
//!   tell which it is; so every byte buffer and dma-buf is copied through staging, and every call
//!   says how many bytes it copied ([`Transfer`]).
//! - **Failures:** [`Vglite`] opens the GPU when it is needed, and after a failure closes it and
//!   reopens it with a backoff doubling from 250 ms to 8 s. It never gives up and never turns
//!   itself off.
//! - **Lines:** the stroke port (`stroke.rs`) is the vendor's `vg_lite_update_stroke` for straight
//!   open and closed polylines with BUTT and SQUARE caps and MITER and BEVEL joins; round caps and
//!   joins, curves, arcs and dashes are not ported. [`Vglite::stroke_bytes`] draws one;
//!   a line is its square-capped width-1 stroke from pixel centre to pixel centre, not proven
//!   equal to Bresenham on board (`tests/lines.rs`).
//!
//! - **Driver:** [`VgliteDriver`] serves `image.scaler/1` and `image.convert/1`, each from its
//!   own stream of dma-bufs; [`serve`] runs the jobs and lists every check a job's bytes pass
//!   through. A conversion is an identity blit with the source and target formats apart, among
//!   [`CONVERTIBLE`]. See [`driver`] for the board file and what is not provided.
//!
//! # What is checked
//!
//! Every word of a stream that a caller influences becomes an address, a stride, a size, a format
//! or a mode the GPU uses; `lemnos-driver-vglite-sys` lists each and its check. Here: every image
//! is checked against the bytes or the dma-buf that hold it (the dma-buf's real size, not the
//! caller's claim), each side within the GPU's 16-bit fields, and rectangles inside their frame,
//! before anything is allocated; staging buffers are sized from the checked image; and the
//! stream checks those buffers again as it names them.
//!
//! # Status
//!
//! Untested on board. The streams are compared word for word with the vendor library's own
//! output for small fixtures, recorded on a host by the dev branch's oracle (the library built
//! unmodified with its kernel request replaced by a recorder). Register values and command
//! encodings inferred rather than read from a source that states them are marked unverified
//! where they are defined. The board checks are `testing/board-runs/E3.md` and, for the driver,
//! `testing/board-runs/H3c.md`. A conversion's stream is the recorded copy with only its format
//! words changed (`tests.rs`), not itself recorded from the vendor library.
//!
//! Provenance: ported from the dev branch (`crates/lemnos-drivers-vglite`, and
//! `crates/lemnos-linux-sys/src/vg_lite.rs` for the `-sys` crate). Changes from dev: no dma-buf
//! is mapped in place (see Memory above); the stream has no raw-word or raw-register writes, and
//! its formats and mode words are typed and checked; the GPU reopens with a backoff instead of
//! keeping the first open's error for the life of the process; the resize filter and transform
//! types are this crate's own, so it does not depend on the RGA driver.

#![cfg(target_os = "linux")]
#![forbid(unsafe_code)]

mod command;
pub mod driver;
mod gpu;
mod plan;
pub mod serve;
mod stroke;
mod surface;

pub use driver::{VgliteDriver, VgliteDriverNeeds};
pub use gpu::{MAX_STROKE_POINTS, RunError, State, Stroke, Vglite, backoff_ns};
pub use lemnos_driver_vglite_sys::DEVICE;
pub use plan::{BlendMode, CONVERTIBLE, Filter, Format, Image, MAX_SIDE, Transform};
pub use serve::Gpu;
pub use stroke::{Cap, Join};
pub use surface::{Frame, Transfer};

#[cfg(test)]
mod stroke_tests;
#[cfg(test)]
mod tests;

#[cfg(test)]
#[global_allocator]
static ALLOCATOR: lemnos_measure::test_alloc::Counting = lemnos_measure::test_alloc::Counting;
