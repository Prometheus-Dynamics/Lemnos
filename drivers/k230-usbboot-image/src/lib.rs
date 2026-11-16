//! The stage image, as a static library: the entry, the stage and the probe, from
//! `lemnos-driver-k230-usbboot-sys`. `check.sh` links it at the load address with `image.ld` and checks
//! the link. Empty on the host: the image is bare-metal riscv only.

#![no_std]

#[cfg(all(target_arch = "riscv64", target_os = "none"))]
extern crate lemnos_driver_k230_usbboot_sys;
