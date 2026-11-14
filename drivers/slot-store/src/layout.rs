//! The file names of one slot's boot directory on the Raspberry Pi firmware's boot partition. The
//! list itself is `contracts/slot-layout.toml` (version 3): the image builder fills the directory
//! from it, `lemnos-ctl tree --boot-dir` writes a copy beside the firmware's text, and `lemnos-ctl`'s
//! tests check these constants against that file. The generated `config.txt` and `tryboot.txt`
//! (`firmware.rs`) name the same files with the same spelling, and the installer and the every-boot
//! check read them from here.
//!
//! The names are relative to the slot's directory `slots/<id>/`, which `os_prefix` selects.

/// The kernel image the firmware loads (`kernel=`).
pub const KERNEL: &str = "kernel8.img";
/// The initramfs the firmware loads after the kernel (`initramfs ... followkernel`).
pub const INITRAMFS: &str = "initramfs.cpio";
/// The device tree blob the firmware loads instead of its own pick by revision (`device_tree=`).
/// It is the one built with this slot's kernel.
pub const DEVICE_TREE: &str = "board.dtb";
/// The kernel command line the firmware reads (`cmdline=`).
pub const CMDLINE: &str = "cmdline.txt";
/// The directory the firmware loads overlays from, relative to `os_prefix` (`overlay_prefix`).
pub const OVERLAY_DIR: &str = "overlays/";
/// The file that makes `os_prefix` apply to overlays: without it the firmware reads the shared
/// overlays instead (config_txt_boot.adoc, `overlay_prefix`).
pub const OVERLAY_README: &str = "overlays/README";
/// The overlay the board's generated device-tree overlay compiles to (`dtoverlay=`, which the
/// firmware resolves as `overlays/<name>.dtbo` under `os_prefix`).
pub const BOARD_OVERLAY: &str = "overlays/lemnos-board.dtbo";
/// The `dtoverlay=` name of [`BOARD_OVERLAY`].
pub const BOARD_OVERLAY_NAME: &str = "lemnos-board";

/// The files every `rpi-tryboot` slot must have in its directory, in the order the contract lists.
pub const RPI_TRYBOOT_REQUIRED: [&str; 6] = [
    KERNEL,
    INITRAMFS,
    DEVICE_TREE,
    CMDLINE,
    OVERLAY_README,
    BOARD_OVERLAY,
];

/// The names of the files a slot's directory is missing for the `rpi-tryboot` form, `Err` naming
/// them. `present` lists the names the bundle or the directory has (paths under the slot).
pub fn rpi_tryboot_missing(present: &[&str]) -> Result<(), Vec<&'static str>> {
    let missing: Vec<&'static str> = RPI_TRYBOOT_REQUIRED
        .iter()
        .copied()
        .filter(|name| !present.contains(name))
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(missing)
    }
}

#[cfg(test)]
#[path = "layout_tests.rs"]
mod tests;
