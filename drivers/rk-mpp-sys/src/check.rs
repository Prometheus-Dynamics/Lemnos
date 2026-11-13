//! What a chain must be before the session issues it. Pure: nothing here touches the device or
//! the session's memory.

use std::io;

use lemnos_driver_rk_mpp::uapi::{Command, FD_MASK, MAX_REG_TRANS_NUM, flags};
use lemnos_driver_rk_mpp::{
    Block, Facts, MAX_ADDRESSES, MAX_BUFFERS, MAX_RUNS, Message, ParamFacts, Payload,
    READ_BACK_BYTES, READ_BACK_SLOTS,
};

use crate::session::DATA_BYTES;

/// A chain the session may issue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Checked {
    /// The block a `MPP_CMD_INIT_CLIENT_TYPE` binds the session to.
    pub(crate) binds: Option<Block>,
}

/// The bit of `block` in a session's allowed set: one bit per block of [`Block::ALL`].
pub(crate) fn block_bit(block: Block) -> u32 {
    let index = Block::ALL
        .iter()
        .position(|b| *b == block)
        .expect("a block of Block::ALL");
    1 << index
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

/// A run of whole registers inside one of `facts`' windows and reaching its first: what the
/// driver's job checks promise, checked again here.
fn run(facts: &Facts, offset: u32, size: u32) -> Option<(usize, usize)> {
    if size == 0 || !offset.is_multiple_of(4) || !size.is_multiple_of(4) {
        return None;
    }
    let first = (offset / 4) as usize;
    let count = (size / 4) as usize;
    let end = first.checked_add(count)?;
    facts.window_of(u16::try_from(first).ok()?, count)?;
    (end >= usize::from(facts.first)).then_some((first, end))
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
    // A job is its table, at most `MAX_RUNS` writes, the offsets and the read-back.
    if n == 0 || n > MAX_RUNS + 4 {
        return Err(refused());
    }
    // A job on a parameter block says, on every message, that a descriptor word holds no
    // offset; nothing else does.
    let params = bound.and_then(Block::params).filter(|_| n > 1);
    let extra = if params.is_some() {
        flags::REG_NO_OFFSET
    } else {
        0
    };

    // The chain ends exactly at its last message, so the kernel never reads a message past it.
    for (i, m) in messages.iter().enumerate() {
        let expect = match (n, i + 1 == n) {
            (1, _) => 0,
            (_, true) => flags::MULTI_MSG | flags::LAST_MSG,
            (_, false) => flags::MULTI_MSG,
        };
        if m.flags() != expect | extra {
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
    match params {
        Some(facts) => param_job(messages, facts),
        None => job(messages, bound),
    }
    .map(|()| Checked { binds: None })
}

/// A parameter block's job: the structure, then optionally the offsets, then the results'
/// read-back. The structure is exactly the kernel's size, every descriptor word holds a
/// descriptor a register can name (1 to 1023) and every word the kernel uses as an index, a
/// size, a count or a mode is in its range; every offset is keyed by a descriptor word's
/// register; the read-back is exactly the results' size.
fn param_job(messages: &[Message<'_>], facts: &ParamFacts) -> io::Result<()> {
    let [write, rest @ .., read] = messages else {
        return Err(refused());
    };
    if !matches!(write.cmd(), Command::SetRegWrite)
        || !matches!(write.payload(), Payload::In(_))
        || write.offset() != 0
        || write.size() as usize != facts.bytes
    {
        return Err(refused());
    }
    let structure = exact(write).ok_or_else(refused)?;
    let word = |at: u16| {
        let at = usize::from(at);
        u32::from_ne_bytes([
            structure[at],
            structure[at + 1],
            structure[at + 2],
            structure[at + 3],
        ])
    };
    for descriptor in facts.descriptors {
        if !(1..=FD_MASK).contains(&word(descriptor.at)) {
            return Err(refused());
        }
    }
    for checked in facts.words {
        if !checked.check.allows(word(checked.at)) {
            return Err(refused());
        }
    }

    match rest {
        [] => {}
        [offsets] => {
            if !matches!(offsets.cmd(), Command::SetRegAddrOffset)
                || !matches!(offsets.payload(), Payload::In(_))
                || offsets.offset() != 0
            {
                return Err(refused());
            }
            let elems = exact(offsets).ok_or_else(refused)?;
            if elems.is_empty()
                || !elems.len().is_multiple_of(8)
                || elems.len() > 8 * facts.descriptors.len().min(MAX_REG_TRANS_NUM)
            {
                return Err(refused());
            }
            for elem in elems.as_chunks::<8>().0 {
                let index = u32::from_ne_bytes([elem[0], elem[1], elem[2], elem[3]]);
                if !facts
                    .descriptors
                    .iter()
                    .any(|d| u32::from(d.register) == index)
                {
                    return Err(refused());
                }
            }
        }
        _ => return Err(refused()),
    }

    match read.payload() {
        Payload::ReadBack { slot }
            if matches!(read.cmd(), Command::SetRegRead)
                && *slot < READ_BACK_SLOTS
                && read.offset() == 0
                && read.size() as usize == facts.results
                && facts.results <= READ_BACK_BYTES =>
        {
            Ok(())
        }
        _ => Err(refused()),
    }
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
            // The one allowed block with that client type. Two blocks can share a client type (the
            // two rkvenc generations); when both are allowed the bind is refused, since nothing says
            // which one the claim meant.
            let mut found = Block::ALL
                .into_iter()
                .filter(|b| b.facts().client == value && allowed & block_bit(*b) != 0);
            let block = match (found.next(), found.next()) {
                (Some(block), None) => block,
                _ => return Err(refused()),
            };
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

/// A job: the translation table, then one register write per run (at most [`MAX_RUNS`]), then
/// optionally the offsets, then optionally the read-back, on a bound session. Each table and
/// offset index is a register inside one of the writes, in a window the kernel translates.
fn job(messages: &[Message<'_>], bound: Option<Block>) -> io::Result<()> {
    let facts = bound.ok_or_else(refused)?.facts();
    let [table, rest @ ..] = messages else {
        return Err(refused());
    };
    let writes = rest
        .iter()
        .take_while(|m| matches!(m.cmd(), Command::SetRegWrite))
        .count();
    if writes == 0 || writes > MAX_RUNS {
        return Err(refused());
    }
    let (writes, rest) = rest.split_at(writes);
    let mut runs = [(0usize, 0usize); MAX_RUNS];
    for (i, write) in writes.iter().enumerate() {
        if !matches!(write.payload(), Payload::In(_)) {
            return Err(refused());
        }
        exact(write).ok_or_else(refused)?;
        let range = run(&facts, write.offset(), write.size()).ok_or_else(refused)?;
        if runs[..i].iter().any(|r| range.0 < r.1 && r.0 < range.1) {
            return Err(refused());
        }
        runs[i] = range;
    }
    let inside = |index: usize| {
        runs[..writes.len()]
            .iter()
            .any(|r| (r.0..r.1).contains(&index))
            && facts
                .window_of(u16::try_from(index).unwrap_or(u16::MAX), 1)
                .is_some_and(|w| facts.windows[w].translated)
    };

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
    // A table entry is an index into the translated window, so the register it names is that
    // window's first word plus the entry.
    let base = facts.table_window().map_or(0, |w| usize::from(w.first));
    for entry in entries.as_chunks::<2>().0 {
        let index = base + usize::from(u16::from_ne_bytes(*entry));
        if !inside(index) {
            return Err(refused());
        }
    }

    let mut rest = rest;
    if let [offsets, tail @ ..] = rest
        && matches!(offsets.cmd(), Command::SetRegAddrOffset)
    {
        // A block that takes offsets in the descriptor word is sent none.
        if facts.offset_in_descriptor {
            return Err(refused());
        }
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
