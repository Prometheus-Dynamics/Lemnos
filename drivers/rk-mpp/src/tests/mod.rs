//! Layouts, and the claim against a played kernel that does what `mpp_common.c` does with each
//! message, translation included. No device node is opened; buffers are sealed memfds.

use std::cell::RefCell;
use std::io;
use std::mem::{align_of, offset_of, size_of};
use std::os::fd::{AsRawFd, BorrowedFd, OwnedFd};
use std::rc::Rc;

use lemnos_linux_sys::dma_buf;
use lemnos_measure::test_alloc::allocations;

use crate::played::{Kernel, iova};
use crate::uapi::{
    self, BatMsg, Command, IOC_CFG_V1, IOC_CFG_V2, MAX_MSG_NUM, MAX_REG_TRANS_NUM, MsgV1,
    RegOffsetElem, client, flags,
};
use crate::{
    Address, Block, BufferError, BufferId, ClaimError, CompleteError, Completion, Device, Job,
    JobError, MAX_ADDRESSES, MAX_BUFFERS, MAX_JOB_WORDS, MAX_REGISTERS, MAX_RUNS, Message,
    READ_BACK_BYTES, READ_BACK_SLOTS, ReadBack, Run, Service, Session, SubmitError, backoff_ns,
};

mod codec;
mod params;

#[global_allocator]
static ALLOCATOR: lemnos_measure::test_alloc::Counting = lemnos_measure::test_alloc::Counting;

// ---------------------------------------------------------------------------------------------
// Layouts

#[test]
fn request_numbers_match_the_header() {
    // _IOW('v', 1, unsigned int) and _IOW('v', 2, unsigned int), from gcc on rk-mpp.h.
    assert_eq!(IOC_CFG_V1, 0x4004_7601);
    assert_eq!(IOC_CFG_V2, 0x4004_7602);
    assert_eq!(uapi::MAGIC, b'v');
}

#[test]
fn message_layouts_match_the_kernel() {
    assert_eq!(size_of::<MsgV1>(), 24);
    assert_eq!(align_of::<MsgV1>(), 8);
    assert_eq!(offset_of!(MsgV1, cmd), 0);
    assert_eq!(offset_of!(MsgV1, flags), 4);
    assert_eq!(offset_of!(MsgV1, size), 8);
    assert_eq!(offset_of!(MsgV1, offset), 12);
    assert_eq!(offset_of!(MsgV1, data_ptr), 16);

    assert_eq!(size_of::<BatMsg>(), 16);
    assert_eq!(offset_of!(BatMsg, flag), 0);
    assert_eq!(offset_of!(BatMsg, fd), 8);
    assert_eq!(offset_of!(BatMsg, ret), 12);

    assert_eq!(size_of::<RegOffsetElem>(), 8);
    assert_eq!(offset_of!(RegOffsetElem, index), 0);
    assert_eq!(offset_of!(RegOffsetElem, offset), 4);
    let elem = RegOffsetElem {
        index: 0x0102_0304,
        offset: 0x0506_0708,
    };
    let bytes = elem.to_ne_bytes();
    assert_eq!(bytes[..4], 0x0102_0304u32.to_ne_bytes());
    assert_eq!(bytes[4..], 0x0506_0708u32.to_ne_bytes());
}

#[test]
fn commands_flags_and_clients_match_the_header() {
    let commands = [
        (Command::QueryHwSupport, 0x000),
        (Command::QueryHwId, 0x001),
        (Command::QueryCmdSupport, 0x002),
        (Command::InitClientType, 0x100),
        (Command::InitDriverData, 0x101),
        (Command::InitTransTable, 0x102),
        (Command::SetRegWrite, 0x200),
        (Command::SetRegRead, 0x201),
        (Command::SetRegAddrOffset, 0x202),
        (Command::SetRcbInfo, 0x203),
        (Command::SetSessionFd, 0x204),
        (Command::PollHwFinish, 0x300),
        (Command::PollHwIrq, 0x301),
        (Command::ResetSession, 0x400),
        (Command::TransFdToIova, 0x401),
        (Command::ReleaseFd, 0x402),
        (Command::SendCodecInfo, 0x403),
    ];
    for (command, code) in commands {
        assert_eq!(command.code(), code, "{command:?}");
    }
    assert_eq!(
        [
            flags::MULTI_MSG,
            flags::LAST_MSG,
            flags::REG_FD_NO_TRANS,
            flags::SCL_FD_NO_TRANS,
            flags::REG_NO_OFFSET,
            flags::SECURE_MODE,
        ],
        [0x1, 0x2, 0x4, 0x8, 0x10, 0x1_0000]
    );
    assert_eq!(uapi::BAT_MSG_DONE, 1);
    assert_eq!(
        [
            client::VDPU1,
            client::VDPU2,
            client::VDPU1_PP,
            client::VDPU2_PP,
            client::AV1DEC,
            client::HEVC_DEC,
            client::RKVDEC,
            client::AVSPLUS_DEC,
            client::RKJPEGD,
            client::RKVENC,
            client::VEPU1,
            client::VEPU2,
            client::VEPU2_JPEG,
            client::RKJPEGE,
            client::VEPU22,
            client::IEP2,
            client::VDPP,
            client::END,
        ],
        [
            0, 1, 2, 3, 4, 8, 9, 12, 13, 16, 17, 18, 19, 20, 24, 28, 29, 30
        ]
    );
    assert_eq!(MAX_MSG_NUM, 16);
    assert_eq!(MAX_REG_TRANS_NUM, 80);
    assert_eq!(uapi::FD_MASK, 0x3ff);
}

#[test]
fn block_facts_stay_inside_the_kernel_limits() {
    let mut most = 0;
    for block in Block::ALL {
        let facts = block.facts();
        assert!(facts.client < client::END, "{block:?}");
        if block.params().is_some() {
            // A parameter block has no register file: every register-file job runs past it.
            assert!(facts.windows.is_empty(), "{block:?}");
            continue;
        }
        for window in facts.windows {
            assert!(usize::from(window.words) <= MAX_REGISTERS, "{block:?}");
        }
        assert!(facts.words() <= MAX_JOB_WORDS, "{block:?}");
        assert!(
            facts.windows.iter().any(|w| w.holds(facts.first, 1)),
            "{block:?}"
        );
        if let Some(d) = facts.direct {
            assert!(facts.flat(usize::from(d.register)).is_some(), "{block:?}");
            assert!(
                facts.flat(usize::from(d.mode_register)).is_some(),
                "{block:?}"
            );
        }
        most = most.max(facts.words());
    }
    assert_eq!(most, MAX_JOB_WORDS);
    assert_eq!(READ_BACK_BYTES, 4 * MAX_REGISTERS);
    // Addresses, plus one for a direct register, stay below the kernel's region array, whose
    // bound check is off by one.
    const { assert!(MAX_ADDRESSES + 1 < MAX_REG_TRANS_NUM) };
    // A job is at most the table, its writes, the offsets and the read-back.
    const { assert!(MAX_RUNS + 3 <= MAX_MSG_NUM) };
    assert_eq!(
        (Block::Vdpu2.facts().words(), Block::Vdpu2.facts().first),
        (159, 50)
    );
}

#[test]
fn wire_form_carries_what_the_message_says() {
    let data = [1u8, 2, 3, 4, 5, 6, 7, 8];
    let mut chain = [
        Message::input(Command::SetRegWrite, 0x40, &data),
        Message::bare(Command::PollHwFinish),
    ];
    Message::chain(&mut chain);
    assert_eq!(
        chain[0].wire(0xdead_beef),
        MsgV1 {
            cmd: 0x200,
            flags: flags::MULTI_MSG,
            size: 8,
            offset: 0x40,
            data_ptr: 0xdead_beef,
        }
    );
    assert_eq!(chain[1].flags(), flags::MULTI_MSG | flags::LAST_MSG);
    let mut single = [Message::bare(Command::ResetSession)];
    Message::chain(&mut single);
    assert_eq!(single[0].flags(), 0);
}

// ---------------------------------------------------------------------------------------------
// A played kernel

struct Played(Rc<RefCell<Kernel>>);

impl Session for Played {
    fn send(&mut self, messages: &mut [Message<'_>]) -> io::Result<()> {
        self.0.borrow_mut().call(messages)
    }

    fn read_back(&mut self, slot: usize, into: &mut [u8]) {
        let n = into.len();
        into.copy_from_slice(&self.0.borrow().areas[slot][..n]);
    }
}

struct PlayedDevice(Rc<RefCell<Kernel>>);

impl Device for PlayedDevice {
    type Session = Played;

    fn open(&mut self) -> io::Result<Played> {
        let mut kernel = self.0.borrow_mut();
        if let Some(errno) = kernel.fail_open.take() {
            return Err(io::Error::from_raw_os_error(errno));
        }
        kernel.opens += 1;
        kernel.client = None;
        Ok(Played(Rc::clone(&self.0)))
    }

    fn confined(&mut self, _block: Block) -> io::Result<bool> {
        Ok(self.0.borrow().confined)
    }

    fn buffer_size(&mut self, fd: BorrowedFd<'_>) -> io::Result<u64> {
        dma_buf::size(fd)
    }
}

fn all_blocks() -> u32 {
    Block::ALL
        .into_iter()
        .fold(0, |bits, b| bits | 1 << b.facts().client)
}

fn service() -> (Service<PlayedDevice>, Rc<RefCell<Kernel>>) {
    let kernel = Rc::new(RefCell::new(Kernel::new(all_blocks())));
    (Service::new(PlayedDevice(Rc::clone(&kernel))), kernel)
}

fn memfd(len: u64) -> OwnedFd {
    dma_buf::sealed_memfd(c"mpp-test", len).unwrap()
}

/// A Vepu2 job writing registers 0..32 with buffers in registers 8 and 9.
fn regs() -> [u32; 32] {
    std::array::from_fn(|i| 0xa500_0000 | i as u32)
}

// ---------------------------------------------------------------------------------------------
// Claims

#[test]
fn a_claim_checks_the_block_then_binds_the_session() {
    let (mut service, kernel) = service();
    {
        let claim = service.claim(Block::Vepu2, 0).unwrap();
        assert_eq!(claim.block(), Block::Vepu2);
    }
    let k = kernel.borrow();
    assert_eq!(k.client, Some(client::VEPU2));
    assert_eq!(k.opens, 1);
    // Query, bind, and the reset on drop.
    assert_eq!(k.calls, 3);
    assert_eq!(k.resets, 1);
}

#[test]
fn an_absent_or_unconfined_block_is_refused() {
    let (mut service, kernel) = service();
    kernel.borrow_mut().hw_support = 1 << client::VDPU2;
    assert!(matches!(
        service.claim(Block::Vepu2, 0),
        Err(ClaimError::Absent)
    ));
    assert_eq!(kernel.borrow().client, None);
    kernel.borrow_mut().confined = false;
    assert!(matches!(
        service.claim(Block::Vdpu2, 0),
        Err(ClaimError::NotConfined)
    ));
    assert_eq!(kernel.borrow().client, None, "never bound without an IOMMU");
}

#[test]
fn a_failed_claim_backs_off_doubling_to_eight_seconds_and_never_gives_up() {
    assert_eq!(backoff_ns(1), 250_000_000);
    assert_eq!(backoff_ns(2), 500_000_000);
    assert_eq!(backoff_ns(6), 8_000_000_000);
    assert_eq!(backoff_ns(60), 8_000_000_000);

    let (mut service, kernel) = service();
    let mut now = 0;
    for failures in 1..=8 {
        kernel.borrow_mut().fail_open = Some(libc_eio());
        let Err(ClaimError::Device { retry_at_ns, .. }) = service.claim(Block::Vepu1, now) else {
            panic!("expected a device failure");
        };
        assert_eq!(retry_at_ns, now + backoff_ns(failures));
        let opens = kernel.borrow().opens;
        assert!(matches!(
            service.claim(Block::Vepu1, retry_at_ns - 1),
            Err(ClaimError::BackingOff { .. })
        ));
        assert_eq!(kernel.borrow().opens, opens, "untouched while backing off");
        now = retry_at_ns;
    }
    assert!(service.claim(Block::Vepu1, now).is_ok());
    // Cleared: a failure now starts again from the first delay.
    kernel.borrow_mut().fail_open = Some(libc_eio());
    let Err(ClaimError::Device { retry_at_ns, .. }) = service.claim(Block::Vepu1, now) else {
        panic!("expected a device failure");
    };
    assert_eq!(retry_at_ns, now + backoff_ns(1));
}

fn libc_eio() -> i32 {
    5
}

#[test]
fn buffers_are_measured_and_counted() {
    let (mut service, _kernel) = service();
    let mut claim = service.claim(Block::Vepu2, 0).unwrap();
    assert!(matches!(
        claim.add_buffer(memfd(0)),
        Err(BufferError::Size(None))
    ));
    let id = claim.add_buffer(memfd(4096)).unwrap();
    assert_eq!(claim.buffer_size(id), Some(4096));
    for _ in 1..MAX_BUFFERS {
        claim.add_buffer(memfd(64)).unwrap();
    }
    assert!(matches!(
        claim.add_buffer(memfd(64)),
        Err(BufferError::Full)
    ));
    let fd = claim.remove_buffer(id).unwrap();
    assert_eq!(dma_buf::size(std::os::fd::AsFd::as_fd(&fd)).unwrap(), 4096);
    // The id is stale once removed, even when its slot is reused.
    assert!(matches!(claim.remove_buffer(id), Err(BufferError::Unknown)));
    let again = claim.add_buffer(fd).unwrap();
    assert_ne!(again, id);
    assert_eq!(claim.buffer_size(id), None);
}

// ---------------------------------------------------------------------------------------------
// Jobs

#[test]
fn a_job_reaches_the_block_with_its_buffers_translated() {
    let (mut service, kernel) = service();
    let mut claim = service.claim(Block::Vepu2, 0).unwrap();
    let input = claim.add_buffer(memfd(8192)).unwrap();
    let output = claim.add_buffer(memfd(4096)).unwrap();
    let regs = regs();
    let addresses = [
        Address {
            register: 8,
            buffer: input,
            offset: 0,
        },
        Address {
            register: 9,
            buffer: output,
            offset: 4095,
        },
    ];
    let job = Job {
        identity: 77,
        first: 0,
        registers: &regs,
        more: &[],
        addresses: &addresses,
        read: Some(ReadBack {
            first: 0,
            count: 32,
        }),
    };
    claim.submit(&job).unwrap();
    {
        let k = kernel.borrow();
        let cmds: Vec<u32> = k.last[..k.last_len].iter().map(|c| c.0).collect();
        assert_eq!(cmds, [0x102, 0x200, 0x202, 0x201]);
        assert_eq!(k.table[..k.table_len], [8, 9]);
    }
    let mut back = [0u32; 32];
    assert_eq!(
        claim.complete(&mut back).unwrap(),
        Completion {
            identity: 77,
            registers: 32
        }
    );
    // Every other register reached the block as the caller wrote it; the two addresses are the
    // buffers' device addresses plus their offsets.
    for (i, (&got, &wrote)) in back.iter().zip(&regs).enumerate() {
        match i {
            8 | 9 => {}
            _ => assert_eq!(got, wrote, "register {i}"),
        }
    }
    let fds: Vec<u32> = kernel.borrow().translated[..2].to_vec();
    assert_eq!(back[8], iova(fds[0]));
    assert_eq!(back[9], iova(fds[1]) + 4095);
    assert_eq!(kernel.borrow().translated_len, 2);
}

#[test]
fn a_raw_value_in_an_address_register_never_reaches_the_kernel() {
    // The caller writes another descriptor's number (and an offset) into the register; the driver
    // replaces it with the descriptor of the buffer it names.
    let (mut service, kernel) = service();
    let mut claim = service.claim(Block::Vepu2, 0).unwrap();
    let held = claim.add_buffer(memfd(4096)).unwrap();
    let held_fd = {
        let fd = claim.remove_buffer(held).unwrap();
        let raw = fd.as_raw_fd() as u32;
        (claim.add_buffer(fd).unwrap(), raw)
    };
    let mut regs = regs();
    regs[8] = (5000 << 10) | 3; // descriptor 3, offset 5000
    let addresses = [Address {
        register: 8,
        buffer: held_fd.0,
        offset: 0,
    }];
    claim
        .submit(&Job {
            identity: 1,
            first: 0,
            registers: &regs,
            more: &[],
            addresses: &addresses,
            read: None,
        })
        .unwrap();
    let k = kernel.borrow();
    assert_eq!(k.translated[..k.translated_len], [held_fd.1]);
    assert_eq!(k.tasks[k.head].regs[8], iova(held_fd.1));
}

fn submit_one(
    block: Block,
    first: u16,
    registers: &[u32],
    addresses: impl Fn(BufferId) -> Vec<Address>,
    read: Option<ReadBack>,
) -> Result<(), SubmitError> {
    let (mut service, kernel) = service();
    let mut claim = service.claim(block, 0).unwrap();
    let buffer = claim.add_buffer(memfd(4096)).unwrap();
    let calls = kernel.borrow().calls;
    let addresses = addresses(buffer);
    let result = claim.submit(&Job {
        identity: 0,
        first,
        registers,
        more: &[],
        addresses: &addresses,
        read,
    });
    if result.is_err() {
        assert_eq!(
            kernel.borrow().calls,
            calls,
            "a refused job touches nothing"
        );
    }
    result
}

fn at(register: u16, offset: u32) -> impl Fn(BufferId) -> Vec<Address> {
    move |buffer| {
        vec![Address {
            register,
            buffer,
            offset,
        }]
    }
}

fn refused(result: Result<(), SubmitError>) -> JobError {
    match result {
        Err(SubmitError::Job(e)) => e,
        other => panic!("expected a refused job, got {other:?}"),
    }
}

#[test]
fn an_offset_must_be_inside_its_buffer() {
    let regs = regs();
    assert!(submit_one(Block::Vepu2, 0, &regs, at(8, 4095), None).is_ok());
    assert_eq!(
        refused(submit_one(Block::Vepu2, 0, &regs, at(8, 4096), None)),
        JobError::OffsetOutside {
            register: 8,
            offset: 4096,
            size: 4096
        }
    );
    assert!(matches!(
        refused(submit_one(Block::Vepu2, 0, &regs, at(8, u32::MAX), None)),
        JobError::OffsetOutside { .. }
    ));
}

#[test]
fn writes_and_reads_stay_inside_the_block() {
    let regs = regs();
    let vepu2 = Block::Vepu2.facts().words() as u16; // 184
    // The last 32 registers: fine; one further: refused.
    assert!(submit_one(Block::Vepu2, vepu2 - 32, &regs, at(vepu2 - 1, 0), None).is_ok());
    assert_eq!(
        refused(submit_one(
            Block::Vepu2,
            vepu2 - 31,
            &regs,
            at(vepu2 - 1, 0),
            None
        )),
        JobError::WriteOutside
    );
    assert_eq!(
        refused(submit_one(Block::Vepu2, u16::MAX, &regs, at(0, 0), None)),
        JobError::WriteOutside
    );
    assert_eq!(
        refused(submit_one(Block::Vepu2, 0, &[], at(0, 0), None)),
        JobError::WriteOutside
    );
    // The JPEG decoder has 42 registers, though the kernel's array holds 70.
    assert_eq!(
        refused(submit_one(Block::JpegDecoder, 16, &regs, at(16, 0), None)),
        JobError::WriteOutside
    );
    // vdpu2 skips what ends before register 50.
    assert_eq!(
        refused(submit_one(Block::Vdpu2, 0, &regs, at(8, 0), None)),
        JobError::WriteBeforeFirst
    );
    let read = |first, count| Some(ReadBack { first, count });
    assert!(submit_one(Block::Vepu2, 0, &regs, at(8, 0), read(vepu2 - 1, 1)).is_ok());
    assert_eq!(
        refused(submit_one(Block::Vepu2, 0, &regs, at(8, 0), read(vepu2, 1))),
        JobError::ReadOutside
    );
    assert_eq!(
        refused(submit_one(Block::Vepu2, 0, &regs, at(8, 0), read(0, 0))),
        JobError::ReadOutside
    );
    assert_eq!(
        refused(submit_one(
            Block::Vepu2,
            0,
            &regs,
            at(8, 0),
            read(u16::MAX, u16::MAX)
        )),
        JobError::ReadOutside
    );
}

#[test]
fn addresses_are_registers_of_the_write_named_once() {
    let regs = regs();
    assert_eq!(
        refused(submit_one(Block::Vepu2, 0, &regs, at(32, 0), None)),
        JobError::AddressOutsideWrite { register: 32 }
    );
    assert_eq!(
        refused(submit_one(Block::Vepu2, 0, &regs, |_| vec![], None)),
        JobError::NoTableAddress
    );
    let twice = |buffer| {
        vec![
            Address {
                register: 4,
                buffer,
                offset: 0,
            },
            Address {
                register: 4,
                buffer,
                offset: 16,
            },
        ]
    };
    assert_eq!(
        refused(submit_one(Block::Vepu2, 0, &regs, twice, None)),
        JobError::DuplicateAddress { register: 4 }
    );
    let many = |buffer| {
        (0..=MAX_ADDRESSES as u16)
            .map(|register| Address {
                register,
                buffer,
                offset: 0,
            })
            .collect()
    };
    let wide = [0u32; 100];
    assert_eq!(
        refused(submit_one(Block::Vepu2, 0, &wide, many, None)),
        JobError::TooManyAddresses
    );
    let stale = |buffer: BufferId| {
        vec![Address {
            register: 4,
            buffer: BufferId {
                generation: buffer.generation.wrapping_add(1),
                ..buffer
            },
            offset: 0,
        }]
    };
    assert_eq!(
        refused(submit_one(Block::Vepu2, 0, &regs, stale, None)),
        JobError::UnknownBuffer { register: 4 }
    );
}

#[test]
fn a_direct_register_must_be_an_address_in_its_mode_and_leaves_the_table() {
    let (mode, dmv) = (53u16, 62u16);
    // Registers 50..82 of vdpu2; register 53's low nibble 0 is H.264.
    let mut regs = [0u32; 32];
    let h264 = |regs: &mut [u32; 32], fmt: u32| regs[usize::from(mode - 50)] = 0x1230 | fmt;
    h264(&mut regs, 0);
    // Not an address: refused, since the kernel would take its low bits as a descriptor.
    assert_eq!(
        refused(submit_one(Block::Vdpu2, 50, &regs, at(64, 0), None)),
        JobError::DirectNotAddress { register: dmv }
    );
    // The only address: refused, since the table would be empty.
    assert_eq!(
        refused(submit_one(Block::Vdpu2, 50, &regs, at(dmv, 0), None)),
        JobError::NoTableAddress
    );
    // In another mode the register is an ordinary one.
    h264(&mut regs, 5);
    assert!(submit_one(Block::Vdpu2, 50, &regs, at(64, 0), None).is_ok());

    // With both: the direct one is translated once, outside the table, with its offset.
    h264(&mut regs, 0);
    let (mut service, kernel) = service();
    let mut claim = service.claim(Block::Vdpu2, 0).unwrap();
    let stream = claim.add_buffer(memfd(4096)).unwrap();
    let mv = claim.add_buffer(memfd(8192)).unwrap();
    let addresses = [
        Address {
            register: 64,
            buffer: stream,
            offset: 0,
        },
        Address {
            register: dmv,
            buffer: mv,
            offset: 4100,
        },
    ];
    claim
        .submit(&Job {
            identity: 9,
            first: 50,
            registers: &regs,
            more: &[],
            addresses: &addresses,
            read: Some(ReadBack {
                first: 50,
                count: 32,
            }),
        })
        .unwrap();
    assert_eq!(kernel.borrow().table[..kernel.borrow().table_len], [64]);
    let mut back = [0u32; 32];
    claim.complete(&mut back).unwrap();
    let k = kernel.borrow();
    assert_eq!(k.translated_len, 2);
    assert_eq!(back[64 - 50], iova(k.translated[0]));
    assert_eq!(back[usize::from(dmv - 50)], iova(k.translated[1]) + 4100);
}

#[test]
fn completions_come_in_order_with_their_read_back() {
    let (mut service, kernel) = service();
    let mut claim = service.claim(Block::JpegEncoder, 0).unwrap();
    let buffer = claim.add_buffer(memfd(4096)).unwrap();
    let mut back = [0u32; 4];
    assert!(matches!(
        claim.complete(&mut back),
        Err(CompleteError::Nothing)
    ));
    for identity in 0..READ_BACK_SLOTS as u64 {
        let regs = [identity as u32 * 100, 0, 0, 0];
        claim
            .submit(&Job {
                identity,
                first: 0,
                registers: &regs,
                more: &[],
                addresses: &[Address {
                    register: 3,
                    buffer,
                    offset: 0,
                }],
                read: Some(ReadBack { first: 0, count: 1 }),
            })
            .unwrap();
    }
    let regs = [0u32; 4];
    let full = claim.submit(&Job {
        identity: 99,
        first: 0,
        registers: &regs,
        more: &[],
        addresses: &[],
        read: None,
    });
    assert!(matches!(full, Err(SubmitError::Full)));
    assert!(matches!(
        claim.remove_buffer(buffer),
        Err(BufferError::Busy)
    ));
    assert!(matches!(
        claim.complete(&mut []),
        Err(CompleteError::TooSmall { need: 1 })
    ));
    for identity in 0..READ_BACK_SLOTS as u64 {
        let done = claim.complete(&mut back).unwrap();
        assert_eq!(done.identity, identity);
        assert_eq!(back[0], identity as u32 * 100);
    }
    assert_eq!(claim.in_flight(), 0);
    // Used, so its mapping is dropped when it is given back.
    let fd = claim.remove_buffer(buffer).unwrap();
    let k = kernel.borrow();
    assert_eq!(k.released[..k.released_len], [fd.as_raw_fd() as u32]);
}

#[test]
fn a_refused_job_leaves_nothing_in_flight() {
    let (mut service, kernel) = service();
    let mut claim = service.claim(Block::Vepu1, 0).unwrap();
    let buffer = claim.add_buffer(memfd(4096)).unwrap();
    let regs = [0u32; 8];
    let addresses = [Address {
        register: 0,
        buffer,
        offset: 0,
    }];
    let job = Job {
        identity: 1,
        first: 0,
        registers: &regs,
        more: &[],
        addresses: &addresses,
        read: None,
    };
    kernel.borrow_mut().fail_send = Some(22);
    assert!(matches!(claim.submit(&job), Err(SubmitError::Device(_))));
    assert_eq!(claim.in_flight(), 0);
    // Never mapped, so not released.
    claim.remove_buffer(buffer).unwrap();
    assert_eq!(kernel.borrow().released_len, 0);
}

#[test]
fn dropping_a_claim_waits_for_its_jobs_then_unmaps() {
    let (mut service, kernel) = service();
    {
        let mut claim = service.claim(Block::Vepu1, 0).unwrap();
        let buffer = claim.add_buffer(memfd(4096)).unwrap();
        let regs = [0u32; 8];
        for _ in 0..3 {
            claim
                .submit(&Job {
                    identity: 1,
                    first: 0,
                    registers: &regs,
                    more: &[],
                    addresses: &[Address {
                        register: 2,
                        buffer,
                        offset: 0,
                    }],
                    read: None,
                })
                .unwrap();
        }
    }
    let k = kernel.borrow();
    assert_eq!((k.polls, k.queued, k.resets), (3, 0, 1));
    assert_eq!(k.last[0].0, Command::ResetSession.code());
}

#[test]
fn submitting_and_completing_allocate_nothing() {
    let (mut service, _kernel) = service();
    let mut claim = service.claim(Block::Vepu2, 0).unwrap();
    let a = claim.add_buffer(memfd(4096)).unwrap();
    let b = claim.add_buffer(memfd(4096)).unwrap();
    let regs = regs();
    let addresses = [
        Address {
            register: 8,
            buffer: a,
            offset: 64,
        },
        Address {
            register: 9,
            buffer: b,
            offset: 0,
        },
    ];
    let job = Job {
        identity: 0,
        first: 0,
        registers: &regs,
        more: &[],
        addresses: &addresses,
        read: Some(ReadBack {
            first: 0,
            count: 32,
        }),
    };
    let mut back = [0u32; 32];
    let ((), n) = allocations(|| {
        for _ in 0..100 {
            claim.submit(&job).unwrap();
            claim.complete(&mut back).unwrap();
        }
    });
    assert_eq!(n, 0);
}

// ---------------------------------------------------------------------------------------------
// Several windows

/// The first window of the first-generation encoder: 780 words, and the second at word 0x4000.
const RKVENC_L2_FIRST: u16 = 0x4000;

#[test]
fn a_job_writes_its_second_window_raw_and_reads_it_back() {
    let (mut service, kernel) = service();
    let mut claim = service.claim(Block::Rkvenc, 0).unwrap();
    let input = claim.add_buffer(memfd(4096)).unwrap();
    let regs = regs();
    let l2: [u32; 8] = std::array::from_fn(|i| 0x5a00_0000 | i as u32);
    let addresses = [Address {
        register: 8,
        buffer: input,
        offset: 0,
    }];
    let more = [Run {
        first: RKVENC_L2_FIRST,
        registers: &l2,
    }];
    claim
        .submit(&Job {
            identity: 5,
            first: 0,
            registers: &regs,
            more: &more,
            addresses: &addresses,
            read: Some(ReadBack {
                first: RKVENC_L2_FIRST,
                count: 8,
            }),
        })
        .unwrap();
    {
        // The table, the two writes, the read: one message per run, each at its own offset.
        let k = kernel.borrow();
        let cmds: Vec<u32> = k.last[..k.last_len].iter().map(|c| c.0).collect();
        assert_eq!(
            cmds,
            [
                Command::InitTransTable.code(),
                Command::SetRegWrite.code(),
                Command::SetRegWrite.code(),
                Command::SetRegRead.code(),
            ]
        );
        assert_eq!(k.table[..k.table_len], [8]);
    }
    let mut back = [0u32; 8];
    claim.complete(&mut back).unwrap();
    assert_eq!(back, l2);
    // The second window's words were written raw: nothing translated there.
    assert_eq!(kernel.borrow().translated_len, 1);
}

#[test]
fn a_write_must_lie_inside_one_window_and_runs_must_not_overlap() {
    let regs = regs();
    // The first window ends at word 780; a write from 779 for two words crosses it.
    let crossing = [0u32; 2];
    assert!(matches!(
        refused(submit_runs(
            Block::Rkvenc,
            779,
            &crossing,
            &[],
            at(8, 0),
            None
        )),
        JobError::WriteOutside
    ));
    // A write that starts in the first window and runs into the second is refused too.
    let past = [0u32; 2];
    assert!(matches!(
        refused(submit_runs(
            Block::Rkvenc,
            0x4000 - 1,
            &past,
            &[],
            at(8, 0),
            None
        )),
        JobError::WriteOutside
    ));
    // Two runs that name the same register.
    let two = [1u32, 2];
    let more = [Run {
        first: 1,
        registers: &two,
    }];
    assert!(matches!(
        refused(submit_runs(Block::Rkvenc, 0, &regs, &more, at(8, 0), None)),
        JobError::WriteOverlap { register: 1 }
    ));
    // An address in the second window: the kernel does not translate it.
    let l2 = [0u32; 4];
    let more = [Run {
        first: RKVENC_L2_FIRST,
        registers: &l2,
    }];
    assert!(matches!(
        refused(submit_runs(
            Block::Rkvenc,
            0,
            &regs,
            &more,
            at(RKVENC_L2_FIRST, 0),
            None
        )),
        JobError::NotTranslated { .. }
    ));
    // An address outside every run.
    assert!(matches!(
        refused(submit_runs(Block::Rkvenc, 0, &regs, &[], at(40, 0), None)),
        JobError::AddressOutsideWrite { register: 40 }
    ));
    // Nine runs: past MAX_RUNS.
    let one = [0u32; 1];
    let many: Vec<Run<'_>> = (0..8)
        .map(|i| Run {
            first: 200 + 2 * i,
            registers: &one,
        })
        .collect();
    assert!(matches!(
        refused(submit_runs(Block::Rkvenc, 0, &regs, &many, at(8, 0), None)),
        JobError::TooManyRuns
    ));
}

/// [`submit_one`] with more runs after the first.
fn submit_runs(
    block: Block,
    first: u16,
    registers: &[u32],
    more: &[Run<'_>],
    addresses: impl Fn(BufferId) -> Vec<Address>,
    read: Option<ReadBack>,
) -> Result<(), SubmitError> {
    let (mut service, kernel) = service();
    let mut claim = service.claim(block, 0).unwrap();
    let buffer = claim.add_buffer(memfd(4096)).unwrap();
    let calls = kernel.borrow().calls;
    let addresses = addresses(buffer);
    let result = claim.submit(&Job {
        identity: 0,
        first,
        registers,
        more,
        addresses: &addresses,
        read,
    });
    assert_eq!(
        kernel.borrow().calls,
        calls,
        "a refused job touches nothing"
    );
    result
}
