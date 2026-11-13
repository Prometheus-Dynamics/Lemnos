//! The ISP600 as `camera.isp/1` and `isp.isp600/1`: [`Isp600`] runs the vendor kernel's `vin`
//! camera driver's nodes through `lemnos-v4l2` and the `-sys` crate, carrying each job's load
//! image in and its statistics and image out. See the `driver` module's documentation (on
//! [`Isp600`]) for the streams, the job steps, the settings and the failure rule.
//!
//! The same shape as the PiSP driver: the block's own configuration and statistics layouts are
//! exposed and versioned, and nothing between them is interpreted. 3A and tuning stay with the
//! caller, in the camera stack; no vendor library, tuning file or default table is used, read or
//! mirrored.
//!
//! What it stands on, each with no device access of its own. [`layout`] is the library half:
//! built with no features (no `device`), it needs no driver, engine or daemon.
//!
//! - [`layout`]: the load image's and the statistics buffer's regions, the register offsets and
//!   bits the vendor kernel names, and [`layout::check_load`], which every load image passes
//!   before the kernel sees it. Checked at compile time against the C compiler's reading of the
//!   vendor headers.
//! - [`tuning`]: the register encodings the vendor headers define for the blocks a caller may set
//!   (channel black level and bit depths, module enables, temporal denoise's record enable),
//!   written into a load image through [`tuning::Image`]. Every other tuning block is not here:
//!   see "Tuning blocks without encodings" below.
//! - [`job`]: the checks a caller's [`IspJob`](lemnos_contract_camera_isp::IspJob) passes before
//!   any of its numbers is used, and the bounded copy of statistics and images out.
//! - [`hw`]: the kernel's nodes as the driver sees them ([`Hardware`]), and the Linux
//!   implementation over the capture node and the ISP and statistics subdevices.
//!
//! # Where a caller's bytes go
//!
//! Every path by which a caller's bytes reach the kernel or the block:
//!
//! - A job's indices (config buffer, output buffer) are checked against the buffers the streams
//!   made and the driver holds ([`job::check`], then the held masks); they never reach the kernel.
//! - A load image is read once from a held config buffer into the driver's own
//!   [`layout::LOAD_BYTES`] bytes (the stream refuses a read past the buffer), then
//!   [`layout::check_load`] refuses any image whose address words or extent words (the input and
//!   valid sizes and start, and the lossless group that sets the block's write extents) are not
//!   zero. The kernel copies exactly those bytes: the `-sys` crate builds the pointer and length
//!   from the driver's array.
//!   The kernel writes the block's load, save and save-load addresses itself, and the temporal
//!   denoise addresses and the frame sizes into each image; the vendor source names no other
//!   register that holds an address or sets how far the block writes, and the block's
//!   statistics land in buffers the kernel sized ([`layout::SAVE_BYTES`]) whatever the image
//!   holds. The rest of the register image and the tables are the caller's.
//! - Statistics are copied by the kernel into the driver's own [`layout::STATISTICS_BYTES`]
//!   bytes, with that length as the limit the kernel checks; the length it reports is checked
//!   again against the array, then written into a held stream buffer through the stream, which
//!   refuses a range past the buffer.
//! - Images come from the kernel's own mapped buffers; each plane is bounded by the mapping and
//!   all of them together by the stream buffer's agreed size.
//!
//! # Tuning blocks without encodings
//!
//! The vendor kernel's headers define the module enables, the channel black level and bit depths,
//! the temporal denoise's record enable and the lossless group, and nothing else of the tuning
//! blocks. The vendor user-space library (`radxa/allwinner-isp6xx`, see `docs/drivers.md`) has
//! the sizes and offsets of every table and statistics part, but the register words it writes for
//! white balance, colour correction, gamma, tone mapping, lens shading, spatial denoise, sharpen
//! and the statistics windows are computed in a prebuilt binary (`libisp_algo.so`), whose
//! source is not in the repository. Its statistics decode is in the same binary. So none of these
//! is encoded or decoded here, and the driver does not claim their output. A caller that sets them
//! does so with the words it already has.
//!
//! Untested on board: written from the vendor driver's interface facts; the checks are
//! `testing/board-runs/E13.md`, `testing/board-runs/U3.md` and `testing/board-runs/U4.md`.

#![forbid(unsafe_code)]

#[cfg(all(feature = "device", target_os = "linux"))]
mod driver;
#[cfg(all(feature = "device", target_os = "linux"))]
pub mod hw;
#[cfg(feature = "device")]
pub mod job;
pub mod layout;
pub mod tuning;

#[cfg(all(test, feature = "device", target_os = "linux"))]
mod driver_tests;
#[cfg(all(test, feature = "device"))]
mod job_tests;
#[cfg(test)]
mod library_tests;
#[cfg(test)]
mod tests;

#[cfg(all(feature = "device", target_os = "linux"))]
pub use driver::{
    CONFIG_STREAM, DEFAULT_SETTLE, Isp600, Isp600Needs, MAX_DIMENSION, MAX_SETTLE,
    MAX_STREAM_BUFFERS, MIN_DIMENSION, OUTPUT_STREAM, Options, STATISTICS_STREAM,
};
#[cfg(all(feature = "device", target_os = "linux"))]
pub use hw::{Hardware, LinuxHardware, Settings};
