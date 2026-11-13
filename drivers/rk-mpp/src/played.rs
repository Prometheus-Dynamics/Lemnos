//! A played kernel: what `mpp_common.c` does with each message of a call, translation
//! included, on plain memory. For this crate's tests and, with the `played` feature, for the
//! tests of the crates that drive it. Nothing here reaches a device.

#![allow(missing_docs, clippy::missing_panics_doc)]

use std::io;

use crate::uapi::{self, Command, MAX_MSG_NUM, MAX_REG_TRANS_NUM, flags};
use crate::{
    Block, MAX_JOB_WORDS, MAX_REGISTERS, Message, Payload, READ_BACK_BYTES, READ_BACK_SLOTS,
};

const SLOTS: usize = READ_BACK_SLOTS;

/// The device address the played kernel gives descriptor `fd`.
pub fn iova(fd: u32) -> u32 {
    0x1000_0000 + (fd << 20)
}

pub struct Task {
    /// The block's flat register array: its windows' words, in order ([`crate::Facts::flat`]).
    pub regs: [u32; MAX_JOB_WORDS],
    /// The flat index the read starts at, its bytes and its slot.
    pub read: Option<(usize, usize, usize)>,
    /// An IEP2 task: `regs` holds its parameter structure, and its read copies `output`.
    pub iep2: bool,
}

/// `sizeof(struct iep2_params)` and `sizeof(struct iep2_output)`, from `mpp_iep2.c` (`gcc`,
/// LP64).
pub const IEP2_PARAMS: usize = 656;
pub const IEP2_OUTPUT: usize = 412;

/// `iep2_addr_rnum` of `mpp_iep2.c`: the register of each address word, from `src[0].y` at byte
/// 36 on, -1 for a word the kernel skips. Transcribed again here, apart from the driver's facts,
/// so that the played kernel is not the driver's own description of itself.
pub const IEP2_ADDR_RNUM: [i32; 17] = [
    24, 27, 28, 25, 29, 30, 26, 31, 32, 44, 46, -1, 45, 47, -1, 34, 33,
];

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
    /// What the IEP2 block counted, as the kernel copies it out at a poll.
    pub output: [u8; IEP2_OUTPUT],
    /// The flags of the last call's messages, ORed, as `mpp_collect_msgs` gathers them.
    pub last_flags: u32,
    /// The block the claim named (see [`Self::block`]).
    pub bound: Option<Block>,
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
                regs: [0; MAX_JOB_WORDS],
                read: None,
                iep2: false,
            }),
            head: 0,
            queued: 0,
            areas: [[0; READ_BACK_BYTES]; SLOTS],
            polls: 0,
            resets: 0,
            released: [0; 8],
            released_len: 0,
            output: [0; IEP2_OUTPUT],
            last_flags: 0,
            bound: None,
        }
    }

    /// The block this session is bound to: named by the claim's `confined` call, which every claim
    /// makes before a job (the client type alone does not tell the two rkvenc generations apart).
    pub fn block(&self) -> Block {
        if let Some(block) = self.bound {
            return block;
        }
        let client = self.client.expect("session bound");
        Block::ALL
            .into_iter()
            .find(|b| b.facts().client == client)
            .expect("known client")
    }

    pub fn take_fd(&mut self, fd: u32) -> u32 {
        assert!(fd != 0);
        // The log keeps the first translations; a long run stops adding to it.
        if let Some(slot) = self.translated.get_mut(self.translated_len) {
            *slot = fd;
            self.translated_len += 1;
        }
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
            // Only the chain flags are ever set, and on a parameter job's messages the one
            // that says a descriptor word holds no offset.
            let extra = m.flags() & flags::REG_NO_OFFSET;
            assert_eq!(
                m.flags() & !(flags::MULTI_MSG | flags::LAST_MSG | flags::REG_NO_OFFSET),
                0
            );
            let last = i + 1 == messages.len();
            let expect = match (messages.len(), last) {
                (1, _) => 0,
                (_, true) => flags::MULTI_MSG | flags::LAST_MSG,
                (_, false) => flags::MULTI_MSG,
            };
            assert_eq!(m.flags(), expect | extra);
        }
        // `mpp_collect_msgs`: the task's flags are every message's, ORed.
        self.last_flags = messages.iter().fold(0, |all, m| all | m.flags());
        if self.client == Some(uapi::client::IEP2) && messages.len() > 1 {
            return self.iep2(messages);
        }

        let mut regs = [0u32; MAX_JOB_WORDS];
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
                    // What mpp_check_req and this crate promise: whole registers, inside one
                    // window of the block, reaching its first.
                    assert!(at.is_multiple_of(4) && size.is_multiple_of(4) && size > 0);
                    let (word, count) = (at / 4, size / 4);
                    assert!(
                        facts.window_of(word as u16, count).is_some(),
                        "a write outside a window"
                    );
                    assert!(word + count >= usize::from(facts.first));
                    let base = facts.flat(word).expect("inside a window");
                    for (r, w) in regs[base..].iter_mut().zip(data.as_chunks::<4>().0) {
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
                    assert!(at.is_multiple_of(4) && size.is_multiple_of(4) && size > 0);
                    assert!(
                        facts.window_of((at / 4) as u16, size / 4).is_some(),
                        "a read outside a window"
                    );
                    assert!(size <= READ_BACK_BYTES && *slot < SLOTS);
                    let base = facts.flat(at / 4).expect("inside a window");
                    read = Some((base, size, *slot));
                }
                (Command::PollHwFinish, Payload::None) => {
                    self.polls += 1;
                    assert!(self.queued > 0, "poll with nothing queued");
                    let task = &self.tasks[self.head];
                    if let (Some((_, size, slot)), true) = (task.read, task.iep2) {
                        // `iep2_result`: `size` bytes of the output, whatever the size.
                        assert!(size <= IEP2_OUTPUT, "the kernel would read past its output");
                        self.areas[slot][..size].copy_from_slice(&self.output[..size]);
                    } else if let Some((base, size, slot)) = task.read {
                        let (words, _) = self.areas[slot][..size].as_chunks_mut::<4>();
                        for (b, r) in words.iter_mut().zip(&task.regs[base..]) {
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
        // The kernel translates only the first window's registers (`task->reg`): a table entry or
        // an offset that names another window would index past it.
        let facts = self.block().facts();
        assert!(self.table_len > 0, "the built-in table would be used");
        let translated = |word: usize| {
            facts
                .window_of(word as u16, 1)
                .is_some_and(|w| facts.windows[w].translated)
        };
        let mode_applies = facts.direct.map(|d| {
            let at = facts
                .flat(usize::from(d.mode_register))
                .expect("a mode register in a window");
            (d, d.applies(regs[at]))
        });
        // A table entry is an index into the translated window: the kernel's array for it.
        let table_window = facts.table_window().expect("a translated window");
        for i in 0..self.table_len {
            let index = usize::from(self.table[i]);
            assert!(
                index < usize::from(table_window.words),
                "a table entry outside the array"
            );
            let at = facts
                .flat(usize::from(table_window.first) + index)
                .expect("inside a window");
            let fd = regs[at] & uapi::FD_MASK;
            let offset = regs[at] >> 10;
            if fd == 0 {
                continue;
            }
            regs[at] = self.take_fd(fd) + offset;
        }
        if let Some((d, true)) = mode_applies {
            let at = facts
                .flat(usize::from(d.register))
                .expect("a direct register in a window");
            let fd = regs[at] & uapi::FD_MASK;
            let offset = (regs[at] >> 10) << 4;
            assert!(fd != 0, "the kernel would fail the task");
            regs[at] = self.take_fd(fd) + offset;
        }
        for &(index, offset) in &offsets[..offsets_len] {
            assert!(
                translated(index as usize),
                "an offset outside a translated window"
            );
            let at = facts.flat(index as usize).expect("inside a window");
            regs[at] += offset;
        }
        assert!(self.queued < SLOTS);
        let tail = (self.head + self.queued) % SLOTS;
        self.tasks[tail] = Task {
            regs,
            read,
            iep2: false,
        };
        self.queued += 1;
        Ok(())
    }

    /// `iep2_alloc_task` for one call on a session bound to the IEP2: `iep2_extract_task_msg`,
    /// then `iep2_process_reg_fd`.
    fn iep2(&mut self, messages: &mut [Message<'_>]) -> io::Result<()> {
        let mut params = [0u8; 4 * MAX_REGISTERS];
        let mut wrote = false;
        let mut offsets = [(0u32, 0u32); MAX_REG_TRANS_NUM];
        let mut offsets_len = 0;
        let mut read = None;
        for m in messages.iter() {
            let size = m.size() as usize;
            if size == 0 {
                continue;
            }
            match (m.cmd(), m.payload()) {
                // `copy_from_user(&task->params, req->data, req->size)`: unchecked.
                (Command::SetRegWrite, Payload::In(data)) => {
                    assert!(
                        size <= IEP2_PARAMS,
                        "the kernel would write past its params"
                    );
                    params[..size].copy_from_slice(data);
                    wrote = true;
                }
                (Command::SetRegRead, Payload::ReadBack { slot }) => {
                    assert!(*slot < SLOTS);
                    read = Some((0, size, *slot));
                }
                // `mpp_extract_reg_offset_info`.
                (Command::SetRegAddrOffset, Payload::In(data)) => {
                    assert!(size.is_multiple_of(8), "the kernel would copy a part entry");
                    assert!(offsets_len + size / 8 <= MAX_REG_TRANS_NUM);
                    for e in data.as_chunks::<8>().0 {
                        offsets[offsets_len] = (
                            u32::from_ne_bytes(e[..4].try_into().unwrap()),
                            u32::from_ne_bytes(e[4..].try_into().unwrap()),
                        );
                        offsets_len += 1;
                    }
                }
                (cmd, _) => panic!("{cmd:?} is not part of an IEP2 job"),
            }
        }
        assert!(wrote, "a task without its parameters");
        let no_offset = self.last_flags & flags::REG_NO_OFFSET != 0;
        for (i, &register) in IEP2_ADDR_RNUM.iter().enumerate() {
            let at = 36 + 4 * i;
            let word = u32::from_ne_bytes(params[at..at + 4].try_into().unwrap());
            let (fd, offset) = if no_offset {
                let offset = offsets[..offsets_len]
                    .iter()
                    .find(|&&(index, _)| index as i32 == register)
                    .map_or(0, |&(_, offset)| offset);
                (word, offset)
            } else {
                (word & uapi::FD_MASK, word >> 10)
            };
            if fd == 0 || register == -1 {
                continue;
            }
            let address = self.take_fd(fd).wrapping_add(offset);
            params[at..at + 4].copy_from_slice(&address.to_ne_bytes());
        }
        let mut regs = [0u32; MAX_JOB_WORDS];
        for (r, w) in regs.iter_mut().zip(params.as_chunks::<4>().0) {
            *r = u32::from_ne_bytes(*w);
        }
        assert!(self.queued < SLOTS);
        let tail = (self.head + self.queued) % SLOTS;
        self.tasks[tail] = Task {
            regs,
            read,
            iep2: true,
        };
        self.queued += 1;
        Ok(())
    }
}
