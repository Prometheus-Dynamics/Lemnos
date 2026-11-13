//! The write-side builder: every refusal, the job it makes, and that the steady state allocates
//! nothing. Buffers are sealed memfds the claim holds, as rk-mpp's own tests make them.

use std::io;
use std::os::fd::BorrowedFd;
use std::sync::{Arc, Mutex};

use lemnos_driver_rk_mpp::played::{Kernel, iova};
use lemnos_driver_rk_mpp::{Block, Device, Message, Service, Session};
use lemnos_driver_rkvenc::layout::{AddressReg, Region};
use lemnos_driver_rkvenc::vepu541_h264::{READ_BACK, RegisterSet, SetError, StreamRef, ret, set};
use lemnos_linux_sys::dma_buf;
use lemnos_measure::test_alloc::{Counting, allocations};

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// The played kernel of rk-mpp: every block present, the messages answered as the kernel would.
/// No device node is opened. The kernel is shared, so a test can read what it was given.
struct Played(Arc<Mutex<Kernel>>);

impl Session for Played {
    fn send(&mut self, messages: &mut [Message<'_>]) -> io::Result<()> {
        self.0.lock().unwrap().call(messages)
    }

    fn read_back(&mut self, slot: usize, into: &mut [u8]) {
        let n = into.len();
        into.copy_from_slice(&self.0.lock().unwrap().areas[slot][..n]);
    }
}

struct PlayedDevice(Arc<Mutex<Kernel>>);

impl PlayedDevice {
    fn new() -> (Self, Arc<Mutex<Kernel>>) {
        let all = Block::ALL
            .into_iter()
            .fold(0, |bits, b| bits | 1 << b.facts().client);
        let kernel = Arc::new(Mutex::new(Kernel::new(all)));
        (Self(Arc::clone(&kernel)), kernel)
    }
}

impl Device for PlayedDevice {
    type Session = Played;

    fn open(&mut self) -> io::Result<Played> {
        Ok(Played(Arc::clone(&self.0)))
    }

    fn confined(&mut self, _: Block) -> io::Result<bool> {
        Ok(true)
    }

    fn buffer_size(&mut self, fd: BorrowedFd<'_>) -> io::Result<u64> {
        dma_buf::size(fd)
    }
}

/// A buffer the claim holds, as the job names it.
fn buffer() -> lemnos_driver_rk_mpp::BufferId {
    let (device, _) = PlayedDevice::new();
    let mut service = Service::new(device);
    let mut claim = service.claim(Block::Vepu2, 0).unwrap();
    claim
        .add_buffer(dma_buf::sealed_memfd(c"rkvenc-test", 1 << 20).unwrap())
        .unwrap()
}

#[test]
fn a_field_takes_its_bits_in_place() {
    let mut regs = RegisterSet::new();
    regs.set(set::reg001::LKT_NUM, 0xab).unwrap();
    regs.set(set::reg001::RKVENC_CMD, 3).unwrap();
    assert_eq!(regs.words()[1], 0x3ab);
}

#[test]
fn an_unsigned_field_refuses_a_value_too_wide() {
    let mut regs = RegisterSet::new();
    regs.set(set::reg001::LKT_NUM, 255).unwrap();
    assert_eq!(
        regs.set(set::reg001::LKT_NUM, 256),
        Err(SetError::Range {
            value: 256,
            width: 8,
            signed: false
        })
    );
    assert!(matches!(
        regs.set(set::reg001::LKT_NUM, -1),
        Err(SetError::Range { .. })
    ));
    // A refused value leaves the field as it was.
    assert_eq!(regs.words()[1] & 0xff, 255);
}

#[test]
fn a_signed_field_takes_its_negative_range() {
    let field = set::reg053::QP_ADJ0;
    assert!(field.signed);
    let mut regs = RegisterSet::new();
    regs.set(field, 15).unwrap();
    regs.set(field, -16).unwrap();
    let mask = field.mask() << field.lsb;
    assert_eq!((regs.words()[53] & mask) >> field.lsb, 0x10);
    assert!(matches!(
        regs.set(field, 16),
        Err(SetError::Range { signed: true, .. })
    ));
    assert!(matches!(
        regs.set(field, -17),
        Err(SetError::Range { signed: true, .. })
    ));
}

#[test]
fn a_read_only_register_is_refused() {
    let mut regs = RegisterSet::new();
    assert_eq!(regs.set(set::reg000::SUB_VER, 1), Err(SetError::ReadOnly));
    assert_eq!(regs.words()[0], 0);
}

#[test]
fn a_read_back_field_is_outside_the_write_side() {
    let mut regs = RegisterSet::new();
    assert_eq!(
        regs.set(ret::st_bsl::BS_LGTH, 1),
        Err(SetError::OutsideWrite)
    );
}

#[test]
fn an_address_takes_a_buffer_at_an_aligned_offset() {
    let id = buffer();
    let mut regs = RegisterSet::new();
    regs.set_address(set::reg078::CMVW_ADDR, id, 2048).unwrap();
    let job = regs.job(1).unwrap();
    assert_eq!(job.addresses.len(), 1);
    assert_eq!(
        (job.addresses[0].register, job.addresses[0].offset),
        (78, 2048)
    );
}

#[test]
fn a_misaligned_offset_is_refused_by_the_headers_alignment() {
    let id = buffer();
    let mut regs = RegisterSet::new();
    assert_eq!(
        regs.set_address(set::reg078::CMVW_ADDR, id, 512),
        Err(SetError::Misaligned { align: 1024 })
    );
    assert!(regs.stream_refs().is_empty());
    assert!(regs.job(0).unwrap().addresses.is_empty());
}

#[test]
fn a_register_takes_one_buffer() {
    let id = buffer();
    let mut regs = RegisterSet::new();
    regs.set_address(set::reg070::ADR_SRC0, id, 0).unwrap();
    assert_eq!(
        regs.set_address(set::reg070::ADR_SRC0, id, 0),
        Err(SetError::DuplicateAddress)
    );
}

#[test]
fn the_address_list_is_bounded_by_the_rk_mpp_job() {
    let id = buffer();
    let mut regs = RegisterSet::new();
    for word in 0..64 {
        regs.set_address(AddressReg::new(Region::Set, word, 1), id, 0)
            .unwrap();
    }
    assert_eq!(
        regs.set_address(AddressReg::new(Region::Set, 64, 1), id, 0),
        Err(SetError::TooManyAddresses)
    );
}

#[test]
fn a_job_carries_the_whole_write_side_and_the_read_back() {
    let id = buffer();
    let mut regs = RegisterSet::new();
    regs.set(set::reg001::LKT_NUM, 9).unwrap();
    regs.set_address(set::reg070::ADR_SRC0, id, 4096).unwrap();
    let job = regs.job(7).unwrap();
    assert_eq!(job.identity, 7);
    assert_eq!(job.first, 0);
    assert_eq!(job.registers.len(), 132);
    assert_eq!(job.registers[1], 9);
    assert_eq!(job.read, Some(READ_BACK));
    assert_eq!(job.addresses.len(), 1);
    assert_eq!(
        (job.addresses[0].register, job.addresses[0].offset),
        (70, 4096)
    );
}

#[test]
fn a_set_names_one_kind_of_buffer() {
    let id = buffer();
    let mut regs = RegisterSet::new();
    regs.set_stream_address(set::reg070::ADR_SRC0, 2, 0)
        .unwrap();
    assert_eq!(
        regs.set_address(set::reg070::ADR_SRC0, id, 0),
        Err(SetError::DuplicateAddress)
    );
    // A second kind of buffer in the same set: neither form takes it whole.
    regs.set_address(set::reg078::CMVW_ADDR, id, 1024).unwrap();
    assert!(matches!(regs.job(1), Err(SetError::StreamBuffers)));
    assert!(matches!(regs.codec_job(), Err(SetError::ClaimBuffers)));
    let mut claims = RegisterSet::new();
    claims.set_address(set::reg070::ADR_SRC0, id, 0).unwrap();
    assert!(matches!(claims.codec_job(), Err(SetError::ClaimBuffers)));
}

#[test]
fn a_codec_job_carries_the_write_side_and_the_stream_references() {
    let mut regs = RegisterSet::new();
    regs.set(set::reg001::LKT_NUM, 9).unwrap();
    regs.set_stream_address(set::reg070::ADR_SRC0, 2, 0)
        .unwrap();
    regs.set_stream_address(set::reg078::CMVW_ADDR, 3, 1024)
        .unwrap();
    let job = regs.codec_job().unwrap();
    assert_eq!(job.write.len(), 132);
    assert_eq!(job.write[1], 9);
    assert_eq!(job.read, READ_BACK);
    assert_eq!(
        job.refs,
        &[
            StreamRef {
                register: 70,
                buffer: 2,
                offset: 0
            },
            StreamRef {
                register: 78,
                buffer: 3,
                offset: 1024
            },
        ]
    );
}

#[test]
fn clear_empties_the_set_for_the_next_job() {
    let id = buffer();
    let mut regs = RegisterSet::new();
    regs.set(set::reg001::LKT_NUM, 9).unwrap();
    regs.set_address(set::reg070::ADR_SRC0, id, 0).unwrap();
    regs.clear();
    assert!(regs.words().iter().all(|&w| w == 0));
    assert!(regs.stream_refs().is_empty());
    assert!(regs.job(0).unwrap().addresses.is_empty());
}

#[test]
fn the_steady_state_allocates_nothing() {
    let id = buffer();
    let mut regs = RegisterSet::new();
    // The first job sizes what the set keeps; the measured one must not allocate.
    regs.set_address(set::reg070::ADR_SRC0, id, 0).unwrap();
    regs.clear();
    let (_, n) = allocations(|| {
        for i in 0..64u32 {
            regs.set(set::reg053::QP_ADJ0, i64::from(i % 16)).unwrap();
            regs.set(set::reg001::LKT_NUM, i64::from(i)).unwrap();
            regs.set_address(set::reg070::ADR_SRC0, id, 0).unwrap();
            let job = regs.job(u64::from(i)).unwrap();
            std::hint::black_box(job.registers.len());
            regs.clear();
        }
    });
    assert_eq!(n, 0);
}

#[test]
fn the_write_side_ends_at_the_last_writable_register() {
    // The job writes the words up to the end of OSD_ADDR, the last writable register.
    let last = set::reg124_131::REGISTER;
    assert_eq!(last.word + last.words, 132);
}

#[test]
fn a_reserved_encoding_is_refused() {
    let mut regs = RegisterSet::new();
    // SRC_CFMT lists 0, 1, 2 and 4 to 9; 3 and 10 to 15 are reserved, and 16 does not fit the field.
    regs.set(set::reg017::SRC_CFMT, 9).unwrap();
    assert_eq!(
        regs.set(set::reg017::SRC_CFMT, 3),
        Err(SetError::Reserved { value: 3 })
    );
    assert_eq!(
        regs.set(set::reg017::SRC_CFMT, 10),
        Err(SetError::Reserved { value: 10 })
    );
    assert!(matches!(
        regs.set(set::reg017::SRC_CFMT, 16),
        Err(SetError::Range { .. })
    ));
}

#[test]
fn an_encoder_job_runs_end_to_end_on_the_rkvenc_block() {
    let (device, kernel) = PlayedDevice::new();
    let mut service = Service::new(device);
    let mut claim = service.claim(Block::Rkvenc, 0).unwrap();
    let source = claim
        .add_buffer(dma_buf::sealed_memfd(c"rkvenc-src", 1 << 20).unwrap())
        .unwrap();
    let recon = claim
        .add_buffer(dma_buf::sealed_memfd(c"rkvenc-rec", 1 << 20).unwrap())
        .unwrap();
    let mut regs = RegisterSet::new();
    regs.set(set::reg017::SRC_CFMT, 4).unwrap();
    regs.set(set::reg001::LKT_NUM, 0xab).unwrap();
    regs.set_address(set::reg070::ADR_SRC0, source, 0).unwrap();
    regs.set_address(set::reg078::CMVW_ADDR, recon, 1024)
        .unwrap();
    claim.submit(&regs.job(1).unwrap()).unwrap();
    let mut back = vec![0u32; 780];
    let done = claim.complete(&mut back).unwrap();
    assert_eq!(
        (done.identity, done.registers),
        (1, usize::from(READ_BACK.count))
    );
    // The read-back starts at word 7. SRC_CFMT (word 17, bit 2) holds 4.
    assert_eq!(back[17 - 7], 4 << 2);
    // The two address registers hold the device addresses the kernel gave the buffers, the
    // second one 1024 bytes on.
    let fds: Vec<u32> = kernel.lock().unwrap().translated[..2].to_vec();
    assert_eq!(back[70 - 7], iova(fds[0]));
    assert_eq!(back[78 - 7], iova(fds[1]).wrapping_add(1024));
}
