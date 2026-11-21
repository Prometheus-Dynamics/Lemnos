//! Boot files shaped as the Pi firmware reads them, for the tests that build a boot partition: the
//! image builder runs the firmware-layout check on every image and refuses one that fails, so a
//! stand-in payload must pass it the way a real build does (`lemnos_firmware_check::sample`). Each
//! test binary that builds an image includes this file with `#[path]`.
#![allow(dead_code)]

use lemnos_firmware_check::sample;
use std::path::{Path, PathBuf};

/// The command line every stand-in slot boots with unless a test gives its own.
pub const CMDLINE: &[u8] = b"console=serial0,115200 root=lemnos\n";

/// Writes the five build roles into `payload` (created): a gzip arm64 kernel whose Image is
/// `kernel_len` bytes, a newc cpio initramfs, a device tree with `__symbols__`, `cmdline`, and a
/// board overlay that applies to the tree.
pub fn write_payload(payload: &Path, cmdline: &[u8], kernel_len: usize) {
    std::fs::create_dir_all(payload).unwrap();
    std::fs::write(payload.join("kernel"), sample::kernel(kernel_len)).unwrap();
    std::fs::write(payload.join("initramfs"), sample::cpio()).unwrap();
    std::fs::write(payload.join("device-tree"), sample::base_dtb()).unwrap();
    std::fs::write(payload.join("cmdline"), cmdline).unwrap();
    std::fs::write(
        payload.join("board-overlay"),
        sample::overlay("/soc", "gpio"),
    )
    .unwrap();
}

/// Writes the firmware files a board declares into `firmware` (created): the overlay and HAT maps,
/// and each of `overlays` as `<name>.dtbo`, an overlay that applies to the stand-in tree.
pub fn write_firmware(firmware: &Path, overlays: &[String]) {
    std::fs::create_dir_all(firmware).unwrap();
    std::fs::write(firmware.join("overlay_map.dtb"), sample::map_dtb()).unwrap();
    std::fs::write(firmware.join("hat_map.dtb"), sample::map_dtb()).unwrap();
    for name in overlays {
        std::fs::write(
            firmware.join(format!("{name}.dtbo")),
            sample::overlay("/soc", "gpio"),
        )
        .unwrap();
    }
}

/// The firmware directory beside a payload directory (`<payload>/../firmware`).
pub fn firmware_beside(payload: &Path) -> PathBuf {
    payload.parent().unwrap().join("firmware")
}

/// `args` with `--firmware <payload>/../firmware` added when they give `--payload` and no
/// `--firmware`, and that directory exists: the stand-in board's maps go with every build.
pub fn with_firmware(args: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    if args.contains(&"--firmware") {
        return out;
    }
    let Some(at) = args.iter().position(|a| *a == "--payload") else {
        return out;
    };
    let Some(payload) = args.get(at + 1) else {
        return out;
    };
    let firmware = firmware_beside(Path::new(payload));
    if firmware.is_dir() {
        out.push("--firmware".into());
        out.push(firmware.display().to_string());
    }
    out
}
