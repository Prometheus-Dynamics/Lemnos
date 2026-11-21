//! The firmware-layout check: a boot partition image read the way the Pi firmware reads it, and one
//! verdict line per rule for every file the firmware loads.
//!
//! A view is one way the firmware reaches a directory: `config.txt` on a normal boot, `tryboot.txt`
//! on a trial boot, or `config.txt` with `os_prefix` set to a named directory. The firmware text
//! is followed as the firmware does (`include`, the section filters the caller says are on,
//! `os_prefix`), and then every file it loads is checked:
//!
//! - **names**: no `kernel=`, `cmdline=` or `device_tree=`: the firmware's default names only.
//! - **kernel**: a gzip kernel is one member that ends exactly at the end of the file, its CRC and
//!   its ISIZE right, and the decompressed image carries the arm64 Image magic. The firmware sizes
//!   the output from the file's last four bytes, so padding after the stream breaks the boot.
//! - **device tree**: each default name the caller lists is there, its FDT `totalsize` equals the
//!   file size, and it carries `__symbols__`.
//! - **overlays**: `overlays/README`, `overlay_map.dtb` and `hat_map.dtb` under the prefix; each
//!   loaded or required overlay is exact (`totalsize` equals the file size) and applies to every
//!   default device tree (each fixup label and each target path resolves).
//! - **cmdline**: one line, at most [`CMDLINE_MAX`] bytes, nothing after the newline.
//! - **initramfs**: plain cpio ending in its trailer, or gzip, zstd or xz ending at the end of the
//!   file.
//! - **case**: each file's directory entry shows the name exactly as the firmware asks for it (a
//!   long name, or the 8.3 name's lowercase flags), as Linux vfat writes it.
//!
//! [`RULE_VERSION`] names this rule set: a bundle records the version it was checked at.

mod config;
mod fdt;
mod stream;
mod view;

#[cfg(test)]
mod tests;

pub mod sample;
pub use config::{Filters, Settings, Source};
pub use view::{Finding, Report, Request, check};

/// The version of the rule set. A change to what passes is a new version.
pub const RULE_VERSION: u32 = 1;

/// The longest command line a bay may carry, in bytes, newline included (arm64 takes 2048; the
/// rest is the firmware's own additions).
pub const CMDLINE_MAX: usize = 1600;
