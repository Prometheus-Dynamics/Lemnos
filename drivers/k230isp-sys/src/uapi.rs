//! The `vvcam-isp` misc device's requests and the structures they carry, transcribed from
//! `buildroot-overlay/package/vvcam/isp/vvcam_isp.h` and `vvcam_event.h` of the vendor SDK
//! (`k230_linux_sdk` at `fc6cc21`; VeriSilicon, dual MIT and GPL, used under MIT). Interface facts
//! only; no code is copied. The numbers are the asm-generic `_IOC` encodings (riscv, arm, x86),
//! pinned by the tests and by `layout-check/check.c` against the C compiler.
//!
//! Requests the device has and this crate does not issue:
//!
//! - `VVCAM_ISP_FE_START` (`_IOW('v', 0x08, isp_fe_buffer_t)`): points the front end at a command
//!   list by bus address (`cmd_dma_addr`), which the block then reads and runs. Nothing can tie
//!   that address to memory a caller holds from user space, so it is left out.
//! - `VVCAM_ISP_CLOSE_IRQ` (`_IOW('v', 0x07, uint32_t)`): turns the ISP's interrupt lines off.
//! - `VVCAM_ISP_RESET` ([`RESET`]): the request number is kept for the layout test; the driver does
//!   not reset the block, which would drop whatever programmed its frame path.
//! - `mmap` of the register window: it would give unchecked access to every register.

use core::mem::size_of;

const NRSHIFT: u32 = 0;
const TYPESHIFT: u32 = 8;
const SIZESHIFT: u32 = 16;
const DIRSHIFT: u32 = 30;
const WRITE: u32 = 1;
const READ: u32 = 2;

/// `_IOC(dir, type, nr, size)`, asm-generic.
const fn ioc(dir: u32, ty: u8, nr: u8, size: usize) -> u32 {
    assert!(size < (1 << 14));
    (dir << DIRSHIFT)
        | ((ty as u32) << TYPESHIFT)
        | ((nr as u32) << NRSHIFT)
        | ((size as u32) << SIZESHIFT)
}

/// `VVCAM_ISP_IOC_MAGIC`.
pub const MAGIC: u8 = b'v';

/// `vvcam_isp_reg_t`: one register's offset in the window and its value (8 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RegPair {
    /// The register's offset within the window.
    pub addr: u32,
    /// The value read or written.
    pub value: u32,
}

/// `vvcam_subscription_t`: an event type and id to queue on this file (8 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Subscription {
    /// [`EVENT_IRQ_TYPE`].
    pub kind: u32,
    /// [`EID_ISP_MIS`].
    pub id: u32,
}

/// `vvcam_event_t`: one queued interrupt (24 bytes on a 64-bit target: the union is one `u32`
/// and the timestamp is aligned to 8).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Event {
    /// `type`: [`EVENT_IRQ_TYPE`].
    pub kind: u32,
    /// The event id: [`EID_ISP_MIS`] for the ISP's interrupt.
    pub id: u32,
    /// `irqevent.irq_value`: the masked interrupt status the handler read and cleared.
    pub irq_value: u32,
    /// The kernel's `ktime_get_ns()` when the handler ran (`CLOCK_MONOTONIC`).
    pub timestamp: u64,
}

/// `VVCAM_EVENT_IRQ_TYPE`.
pub const EVENT_IRQ_TYPE: u32 = 0;
/// `VVCAM_EID_ISP_MIS`: the ISP core's interrupt, carrying `ISP_MIS`.
pub const EID_ISP_MIS: u32 = 0x00;
/// `VVCAM_ISP_EVENT_ELEMS` (`vvcam_isp_driver.h`): events the kernel keeps per subscription; past
/// that it drops the oldest.
pub const EVENT_ELEMS: usize = 10;

/// `VVCAM_ISP_RESET`: `_IOW('v', 0x01, uint32_t)`. Not issued.
pub const RESET: u32 = ioc(WRITE, MAGIC, 0x01, size_of::<u32>());
/// `VVCAM_ISP_READ_REG`: `_IOWR('v', 0x02, vvcam_isp_reg_t)`.
pub const READ_REG: u32 = ioc(READ | WRITE, MAGIC, 0x02, size_of::<RegPair>());
/// `VVCAM_ISP_WRITE_REG`: `_IOW('v', 0x03, vvcam_isp_reg_t)`.
pub const WRITE_REG: u32 = ioc(WRITE, MAGIC, 0x03, size_of::<RegPair>());
/// `VVCAM_ISP_SUBSCRIBE_EVENT`: `_IOW('v', 0x04, vvcam_subscription_t)`.
pub const SUBSCRIBE_EVENT: u32 = ioc(WRITE, MAGIC, 0x04, size_of::<Subscription>());
/// `VVCAM_ISP_UNSUBSCRIBE_EVENT`: `_IOW('v', 0x05, vvcam_subscription_t)`.
pub const UNSUBSCRIBE_EVENT: u32 = ioc(WRITE, MAGIC, 0x05, size_of::<Subscription>());
/// `VVCAM_ISP_DQEVENT`: `_IOR('v', 0x06, vvcam_event_t)`. Fails with `ENOENT` when no event is
/// queued; it never blocks.
pub const DQEVENT: u32 = ioc(READ, MAGIC, 0x06, size_of::<Event>());
