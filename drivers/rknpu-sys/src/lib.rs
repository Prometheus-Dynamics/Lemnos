//! The `rknpu` driver's kernel interface: the node's layouts ([`uapi`]), the only requests the
//! driver sends, each from a checked constructor ([`request`]), an [`NpuSession`] on the vendor
//! kernel's NPU DRM node (a render node), and an [`NpuNode`] that checks the node is that
//! driver's and opens sessions. `lemnos-driver-rknpu` implements its `Device` and `Session`
//! traits on these two, so a `lemnos_driver_rknpu::Service<NpuNode>` reaches a real device.
//!
//! # What is issued
//!
//! Four requests, each from the checked builders in [`request`], each
//! argument a plain structure the kernel copies in and out during the call and holding no user
//! pointer:
//!
//! - `RKNPU_ACTION`, for the [`Query`] values only.
//! - `RKNPU_MEM_CREATE`, then `RKNPU_MEM_MAP` and `mmap` of the object on the same file. The
//!   object goes into the session's table with the kernel's address for it, which never leaves
//!   this crate.
//! - `RKNPU_MEM_DESTROY`, after the session's own mapping is gone (a mapping holds the object,
//!   and it stays mapped in its IOMMU domain for as long as anything holds it).
//! - `RKNPU_SUBMIT`, blocking. Its `task_obj_addr` is a kernel pointer the kernel dereferences
//!   without looking it up (`rknpu_job_alloc`, `rknpu_job_subcore_commit_pc`). The session fills
//!   it in itself, only from its table: the body must be an object this session made and still
//!   holds, made kernel-mapped, in the run's domain, with the run's tasks and task base inside
//!   the bytes asked for. Anything else is refused with `InvalidInput` before the call.
//!
//! `RKNPU_MEM_SYNC` is never sent (it takes a kernel pointer and an unchecked range, and only
//! cacheable objects need it; the driver makes none). Nothing is imported or exported.
//!
//! # Not a vendor library
//!
//! No vendor library is used or followed. The facts this crate relies on were read from
//! rockchip-linux/kernel `develop-6.1` at `77168c8d5ab82399f65a80e9f807b50ba37cf483`,
//! `drivers/rknpu/`: `rknpu_drv.c` (the DRM ioctl table, `rknpu_driver.driver.name`),
//! `rknpu_gem.c` (`rknpu_gem_create_ioctl`, `rknpu_gem_map_ioctl`, `rknpu_gem_destroy_ioctl`,
//! `rknpu_gem_object_destroy`) and `rknpu_job.c` (`rknpu_submit`). The layouts and request
//! numbers are in [`uapi`], pinned by the tests.
//!
//! Hardware status: untested on board. The check is `testing/board-runs/E32.md`.

#![cfg(target_os = "linux")]

pub mod node;
pub mod request;
pub mod session;
pub mod uapi;

#[cfg(test)]
mod tests;

pub use node::NpuNode;
pub use request::{Create, Domain, Kind, Object, Query, RUN_TIMEOUT_MS, Run, RunError};
pub use session::{MAX_OBJECTS, NpuSession};
