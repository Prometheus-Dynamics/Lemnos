//! The RGA driver's unsafe layer, and nothing else:
//!
//! - [`request`]: the vendor kernel's multi-RGA request on `/dev/rga` (`RGA_BLIT_SYNC` with
//!   `struct rga_req`). A request names its images by dma-buf descriptors only, each checked to
//!   hold the whole image it describes before the request is sent.
//! - `rvv` (riscv64 only): RISC-V vector (RVV 1.0) kernels for the rows of the CPU route, each a
//!   safe function that bounds every access by the slices it is given.
//!
//! Provenance: ported from the dev branch (`crates/lemnos-linux-sys/src/rga.rs` and
//! `crates/lemnos-linux-sys/src/rvv/`). The request's layout is transcribed from the Apache-2.0
//! headers of librga (see [`request`]); no GPL or vendor code is copied.

#![cfg(target_os = "linux")]

pub mod request;
#[cfg(target_arch = "riscv64")]
pub mod rvv;
