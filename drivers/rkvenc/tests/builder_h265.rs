//! The VEPU541 H.265 builder: field placement and refusals, address alignment, the second window's
//! run, the steady state, and an encode through the claim on the played kernel's `Block::Rkvenc`.

use std::io;
use std::os::fd::BorrowedFd;
use std::sync::{Arc, Mutex};

use lemnos_driver_rk_mpp::played::{Kernel, iova};
use lemnos_driver_rk_mpp::{Block, Device, MAX_ADDRESSES, Message, Run, Service, Session};
use lemnos_driver_rkvenc::layout::{AddressReg, Region};
use lemnos_driver_rkvenc::vepu541_h265::{
    L2_FIRST, READ_BACK, RegisterSet, SetError, WRITE_WORDS, ret, set,
};
use lemnos_linux_sys::dma_buf;
use lemnos_measure::test_alloc::{Counting, allocations};

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// The played kernel of rk-mpp: every block present, the messages answered as the kernel would.
/// The kernel is shared, so a test can read what it was given.
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
        .add_buffer(dma_buf::sealed_memfd(c"rkvenc-h265-test", 1 << 20).unwrap())
        .unwrap()
}

fn no_spare() -> [Run<'static>; 1] {
    [Run {
        first: 0,
        registers: &[],
    }]
}

#[test]
fn a_field_takes_its_bits_in_place() {
    let mut regs = RegisterSet::new();
    regs.set(set::enc_strt::LKT_NUM, 0xab).unwrap();
    regs.set(set::enc_strt::RKVENC_CMD, 2).unwrap();
    // LKT_NUM is bits 0 to 7 of word 1, RKVENC_CMD bits 8 and 9.
    assert_eq!(regs.words()[1], 0xab | (2 << 8));
    assert_eq!(regs.words()[0], 0);
}

#[test]
fn an_unsigned_field_refuses_a_value_too_wide_or_negative() {
    let mut regs = RegisterSet::new();
    assert_eq!(
        regs.set(set::enc_strt::LKT_NUM, 256),
        Err(SetError::Range {
            value: 256,
            width: 8,
            signed: false
        })
    );
    assert!(matches!(
        regs.set(set::enc_strt::LKT_NUM, -1),
        Err(SetError::Range { value: -1, .. })
    ));
    assert_eq!(regs.words()[1], 0, "a refused value writes nothing");
}

#[test]
fn a_signed_field_takes_its_negative_range() {
    let mut regs = RegisterSet::new();
    // WGHT_B2Y is a 9-bit signed field at bit 0 of word 18.
    regs.set(set::src_udfy::WGHT_B2Y, -256).unwrap();
    regs.set(set::src_udfy::WGHT_B2Y, 255).unwrap();
    assert_eq!(
        regs.set(set::src_udfy::WGHT_B2Y, 256),
        Err(SetError::Range {
            value: 256,
            width: 9,
            signed: true
        })
    );
    assert!(matches!(
        regs.set(set::src_udfy::WGHT_B2Y, -257),
        Err(SetError::Range { value: -257, .. })
    ));
}

#[test]
fn a_read_only_register_is_refused() {
    let mut regs = RegisterSet::new();
    assert_eq!(
        regs.set(set::version::RKVENC_VER, 1),
        Err(SetError::ReadOnly)
    );
    assert_eq!(regs.words()[0], 0);
}

#[test]
fn a_read_back_field_is_outside_the_write_side_and_a_write_field_is_outside_the_second_window() {
    let mut regs = RegisterSet::new();
    // ST_BSL is word 132, past the write side.
    assert_eq!(
        regs.set(ret::st_bsl::BS_LGTH, 1),
        Err(SetError::OutsideWrite)
    );
    assert_eq!(
        regs.set_l2(set::enc_strt::LKT_NUM, 1),
        Err(SetError::OutsideWrite)
    );
}

#[test]
fn an_address_takes_a_buffer_at_an_offset_its_alignment_allows() {
    let id = buffer();
    let mut regs = RegisterSet::new();
    // Every address register of this header is 1-byte aligned: any offset is accepted.
    regs.set_address(set::lkt_addr::LKT_ADDR, id, 4096).unwrap();
    // The alignment check itself: a 16-byte register refuses an offset of 8, takes one of 16.
    let aligned = AddressReg::new(Region::Set, 70, 16);
    assert_eq!(
        regs.set_address(aligned, id, 8),
        Err(SetError::Misaligned { align: 16 })
    );
    regs.set_address(aligned, id, 16).unwrap();
}

#[test]
fn a_register_takes_one_buffer() {
    let id = buffer();
    let mut regs = RegisterSet::new();
    regs.set_address(set::lkt_addr::LKT_ADDR, id, 0).unwrap();
    assert_eq!(
        regs.set_address(set::lkt_addr::LKT_ADDR, id, 0),
        Err(SetError::DuplicateAddress)
    );
}

#[test]
fn the_address_list_is_bounded_by_the_rk_mpp_job() {
    let id = buffer();
    let mut regs = RegisterSet::new();
    for word in 0..MAX_ADDRESSES as u16 {
        regs.set_address(AddressReg::new(Region::Set, word, 1), id, 0)
            .unwrap();
    }
    assert_eq!(
        regs.set_address(AddressReg::new(Region::Set, MAX_ADDRESSES as u16, 1), id, 0),
        Err(SetError::TooManyAddresses)
    );
}

#[test]
fn a_job_carries_the_whole_write_side_and_the_read_back() {
    let mut regs = RegisterSet::new();
    regs.set(set::src_fmt::SRC_CFMT, 4).unwrap();
    let mut spare = no_spare();
    let job = regs.job(9, &mut spare).unwrap();
    assert_eq!((job.identity, job.first), (9, 0));
    assert_eq!(job.registers.len(), WRITE_WORDS);
    assert_eq!(job.registers[17], 4 << 2);
    assert!(job.more.is_empty(), "no second-window word was set");
    assert_eq!(job.read.map(|r| (r.first, r.count)), Some((7, 202)));
    assert_eq!(READ_BACK.count, 202);
}

#[test]
fn a_set_names_one_kind_of_buffer() {
    let id = buffer();
    let mut regs = RegisterSet::new();
    regs.set_stream_address(set::lkt_addr::LKT_ADDR, 0, 0)
        .unwrap();
    assert!(matches!(
        regs.job(1, &mut no_spare()),
        Err(SetError::StreamBuffers)
    ));
    let mut regs = RegisterSet::new();
    regs.set_address(set::lkt_addr::LKT_ADDR, id, 0).unwrap();
    assert!(matches!(regs.codec_job(), Err(SetError::ClaimBuffers)));
}

#[test]
fn clear_empties_the_set_for_the_next_job() {
    let mut regs = RegisterSet::new();
    regs.set(set::enc_strt::LKT_NUM, 7).unwrap();
    regs.set_l2(l2_field(), 0x123).unwrap();
    regs.clear();
    assert_eq!(regs.words()[1], 0);
    assert!(regs.codec_job().unwrap().l2.is_none());
}

fn l2_field() -> lemnos_driver_rkvenc::layout::Field {
    lemnos_driver_rkvenc::vepu541_h265::l2::lvl32_intra_cst_thd0::LVL32_INTRA_CST_THD0
}

#[test]
fn a_second_window_field_takes_its_bits_in_the_l2_words_only() {
    let mut regs = RegisterSet::new();
    regs.set_l2(l2_field(), 0x123).unwrap();
    assert_eq!(regs.words()[1], 0, "the write side is untouched");
    let mut spare = no_spare();
    let job = regs.job(1, &mut spare).unwrap();
    assert_eq!(job.more.len(), 1);
    assert_eq!(job.more[0].first, L2_FIRST);
    // Word 1 of the window: the job writes up to the last word a field set.
    assert_eq!(job.more[0].registers, &[0, 0x123]);
    let plain = RegisterSet::new();
    assert!(plain.job(2, &mut no_spare()).unwrap().more.is_empty());
}

#[test]
fn the_steady_state_allocates_nothing() {
    let id = buffer();
    let mut regs = RegisterSet::new();
    // The first job sizes what the set keeps; the measured one must not allocate.
    regs.set_address(set::adr_srcy_hevc::ADR_SRCY_HEVC, id, 0)
        .unwrap();
    regs.clear();
    let (_, n) = allocations(|| {
        for i in 0..64u32 {
            regs.set(set::src_udfy::WGHT_B2Y, i64::from(i % 16))
                .unwrap();
            regs.set(set::enc_strt::LKT_NUM, i64::from(i)).unwrap();
            regs.set_address(set::adr_srcy_hevc::ADR_SRCY_HEVC, id, 0)
                .unwrap();
            let mut spare = no_spare();
            let job = regs.job(u64::from(i), &mut spare).unwrap();
            std::hint::black_box(job.registers.len());
            regs.clear();
        }
    });
    assert_eq!(n, 0);
}

#[test]
fn an_encoder_job_runs_end_to_end_on_the_rkvenc_block() {
    let (device, kernel) = PlayedDevice::new();
    let mut service = Service::new(device);
    let mut claim = service.claim(Block::Rkvenc, 0).unwrap();
    let source = claim
        .add_buffer(dma_buf::sealed_memfd(c"rkvenc-h265-src", 1 << 20).unwrap())
        .unwrap();
    let recon = claim
        .add_buffer(dma_buf::sealed_memfd(c"rkvenc-h265-rec", 1 << 20).unwrap())
        .unwrap();
    let mut regs = RegisterSet::new();
    regs.set(set::src_fmt::SRC_CFMT, 4).unwrap();
    regs.set(set::enc_strt::LKT_NUM, 0xab).unwrap();
    regs.set_address(set::adr_srcy_hevc::ADR_SRCY_HEVC, source, 0)
        .unwrap();
    regs.set_address(set::roi_addr_hevc::ROI_ADDR_HEVC, recon, 1024)
        .unwrap();
    claim
        .submit(&regs.job(1, &mut no_spare()).unwrap())
        .unwrap();
    let mut back = vec![0u32; 780];
    let done = claim.complete(&mut back).unwrap();
    assert_eq!(
        (done.identity, done.registers),
        (1, usize::from(READ_BACK.count))
    );
    // The read-back starts at word 7. SRC_CFMT (word 17, bit 2) holds 4.
    assert_eq!(back[17 - 7], 4 << 2);
    // The two address registers hold the device addresses the kernel gave the buffers; the second
    // one is 1024 bytes on.
    let fds: Vec<u32> = kernel.lock().unwrap().translated[..2].to_vec();
    assert_eq!(back[70 - 7], iova(fds[0]));
    assert_eq!(back[73 - 7], iova(fds[1]).wrapping_add(1024));
}

#[test]
fn a_second_window_job_runs_end_to_end_on_the_rkvenc_block() {
    let (device, kernel) = PlayedDevice::new();
    let mut service = Service::new(device);
    let mut claim = service.claim(Block::Rkvenc, 0).unwrap();
    let source = claim
        .add_buffer(dma_buf::sealed_memfd(c"rkvenc-h265-src", 1 << 20).unwrap())
        .unwrap();
    let mut regs = RegisterSet::new();
    regs.set(set::src_fmt::SRC_CFMT, 4).unwrap();
    regs.set_address(set::adr_srcy_hevc::ADR_SRCY_HEVC, source, 0)
        .unwrap();
    regs.set_l2(l2_field(), 0x123).unwrap();
    regs.set_l2(
        lemnos_driver_rkvenc::vepu541_h265::l2::lvl32_intra_cst_thd0::LVL32_INTRA_CST_THD1,
        0x456,
    )
    .unwrap();
    claim
        .submit(&regs.job(3, &mut no_spare()).unwrap())
        .unwrap();
    {
        // The kernel's task: the first window (780 words) then the second. The second window's
        // word 1 sits after the first window's words.
        let k = kernel.lock().unwrap();
        let task = &k.tasks[k.head];
        assert_eq!(task.regs[780 + 1], 0x0456_0123);
        // The first window's address was translated; the second window was not.
        assert_eq!(k.translated_len, 1);
        let cmds: Vec<u32> = k.last[..k.last_len].iter().map(|c| c.0).collect();
        assert_eq!(cmds.len(), 4, "table, write, second window, read");
        assert_eq!(k.table[..k.table_len], [70]);
    }
    let mut back = vec![0u32; 780];
    let done = claim.complete(&mut back).unwrap();
    assert_eq!(
        (done.identity, done.registers),
        (3, usize::from(READ_BACK.count))
    );
    assert_eq!(back[17 - 7], 4 << 2);
}
