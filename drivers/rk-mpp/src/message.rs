//! Messages as this crate builds them, before the session layer points them at memory.
//!
//! A [`Message`] is made only here, so a [`crate::Session`] can rely on what it says: the size is
//! the length of the slice it carries, or for [`Payload::ReadBack`] at most
//! [`READ_BACK_BYTES`]; the flags are only [`flags::MULTI_MSG`] and [`flags::LAST_MSG`], and on
//! every message of a parameter job ([`crate::ParamJob`]) [`flags::REG_NO_OFFSET`] as well; and
//! the command is never one of those this crate refuses to send.

use crate::block::MAX_REGISTERS;
#[cfg(any(test, feature = "device", feature = "played"))]
use crate::uapi::{Command, MsgV1, flags};

/// Bytes of one read-back area: the most registers a block has.
pub const READ_BACK_BYTES: usize = 4 * MAX_REGISTERS;

/// Read-back areas a session keeps, one for each task in flight.
pub const READ_BACK_SLOTS: usize = 4;

/// What a message's `data_ptr` points at.
#[derive(Debug)]
pub enum Payload<'a> {
    /// No memory; `size` is 0.
    None,
    /// Bytes the kernel reads during the call.
    In(&'a [u8]),
    /// Bytes the kernel reads and writes during the call.
    InOut(&'a mut [u8]),
    /// The session's own read-back area `slot`, which the kernel writes `size` bytes into when the
    /// task is polled, in a later call on the same session. The area must stay where it is for as
    /// long as the session is open.
    ReadBack {
        /// Below [`READ_BACK_SLOTS`].
        slot: usize,
    },
}

/// One message of an `MPP_IOC_CFG_V1` chain.
#[cfg(any(test, feature = "device", feature = "played"))]
#[derive(Debug)]
pub struct Message<'a> {
    cmd: Command,
    flags: u32,
    offset: u32,
    size: u32,
    payload: Payload<'a>,
}

#[cfg(any(test, feature = "device", feature = "played"))]
#[cfg_attr(not(feature = "device"), allow(dead_code))]
impl<'a> Message<'a> {
    fn new(cmd: Command, offset: u32, payload: Payload<'a>, size: usize) -> Self {
        Self {
            cmd,
            flags: 0,
            offset,
            // Every payload here is below a few kilobytes (the largest is a register file).
            size: u32::try_from(size).unwrap_or(u32::MAX),
            payload,
        }
    }

    /// A message the kernel reads `data` of during the call.
    pub(crate) fn input(cmd: Command, offset: u32, data: &'a [u8]) -> Self {
        let size = data.len();
        Self::new(cmd, offset, Payload::In(data), size)
    }

    /// A message the kernel reads and writes `data` of during the call.
    pub(crate) fn in_out(cmd: Command, data: &'a mut [u8]) -> Self {
        let size = data.len();
        Self::new(cmd, 0, Payload::InOut(data), size)
    }

    /// A message with no payload.
    pub(crate) fn bare(cmd: Command) -> Self {
        Self::new(cmd, 0, Payload::None, 0)
    }

    /// A register read of `bytes` from byte `offset`, landing in read-back area `slot`.
    pub(crate) fn read_back(offset: u32, bytes: usize, slot: usize) -> Self {
        debug_assert!(bytes <= READ_BACK_BYTES && slot < READ_BACK_SLOTS);
        Self::new(
            Command::SetRegRead,
            offset,
            Payload::ReadBack { slot },
            bytes,
        )
    }

    /// Chains `messages` into one call: every one but the last says another follows.
    pub(crate) fn chain(messages: &mut [Self]) {
        Self::chain_with(messages, 0);
    }

    /// [`Self::chain`] for a parameter job: every message also says that a descriptor word is
    /// a whole descriptor, its offset in the offset list ([`flags::REG_NO_OFFSET`]).
    pub(crate) fn chain_params(messages: &mut [Self]) {
        Self::chain_with(messages, flags::REG_NO_OFFSET);
    }

    fn chain_with(messages: &mut [Self], extra: u32) {
        let n = messages.len();
        for (i, message) in messages.iter_mut().enumerate() {
            message.flags = extra
                | if n == 1 {
                    0
                } else if i + 1 == n {
                    flags::MULTI_MSG | flags::LAST_MSG
                } else {
                    flags::MULTI_MSG
                };
        }
    }

    /// A message of any command, payload and offset, chained with `flags` as given: for the
    /// tests of a session layer, which must refuse what the driver never builds.
    #[cfg(feature = "played")]
    #[must_use]
    pub fn forged(cmd: Command, flags: u32, offset: u32, payload: Payload<'a>, size: u32) -> Self {
        Self {
            cmd,
            flags,
            offset,
            size,
            payload,
        }
    }

    /// The command.
    #[must_use]
    pub fn cmd(&self) -> Command {
        self.cmd
    }

    /// The flags.
    #[must_use]
    pub fn flags(&self) -> u32 {
        self.flags
    }

    /// The byte offset (register reads and writes).
    #[must_use]
    pub fn offset(&self) -> u32 {
        self.offset
    }

    /// Bytes at the payload.
    #[must_use]
    pub fn size(&self) -> u32 {
        self.size
    }

    /// The payload.
    #[must_use]
    pub fn payload(&self) -> &Payload<'a> {
        &self.payload
    }

    /// The bytes of an [`Payload::InOut`], which the kernel writes during the call; `None` for any
    /// other payload. A session reaches the payload only through [`Self::payload`] and this, so
    /// it cannot swap one message's memory for another's: a message's size always describes the
    /// memory it was built with.
    pub fn in_out_mut(&mut self) -> Option<&mut [u8]> {
        match &mut self.payload {
            Payload::InOut(data) => Some(data),
            _ => None,
        }
    }

    /// The message as the kernel reads it, with `data_ptr` (0 for [`Payload::None`]).
    #[must_use]
    pub fn wire(&self, data_ptr: u64) -> MsgV1 {
        MsgV1 {
            cmd: self.cmd.code(),
            flags: self.flags,
            size: self.size,
            offset: self.offset,
            data_ptr,
        }
    }
}
