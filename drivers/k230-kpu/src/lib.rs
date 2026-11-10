//! The K230's KPU, the neural accelerator behind the kernel's `k230-gnne` node. No vendor library:
//! what runs on the block (a compiled program) is the caller's bytes, which Lemnos never reads,
//! parses or compiles. The vendor's user-space runtime (`libnncase`), its model tools and its
//! compiled format are not used, read or followed.
//!
//! - [`uapi`]: what the kernel offers for the block (a node with `open`, `release`, `poll` and
//!   `fasync` only, its register window and interrupt) and the memory zone node's requests and
//!   structure, pinned by `layout.rs` and by `layout-check/run.sh` against the C compiler.
//! - [`request`]: checked builders for the memory zone requests, each refusing the fields the
//!   kernel takes unchecked.
//! - [`confine`]: what the kernel lets a job reach, as read from its source, and the check that a
//!   job reaches only its own buffers.
//! - [`Kpu`]: the `npu/1` contract over a [`Device`], refusing to load programs while a job could
//!   reach past its buffers; the check backs off (250 ms doubling to an 8 s cap) and never gives
//!   up.
//!
//! # What is not here, and why
//!
//! No job runs. A program can name any address, so a job is confined only if something in front
//! of the block keeps it to that job's buffers while it runs. On the K230 nothing does: the block
//! has no IOMMU and the device tree describes no bus protection, so its addresses are physical.
//! The kernel driver takes no job at all; whatever runs one writes the block's registers from
//! user space through a mapping of physical memory, which the memory zone node gives for any
//! physical range. [`confine`] lists each fact with its source. Unlike a block behind a
//! translation unit, no kernel path of Lemnos's own closes this without reading the program, so
//! [`Npu::load`](lemnos_contract_npu::Npu::load) answers `NOT_SUPPORTED`, and no Linux
//! [`Device`] (and no `-sys` crate) is built: the tests run the driver and the checks against a
//! model of the block's reach as the sources give it.
//!
//! Hardware status: untested on board. Written from the kernel's source; the check is
//! `testing/board-runs/E34.md`.
//!
//! Provenance: new. Every fact is from the Kendryte K230 Linux SDK (`github.com/kendryte/
//! k230_linux_sdk`) at commit `fc6cc2151d54cb307be47f81815023f7ffb0eae6`, whose
//! `buildroot-overlay/configs/k230_canmv_defconfig` builds `github.com/ruyisdk/linux-xuantie-kernel`
//! at `7d4e1f444f461dbe3833bd99a4640e7b6c2cd529` with its `k230_defconfig`: from that kernel,
//! `drivers/misc/canaan/gnne/k230_gnne.c`, `drivers/misc/canaan/mmz/mmz.c`,
//! `arch/riscv/boot/dts/canaan/k230.dtsi` and `arch/riscv/configs/k230_defconfig` (all
//! `GPL-2.0`); each module names its files. Interface facts only are transcribed; no code is
//! copied. The shape follows `drivers/vip9000`.

#![forbid(unsafe_code)]

pub mod confine;
mod driver;
pub mod request;
pub mod uapi;

mod layout;

#[cfg(test)]
mod tests;

pub use confine::{Facts, Gaps, Range, Table, check_reach};
pub use driver::{Device, Kernel, Kpu, MAX_REACH, State, backoff_ns};
pub use request::{Allocated, RequestError};
