//! A played kernel: what `mpp_common.c` does with each message of a call, translation
//! included, on plain memory. For this crate's tests and, with the `played` feature, for the
//! tests of the crates that drive it. Nothing here reaches a device.

#![allow(missing_docs, clippy::missing_panics_doc)]

use std::io;

use crate::uapi::{self, Command, MAX_MSG_NUM, MAX_REG_TRANS_NUM, flags};
use crate::{Block, MAX_REGISTERS, Message, Payload, READ_BACK_BYTES, READ_BACK_SLOTS};

const SLOTS: usize = READ_BACK_SLOTS;

/// The device address the played kernel gives descriptor `fd`.
pub fn iova(fd: u32) -> u32 {
    0x1000_0000 + (fd << 20)
}

pub struct Task {
    pub regs: [u32; MAX_REGISTERS],
    pub read: Option<(usize, usize, usize)>,
}

/// A kernel that does what `mpp_common.c` does with each message.
pub struct Kernel {
    pub hw_support: u32,
    pub confined: bool,
    pub fail_open: Option<i32>,
    pub fail_send: Option<i32>,
    pub opens: u32,
    pub client: Option<u32>,
    pub table: [u16; MAX_REG_TRANS_NUM],
    pub table_len: usize,
    /// The commands and flags of the last call.
    pub last: [(u32, u32); MAX_MSG_NUM],
    pub last_len: usize,
    pub calls: u32,
    /// Every descriptor a translation took, in order, since the last clear.
    pub translated: [u32; 4 * MAX_REG_TRANS_NUM],
    pub translated_len: usize,
    pub tasks: [Task; SLOTS],
    pub head: usize,
    pub queued: usize,
    pub areas: [[u8; READ_BACK_BYTES]; SLOTS],
    pub polls: u32,
    pub resets: u32,
    pub released: [u32; 8],
    pub released_len: usize,
}

#[allow(missing_docs)]
impl Kernel {
    pub fn new(hw_support: u32) -> Self {
        Self {
            hw_support,
            confined: true,
            fail_open: None,
            fail_send: None,
            opens: 0,
            client: None,
            table: [0; MAX_REG_TRANS_NUM],
            table_len: 0,
            last: [(0, 0); MAX_MSG_NUM],
            last_len: 0,
            calls: 0,
            translated: [0; 4 * MAX_REG_TRANS_NUM],
            translated_len: 0,
            tasks: std::array::from_fn(|_| Task {
                regs: [0; MAX_REGISTERS],
                read: None,
            }),
            head: 0,
            queued: 0,
            areas: [[0; READ_BACK_BYTES]; SLOTS],
            polls: 0,
            resets: 0,
            released: [0; 8],
            released_len: 0,
        }
    }

    pub fn block(&self) -> Block {
        let client = self.client.expect("session bound");
        Block::ALL
            .into_iter()
            .find(|b| b.facts().client == client)
            .expect("known client")
    }

    pub fn take_fd(&mut self, fd: u32) -> u32 {
        assert!(fd != 0);
        self.translated[self.translated_len] = fd;
        self.translated_len += 1;
        iova(fd)
    }

    /// `mpp_collect_msgs` and the per-block `alloc_task` for one call.
    pub fn call(&mut self, messages: &mut [Message<'_>]) -> io::Result<()> {
        self.calls += 1;
        if let Some(errno) = self.fail_send.take() {
            return Err(io::Error::from_raw_os_error(errno));
        }
        assert!(messages.len() <= MAX_MSG_NUM);
        self.last_len = messages.len();
        for (i, m) in messages.iter().enumerate() {
            self.last[i] = (m.cmd().code(), m.flags());
            // Only the chain flags are ever set.
            assert_eq!(m.flags() & !(flags::MULTI_MSG | flags::LAST_MSG), 0);
            let last = i + 1 == messages.len();
            let expect = match (messages.len(), last) {
                (1, _) => 0,
                (_, true) => flags::MULTI_MSG | flags::LAST_MSG,
                (_, false) => flags::MULTI_MSG,
            };
            assert_eq!(m.flags(), expect);
        }

        let mut regs = [0u32; MAX_REGISTERS];
        let mut wrote = false;
        let mut offsets = [(0u32, 0u32); MAX_REG_TRANS_NUM];
        let mut offsets_len = 0;
        let mut read = None;
        for m in messages.iter_mut() {
            let size = m.size() as usize;
            let at = m.offset() as usize;
            if m.cmd() == Command::QueryHwSupport {
                let data = m.in_out_mut().expect("the answer's memory");
                data.copy_from_slice(&self.hw_support.to_ne_bytes());
                continue;
            }
            match (m.cmd(), m.payload()) {
                (Command::InitClientType, Payload::In(data)) => {
                    self.client = Some(u32::from_ne_bytes((*data).try_into().unwrap()));
                }
                (Command::InitTransTable, Payload::In(data)) => {
                    assert!(size > 0 && size <= 2 * MAX_REG_TRANS_NUM);
                    self.table_len = size / 2;
                    for (t, b) in self.table.iter_mut().zip(data.as_chunks::<2>().0) {
                        *t = u16::from_ne_bytes(*b);
                    }
                }
                (Command::SetRegWrite, Payload::In(data)) => {
                    let facts = self.block().facts();
                    // What this crate promises past mpp_check_req: whole registers, inside the
                    // block, reaching its first.
                    assert!(at.is_multiple_of(4) && size.is_multiple_of(4) && size > 0);
                    assert!(at + size <= 4 * usize::from(facts.registers));
                    assert!(at + size >= 4 * usize::from(facts.first));
                    for (r, w) in regs[at / 4..].iter_mut().zip(data.as_chunks::<4>().0) {
                        *r = u32::from_ne_bytes(*w);
                    }
                    wrote = true;
                }
                (Command::SetRegAddrOffset, Payload::In(data)) => {
                    for e in data.as_chunks::<8>().0 {
                        offsets[offsets_len] = (
                            u32::from_ne_bytes(e[..4].try_into().unwrap()),
                            u32::from_ne_bytes(e[4..].try_into().unwrap()),
                        );
                        offsets_len += 1;
                    }
                }
                (Command::SetRegRead, Payload::ReadBack { slot }) => {
                    let facts = self.block().facts();
                    assert!(at + size <= 4 * usize::from(facts.registers));
                    assert!(size <= READ_BACK_BYTES && *slot < SLOTS);
                    read = Some((at, size, *slot));
                }
                (Command::PollHwFinish, Payload::None) => {
                    self.polls += 1;
                    assert!(self.queued > 0, "poll with nothing queued");
                    let task = &self.tasks[self.head];
                    if let Some((at, size, slot)) = task.read {
                        let (words, _) = self.areas[slot][..size].as_chunks_mut::<4>();
                        for (b, r) in words.iter_mut().zip(&task.regs[at / 4..]) {
                            *b = r.to_ne_bytes();
                        }
                    }
                    self.head = (self.head + 1) % SLOTS;
                    self.queued -= 1;
                }
                (Command::ResetSession, Payload::None) => self.resets += 1,
                (Command::ReleaseFd, Payload::In(data)) => {
                    self.released[self.released_len] =
                        u32::from_ne_bytes((*data).try_into().unwrap());
                    self.released_len += 1;
                }
                (cmd, _) => panic!("{cmd:?} is never sent"),
            }
        }
        if !wrote {
            return Ok(());
        }

        // alloc_task: the mode is read before translation, then the session's table, then the
        // direct register, then the offsets.
        let facts = self.block().facts();
        assert!(self.table_len > 0, "the built-in table would be used");
        let mode_applies = facts
            .direct
            .map(|d| (d, d.applies(regs[usize::from(d.mode_register)])));
        for i in 0..self.table_len {
            let index = usize::from(self.table[i]);
            assert!(index < usize::from(facts.registers));
            let fd = regs[index] & uapi::FD_MASK;
            let offset = regs[index] >> 10;
            if fd == 0 {
                continue;
            }
            regs[index] = self.take_fd(fd) + offset;
        }
        if let Some((d, true)) = mode_applies {
            let index = usize::from(d.register);
            let fd = regs[index] & uapi::FD_MASK;
            let offset = (regs[index] >> 10) << 4;
            assert!(fd != 0, "the kernel would fail the task");
            regs[index] = self.take_fd(fd) + offset;
        }
        for &(index, offset) in &offsets[..offsets_len] {
            assert!((index as usize) < usize::from(facts.registers));
            regs[index as usize] += offset;
        }
        assert!(self.queued < SLOTS);
        let tail = (self.head + self.queued) % SLOTS;
        self.tasks[tail] = Task { regs, read };
        self.queued += 1;
        Ok(())
    }
}
