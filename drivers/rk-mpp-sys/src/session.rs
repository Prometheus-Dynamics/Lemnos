//! A session on the service node: one open file, the memory the kernel follows, and the calls.
//!
//! # What is issued
//!
//! [`ServiceSession::send`] (the driver's [`Session`]) issues a chain only when it is one of the
//! shapes `lemnos-driver-rk-mpp` builds, and refuses anything else with `EINVAL` before the device
//! is touched:
//!
//! - One message: `QUERY_HW_SUPPORT`, `QUERY_HW_ID` or `QUERY_CMD_SUPPORT` with a 4-byte answer
//!   (the kernel writes one `u32` whatever the size says); `INIT_CLIENT_TYPE` with a 4-byte client
//!   type of a block the session was opened for, once per session (the kernel would rebind a bound
//!   session without asking); `POLL_HW_FINISH` or `RESET_SESSION` with no data; `RELEASE_FD` with
//!   whole `u32` descriptors. Its flags are 0.
//! - A job, on a bound session: `INIT_TRANS_TABLE` (not empty), `SET_REG_WRITE`, then optionally
//!   `SET_REG_ADDR_OFFSET`, then optionally `SET_REG_READ` into a read-back area. The write and the
//!   read are runs of whole registers inside the bound block's registers and reaching its first;
//!   every table and offset index is a register inside the write. Every message but the last
//!   carries `MULTI_MSG`, the last `MULTI_MSG | LAST_MSG`, so the kernel stops at the last.
//! - A parameter job, on a session bound to a parameter block (the IEP2): `SET_REG_WRITE` of
//!   exactly the parameter structure's size at offset 0, then optionally `SET_REG_ADDR_OFFSET`,
//!   then `SET_REG_READ` of exactly the results' size into a read-back area. Every descriptor
//!   word of the structure holds 1 to 1023, every word the kernel uses as an index, a size, a
//!   count or a mode is in its range ([`lemnos_driver_rk_mpp::ParamFacts`]), every offset is
//!   keyed by a descriptor word's register, and every message carries `REG_NO_OFFSET` besides
//!   the chain's two flags. A register-file job is refused on such a session, and a parameter
//!   job on any other.
//!
//! Each message's size is the exact length of the memory it was built with (`size` and payload
//! are fixed when the driver builds the message, and a session cannot swap the payload). Commands
//! the driver never sends (`INIT_DRIVER_DATA`, `SET_RCB_INFO`, `SET_SESSION_FD`, `POLL_HW_IRQ`,
//! `TRANS_FD_TO_IOVA`, `SEND_CODEC_INFO`) are refused, and no flag past the chain's two (and a
//! parameter job's `REG_NO_OFFSET`) is ever set.
//!
//! # Memory
//!
//! See the crate docs: every `data_ptr` points into the session's own scratch area or one of its
//! read-back areas, never at the caller's memory. A read-back area named by a job is zeroed just
//! before the job is issued, so a task the kernel drops after accepting the call completes with
//! zeros, not an older task's registers.
//!
//! # Errors
//!
//! `EINTR` is retried. `ENOTTY` (the path is not the service node: a regular file, a pipe, another
//! device) comes back as [`io::ErrorKind::Unsupported`]. Everything else is the kernel's error.
//!
//! # Allocation
//!
//! Opening allocates (the scratch and read-back areas). Every call after that allocates nothing.

use std::fs::OpenOptions;
use std::io;
use std::os::fd::{AsRawFd, OwnedFd};
use std::path::Path;
use std::ptr::NonNull;

use lemnos_driver_rk_mpp::uapi::{Command, IOC_CFG_V1, MAX_MSG_NUM, MsgV1};
use lemnos_driver_rk_mpp::{Block, Message, Payload, READ_BACK_BYTES, READ_BACK_SLOTS, Session};

use crate::check::{self, footprint, refused};

/// Bytes of the session's scratch area: room for the largest job the driver builds (the largest
/// block's register write, which is 780 registers, a full translation table and a full offset list)
/// and more.
pub const DATA_BYTES: usize = 4096;

/// Memory the kernel uses during a call only.
struct Scratch {
    /// The chain, as the kernel reads it.
    wire: [MsgV1; MAX_MSG_NUM],
    /// Each message's bytes, at 8-byte steps.
    data: [u8; DATA_BYTES],
}

/// The read-back areas, one per job slot.
type AreaBlock = [[u8; READ_BACK_BYTES]; READ_BACK_SLOTS];

/// The read-back areas: one heap allocation, held by a raw pointer so that moving the session
/// neither moves nor reborrows it, and freed when this is dropped.
struct Areas(NonNull<AreaBlock>);

// SAFETY: `Areas` owns its allocation and nothing else refers to it. The kernel writes it only
// inside a call on the session's file, which takes `&mut` on the session, so moving the session to
// another thread moves the only way to reach it.
unsafe impl Send for Areas {}

impl Areas {
    fn new() -> Self {
        Self(NonNull::from(Box::leak(Box::new(
            [[0; READ_BACK_BYTES]; READ_BACK_SLOTS],
        ))))
    }

    /// The first byte of area `slot`, which is below [`READ_BACK_SLOTS`].
    fn base(&self, slot: usize) -> *mut u8 {
        debug_assert!(slot < READ_BACK_SLOTS);
        self.0
            .as_ptr()
            .cast::<u8>()
            .wrapping_add(slot * READ_BACK_BYTES)
    }

    /// Zeroes area `slot`, which is below [`READ_BACK_SLOTS`].
    fn zero(&mut self, slot: usize) {
        let at = self.base(slot);
        // SAFETY: `at` is the start of area `slot` (below `READ_BACK_SLOTS`, checked by the
        // caller), `READ_BACK_BYTES` long, inside the live allocation `self` owns. No reference to
        // it exists, and the kernel writes it only during a call, which cannot run meanwhile
        // (this takes `&mut self` through the session).
        unsafe { at.write_bytes(0, READ_BACK_BYTES) };
    }

    /// Copies the start of area `slot` into `into`.
    fn copy(&self, slot: usize, into: &mut [u8]) {
        let len = into.len().min(READ_BACK_BYTES);
        let at = self.base(slot);
        // SAFETY: `at` is the start of area `slot` (below `READ_BACK_SLOTS`, checked by the
        // caller) in the live allocation `self` owns, `len` is at most `READ_BACK_BYTES`, and
        // `into` is a distinct, exclusively borrowed slice of at least `len` bytes. No call is in
        // progress, so the kernel is not writing the area.
        unsafe { std::ptr::copy_nonoverlapping(at, into.as_mut_ptr(), len) };
    }
}

impl Drop for Areas {
    fn drop(&mut self) {
        // SAFETY: the pointer came from `Box::leak` in `Areas::new` and is freed only here, once.
        // The session declares its file before its areas, so the file is closed before this
        // runs: the kernel writes user memory only inside a call on that file, and closing it
        // copies nothing out (`mpp_dev_release`), so nothing writes the areas after this.
        drop(unsafe { Box::from_raw(self.0.as_ptr()) });
    }
}

/// One open of the service node: a kernel session, and the memory the kernel follows.
pub struct ServiceSession {
    /// Declared first: closed before the read-back areas are freed.
    fd: OwnedFd,
    scratch: Box<Scratch>,
    /// A bit per client type this session may bind to.
    allowed: u32,
    bound: Option<Block>,
    /// The played kernel the tests issue to instead of the file.
    #[cfg(test)]
    pub(crate) played: Option<crate::tests::Played>,
    /// Declared last: freed after the file is closed.
    areas: Areas,
}

impl std::fmt::Debug for ServiceSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServiceSession")
            .field("fd", &self.fd)
            .field("allowed", &self.allowed)
            .field("bound", &self.bound)
            .finish_non_exhaustive()
    }
}

impl ServiceSession {
    /// Opens `node` (the service node; `/dev/mpp_service` on the boards that have it) read-write
    /// and close-on-exec. The session may later bind to the blocks in `allowed` only: the ones the
    /// caller found behind an IOMMU (see [`crate::ServiceNode`]).
    pub fn open(node: &Path, allowed: &[Block]) -> io::Result<Self> {
        let file = OpenOptions::new().read(true).write(true).open(node)?;
        Ok(Self {
            fd: OwnedFd::from(file),
            scratch: Box::new(Scratch {
                wire: [MsgV1::default(); MAX_MSG_NUM],
                data: [0; DATA_BYTES],
            }),
            allowed: allowed
                .iter()
                .fold(0, |bits, b| bits | (1 << b.facts().client)),
            bound: None,
            #[cfg(test)]
            played: None,
            areas: Areas::new(),
        })
    }

    /// The block the session is bound to.
    #[must_use]
    pub fn bound(&self) -> Option<Block> {
        self.bound
    }

    /// The client types present, a bit each (`MPP_CMD_QUERY_HW_SUPPORT`).
    pub fn hw_support(&mut self) -> io::Result<u32> {
        self.call_u32(Command::QueryHwSupport, 0)
    }

    /// Binds the session to `block` (`MPP_CMD_INIT_CLIENT_TYPE`). Refused (`EINVAL`) without a
    /// call when the session is bound already or was not opened for `block`.
    pub fn bind(&mut self, block: Block) -> io::Result<()> {
        let client = block.facts().client;
        if self.bound.is_some() || self.allowed & (1 << client) == 0 {
            return Err(refused());
        }
        self.call_u32(Command::InitClientType, client)?;
        self.bound = Some(block);
        Ok(())
    }

    /// The bound block's hardware id (`MPP_CMD_QUERY_HW_ID`). Refused (`EINVAL`) without a call
    /// on an unbound session.
    pub fn hw_id(&mut self) -> io::Result<u32> {
        let block = self.bound.ok_or_else(refused)?;
        self.call_u32(Command::QueryHwId, block.facts().client)
    }

    /// Waits for the session's tasks and unmaps every buffer it mapped
    /// (`MPP_CMD_RESET_SESSION`).
    pub fn reset(&mut self) -> io::Result<()> {
        self.scratch.wire[0] = MsgV1 {
            cmd: Command::ResetSession.code(),
            ..MsgV1::default()
        };
        self.issue(1)
    }

    /// One message with a `u32` the kernel reads and may write back.
    fn call_u32(&mut self, cmd: Command, value: u32) -> io::Result<u32> {
        let scratch = &mut *self.scratch;
        scratch.data[..4].copy_from_slice(&value.to_ne_bytes());
        let at = scratch.data.as_mut_ptr().expose_provenance() as u64;
        scratch.wire[0] = MsgV1 {
            cmd: cmd.code(),
            flags: 0,
            size: 4,
            offset: 0,
            data_ptr: at,
        };
        self.issue(1)?;
        let d = &self.scratch.data;
        Ok(u32::from_ne_bytes([d[0], d[1], d[2], d[3]]))
    }

    /// Copies each payload into the scratch area and writes the chain, with every `data_ptr` in
    /// the session's memory. `messages` passed [`check::check`].
    fn stage(&mut self, messages: &[Message<'_>]) {
        let Scratch { wire, data } = &mut *self.scratch;
        let mut at = [0usize; MAX_MSG_NUM];
        let mut cursor = 0;
        for (m, at) in messages.iter().zip(&mut at) {
            let bytes: &[u8] = match m.payload() {
                Payload::In(d) => d,
                Payload::InOut(d) => d,
                Payload::None | Payload::ReadBack { .. } => continue,
            };
            data[cursor..cursor + bytes.len()].copy_from_slice(bytes);
            *at = cursor;
            cursor += footprint(bytes.len());
        }
        // Every pointer from one base, taken after the copies.
        let base = data.as_mut_ptr();
        for (i, m) in messages.iter().enumerate() {
            let ptr = match m.payload() {
                Payload::None => 0,
                Payload::In(_) | Payload::InOut(_) => {
                    base.wrapping_add(at[i]).expose_provenance() as u64
                }
                Payload::ReadBack { slot } => {
                    self.areas.zero(*slot);
                    self.areas.base(*slot).expose_provenance() as u64
                }
            };
            wire[i] = m.wire(ptr);
        }
    }

    /// Issues the first `n` messages of the staged chain as one `MPP_IOC_CFG_V1`.
    fn issue(&mut self, n: usize) -> io::Result<()> {
        debug_assert!((1..=MAX_MSG_NUM).contains(&n));
        #[cfg(test)]
        if let Some(played) = self.played.as_mut() {
            return played.issue(&self.scratch.wire[..n]);
        }
        let chain: *mut MsgV1 = self.scratch.wire.as_mut_ptr();
        loop {
            // SAFETY: `MPP_IOC_CFG_V1` reads a chain of `struct mpp_msg_v1` from its argument
            // until the message that ends it; `chain` is the session's own array of them, live and
            // exclusively borrowed for the call, and its first `n` were checked to end the chain
            // at the `n`th. Each `data_ptr` is 0 or points into the session's scratch area (live
            // for the call, `size` bytes, read or written only during it) or a read-back area
            // (live until after the file is closed, written at a later call on this file).
            let ret = unsafe { libc::ioctl(self.fd.as_raw_fd(), IOC_CFG_V1 as _, chain) };
            if ret >= 0 {
                return Ok(());
            }
            let err = io::Error::last_os_error();
            match err.raw_os_error() {
                Some(libc::EINTR) => {}
                Some(libc::ENOTTY) => return Err(io::ErrorKind::Unsupported.into()),
                _ => return Err(err),
            }
        }
    }

    /// The first byte of read-back area `slot`, for the tests.
    #[cfg(test)]
    pub(crate) fn area(&self, slot: usize) -> *mut u8 {
        self.areas.base(slot)
    }

    /// The scratch area's bytes, for the tests.
    #[cfg(test)]
    pub(crate) fn scratch_range(&self) -> std::ops::Range<usize> {
        let start = self.scratch.data.as_ptr().addr();
        start..start + DATA_BYTES
    }
}

impl Session for ServiceSession {
    fn send(&mut self, messages: &mut [Message<'_>]) -> io::Result<()> {
        let checked = check::check(messages, self.bound, self.allowed)?;
        self.stage(messages);
        self.issue(messages.len())?;
        // An answer comes back out of the scratch area into the message's own memory.
        let mut cursor = 0;
        for m in messages.iter_mut() {
            let len = match m.payload() {
                Payload::In(d) => d.len(),
                Payload::InOut(d) => d.len(),
                Payload::None | Payload::ReadBack { .. } => continue,
            };
            if let Some(out) = m.in_out_mut() {
                out.copy_from_slice(&self.scratch.data[cursor..cursor + len]);
            }
            cursor += footprint(len);
        }
        if let Some(block) = checked.binds {
            self.bound = Some(block);
        }
        Ok(())
    }

    fn read_back(&mut self, slot: usize, into: &mut [u8]) {
        if slot < READ_BACK_SLOTS {
            self.areas.copy(slot, into);
        } else {
            into.fill(0);
        }
    }
}
