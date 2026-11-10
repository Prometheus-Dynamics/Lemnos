//! What a chain must be before the session issues it. Pure: nothing here touches the device or
//! the session's memory.

use std::io;

use lemnos_driver_rk_mpp::uapi::{Command, MAX_REG_TRANS_NUM, flags};
use lemnos_driver_rk_mpp::{
    Block, Facts, MAX_ADDRESSES, MAX_BUFFERS, Message, Payload, READ_BACK_BYTES, READ_BACK_SLOTS,
};

use crate::session::DATA_BYTES;

/// A chain the session may issue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Checked {
    /// The block a `MPP_CMD_INIT_CLIENT_TYPE` binds the session to.
    pub(crate) binds: Option<Block>,
}

pub(crate) fn refused() -> io::Error {
    io::Error::from_raw_os_error(libc::EINVAL)
}

/// The bytes the kernel reads at the message's `data_ptr`, exactly its size.
fn exact<'m>(m: &'m Message<'_>) -> Option<&'m [u8]> {
    let data: &[u8] = match m.payload() {
        Payload::In(data) => data,
        Payload::InOut(data) => data,
        Payload::None | Payload::ReadBack { .. } => return None,
    };
    (data.len() == m.size() as usize).then_some(data)
}

/// Bytes a payload takes in the session's scratch area: its length, rounded up to 8.
pub(crate) fn footprint(len: usize) -> usize {
    len.div_ceil(8) * 8
}

/// A run of whole registers inside `facts`' registers and reaching its first: what the driver's
/// job checks promise, checked again here.
fn run(facts: &Facts, offset: u32, size: u32) -> Option<(usize, usize)> {
    if size == 0 || !offset.is_multiple_of(4) || !size.is_multiple_of(4) {
        return None;
    }
    let first = (offset / 4) as usize;
    let end = first.checked_add((size / 4) as usize)?;
    (end <= usize::from(facts.registers) && end >= usize::from(facts.first)).then_some((first, end))
}

/// The block whose client type `value` is, if the session may bind to it.
fn block_of(value: u32, allowed: u32) -> Option<Block> {
    Block::ALL
        .into_iter()
        .find(|b| b.facts().client == value)
        .filter(|b| allowed & (1 << b.facts().client) != 0)
}

/// Checks `messages` against what the session issues: the chain's flags, the shape of the chain,
/// each message's command, payload kind and size, and for a job the register runs and indices
/// against the bound block. `bound` is the block the session is bound to; `allowed` a bit per
/// client type the session may bind to.
pub(crate) fn check(
    messages: &[Message<'_>],
    bound: Option<Block>,
    allowed: u32,
) -> io::Result<Checked> {
    let n = messages.len();
    if n == 0 || n > 4 {
        return Err(refused());
    }

    // The chain ends exactly at its last message, so the kernel never reads a message past it.
    for (i, m) in messages.iter().enumerate() {
        let expect = match (n, i + 1 == n) {
            (1, _) => 0,
            (_, true) => flags::MULTI_MSG | flags::LAST_MSG,
            (_, false) => flags::MULTI_MSG,
        };
        if m.flags() != expect {
            return Err(refused());
        }
    }

    // Every payload the session copies fits its scratch area.
    let data: usize = messages
        .iter()
        .filter_map(exact)
        .map(|d| footprint(d.len()))
        .sum();
    if data > DATA_BYTES {
        return Err(refused());
    }

    if n == 1 {
        return single(&messages[0], bound, allowed);
    }
    job(messages, bound).map(|()| Checked { binds: None })
}

/// A call of one message.
fn single(m: &Message<'_>, bound: Option<Block>, allowed: u32) -> io::Result<Checked> {
    let none = Checked { binds: None };
    match (m.cmd(), m.payload()) {
        // The kernel reads and writes one `u32` at `data_ptr`, whatever the size says.
        (
            Command::QueryHwSupport | Command::QueryHwId | Command::QueryCmdSupport,
            Payload::InOut(_),
        ) if m.size() == 4 && exact(m).is_some() => Ok(none),
        // One `u32`; the kernel rebinds a bound session without asking, so a session binds once,
        // and only to a block it was opened for.
        (Command::InitClientType, Payload::In(_)) if m.size() == 4 && bound.is_none() => {
            let data = exact(m).ok_or_else(refused)?;
            let value = u32::from_ne_bytes([data[0], data[1], data[2], data[3]]);
            let block = block_of(value, allowed).ok_or_else(refused)?;
            Ok(Checked { binds: Some(block) })
        }
        (Command::PollHwFinish | Command::ResetSession, Payload::None) if m.size() == 0 => Ok(none),
        // `u32` descriptors, `size` bytes of them.
        (Command::ReleaseFd, Payload::In(_))
            if m.size() > 0
                && m.size().is_multiple_of(4)
                && m.size() as usize <= 4 * MAX_BUFFERS
                && exact(m).is_some() =>
        {
            Ok(none)
        }
        _ => Err(refused()),
    }
}

/// A job: the translation table, the register write, then optionally the offsets, then optionally
/// the read-back, on a bound session. Each table and offset index is a register inside the write.
fn job(messages: &[Message<'_>], bound: Option<Block>) -> io::Result<()> {
    let facts = bound.ok_or_else(refused)?.facts();
    let [table, write, rest @ ..] = messages else {
        return Err(refused());
    };

    if !matches!(write.cmd(), Command::SetRegWrite) || !matches!(write.payload(), Payload::In(_)) {
        return Err(refused());
    }
    exact(write).ok_or_else(refused)?;
    let written = run(&facts, write.offset(), write.size()).ok_or_else(refused)?;
    let inside = |index: usize| (written.0..written.1).contains(&index);

    if !matches!(table.cmd(), Command::InitTransTable)
        || !matches!(table.payload(), Payload::In(_))
        || table.offset() != 0
    {
        return Err(refused());
    }
    let entries = exact(table).ok_or_else(refused)?;
    if entries.is_empty()
        || !entries.len().is_multiple_of(2)
        || entries.len() > 2 * MAX_ADDRESSES.min(MAX_REG_TRANS_NUM)
    {
        return Err(refused());
    }
    for entry in entries.as_chunks::<2>().0 {
        if !inside(usize::from(u16::from_ne_bytes(*entry))) {
            return Err(refused());
        }
    }

    let mut rest = rest;
    if let [offsets, tail @ ..] = rest
        && matches!(offsets.cmd(), Command::SetRegAddrOffset)
    {
        if !matches!(offsets.payload(), Payload::In(_)) || offsets.offset() != 0 {
            return Err(refused());
        }
        let elems = exact(offsets).ok_or_else(refused)?;
        if elems.is_empty()
            || !elems.len().is_multiple_of(8)
            || elems.len() > 8 * MAX_ADDRESSES.min(MAX_REG_TRANS_NUM)
        {
            return Err(refused());
        }
        for elem in elems.as_chunks::<8>().0 {
            let index = u32::from_ne_bytes([elem[0], elem[1], elem[2], elem[3]]);
            if !inside(index as usize) {
                return Err(refused());
            }
        }
        rest = tail;
    }

    match rest {
        [] => Ok(()),
        [read] => match read.payload() {
            Payload::ReadBack { slot }
                if matches!(read.cmd(), Command::SetRegRead)
                    && *slot < READ_BACK_SLOTS
                    && read.size() as usize <= READ_BACK_BYTES
                    && run(&facts, read.offset(), read.size()).is_some() =>
            {
                Ok(())
            }
            _ => Err(refused()),
        },
        _ => Err(refused()),
    }
}
