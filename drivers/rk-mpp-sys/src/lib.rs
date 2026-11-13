//! The `rk-mpp` driver's unsafe layer, and nothing else: a [`ServiceSession`] on the vendor
//! kernel's codec service node (`/dev/mpp_service` on the boards that have it; the path is always
//! the caller's), and a [`ServiceNode`] that opens sessions and answers the driver's questions
//! about the system. Both implement `lemnos-driver-rk-mpp`'s traits, so a
//! `lemnos_driver_rk_mpp::Service<ServiceNode>` reaches a real device.
//!
//! # The call
//!
//! The service has one request, `MPP_IOC_CFG_V1`, whose argument is a chain of 24-byte messages,
//! each with a `data_ptr` the kernel follows. The session issues only chains of the shapes the
//! driver builds, after checking each message (see [`session`] for the full list), and points
//! every `data_ptr` at memory the session owns:
//!
//! - **Bytes the kernel reads or writes during the call** (register values, the translation
//!   table, offsets, a query's answer) are copied into the session's scratch area first, and an
//!   answer is copied back out after the call. The caller's slices are never handed to the kernel.
//! - **Register read-backs**, which the kernel writes at a *later* call (the poll that finishes
//!   the task), point at one of the session's read-back areas. The areas are one heap allocation
//!   made with the session and held by a raw pointer, so moving the session never moves them; they
//!   are freed only when the session is dropped, after its file is closed (the fields are declared
//!   so). The kernel writes user memory only inside a call on this file, and every call needs
//!   `&mut` on the session, so no Rust reference to an area exists while the kernel writes it,
//!   and no safe caller can free or move an area while a task may still write it.
//!
//! # Not a vendor library
//!
//! Vendor user-space code is governed by the block rule: register facts may be transcribed from open-source vendor user-space code with provenance (source file and version); the libraries themselves are neither used nor linked. The facts this crate relies on (which commands read or
//! write how many bytes at `data_ptr`, when read-backs are copied, what closing the file does) were
//! read from rockchip-linux/kernel `develop-6.1` at `77168c8d5ab82399f65a80e9f807b50ba37cf483`:
//! `drivers/video/rockchip/mpp/mpp_common.c` (`mpp_process_request`, `mpp_collect_msgs`,
//! `mpp_wait_result_default`, `mpp_dev_release`), the per-block `*_result` functions, and the
//! platform driver names in `mpp_vdpu1.c`, `mpp_vdpu2.c`, `mpp_vepu1.c`, `mpp_vepu2.c`,
//! `mpp_jpgdec.c` and `mpp_jpgenc.c`. Interface facts only; no code is copied. The message layout
//! and request number are `lemnos-driver-rk-mpp`'s, pinned again by this crate's tests.
//!
//! Hardware status: untested on board. The check is `testing/board-runs/E16.md`.

#![cfg(target_os = "linux")]

mod check;
pub mod node;
pub mod session;

#[cfg(test)]
mod tests;

pub use node::{ServiceNode, driver_name};
pub use session::{DATA_BYTES, ServiceSession};
