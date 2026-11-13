//! The encoder job builder: the refusals, the job it makes, the steady state, and the job run on
//! rk-mpp's played kernel as the `JpegEncoder` block. Buffers are sealed memfds the claim holds,
//! as rk-mpp's own tests make them.

use std::io;
use std::os::fd::BorrowedFd;
use std::sync::{Arc, Mutex};

use lemnos_driver_rk_mpp::played::{Kernel, iova};
use lemnos_driver_rk_mpp::{Block, Device, Message, Service, Session};
use lemnos_driver_rkjpege::job::{MAX_SIDE, WRITE_WORDS};
use lemnos_driver_rkjpege::layout::AddressReg;
use lemnos_driver_rkjpege::vpu720::{
    reg001_enc_strt, reg009_lkt_base_addr, reg016_adr_qtbl, reg017_adr_bsbt, reg018_adr_bsbb,
    reg019_adr_bsbr, reg020_adr_bsbs, reg022_adr_src0, reg023_adr_src1, reg029_sw_enc_rsl,
    reg032_sw_src_fmt, st_enc,
};
use lemnos_driver_rkjpege::{Buffer, JobError, READ_BACK, RegisterSet, SetError, Source};
use lemnos_linux_sys::dma_buf;
use lemnos_measure::test_alloc::{Counting, allocations};

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// The played kernel of rk-mpp: every block present, the messages answered as the kernel would.
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

    fn confined(&mut self, block: Block) -> io::Result<bool> {
        self.0.lock().unwrap().bound = Some(block);
        Ok(true)
    }

    fn buffer_size(&mut self, fd: BorrowedFd<'_>) -> io::Result<u64> {
        dma_buf::size(fd)
    }
}

/// Buffers a claim holds, one per size, in order. Ids from one claim are distinct, as a job
/// needs them; the claim is dropped here, which the builder never checks (it has no claim).
fn held(sizes: &[u64]) -> Vec<Buffer> {
    let (device, _) = PlayedDevice::new();
    let mut service = Service::new(device);
    let mut claim = service.claim(Block::JpegEncoder, 0).unwrap();
    sizes
        .iter()
        .map(|&size| {
            let id = claim
                .add_buffer(dma_buf::sealed_memfd(c"k1b-test", size).unwrap())
                .unwrap();
            Buffer { id, size }
        })
        .collect()
}

/// One buffer the claim holds, of `size` bytes.
fn buffer(size: u64) -> Buffer {
    held(&[size])[0]
}

const MB: u64 = 1 << 20;

#[test]
fn a_field_takes_its_bits_in_place() {
    let mut regs = RegisterSet::new();
    regs.set(reg001_enc_strt::VEPU_CMD, 1).unwrap();
    regs.set(reg001_enc_strt::LKT_NUM, 0xab).unwrap();
    assert_eq!(regs.words()[1], 0x1ab);
}

#[test]
fn a_value_outside_the_field_or_its_encodings_is_refused() {
    let mut regs = RegisterSet::new();
    regs.set(reg001_enc_strt::LKT_NUM, 255).unwrap();
    assert_eq!(
        regs.set(reg001_enc_strt::LKT_NUM, 256),
        Err(SetError::Range {
            value: 256,
            width: 8
        })
    );
    assert!(matches!(
        regs.set(reg001_enc_strt::LKT_NUM, -1),
        Err(SetError::Range { .. })
    ));
    // The header lists vepu_cmd 0 to 7; 8 is reserved in its 4-bit field.
    assert_eq!(
        regs.set(reg001_enc_strt::VEPU_CMD, 8),
        Err(SetError::Reserved { value: 8 })
    );
    // A refused value leaves the field as it was.
    assert_eq!(regs.words()[1] & 0xff, 255);
}

#[test]
fn a_status_field_is_read_only() {
    let mut regs = RegisterSet::new();
    // The status struct is word 48 and up, past the words a job writes; its state word is 74.
    assert_eq!(
        regs.set(st_enc::JPEG_ENC_STATE, 1),
        Err(SetError::OutsideWrite { word: 74 })
    );
    // The job's words are the base struct only.
    assert_eq!(WRITE_WORDS, 43);
}

#[test]
fn the_builder_owns_the_picture_source_and_addresses() {
    let mut regs = RegisterSet::new();
    assert_eq!(
        regs.set(reg029_sw_enc_rsl::PIC_WD8_M1, 3),
        Err(SetError::Owned { word: 29 })
    );
    assert_eq!(
        regs.set(reg032_sw_src_fmt::SRC_FMT, 6),
        Err(SetError::Owned { word: 32 })
    );
}

#[test]
fn a_picture_is_rounded_to_16_and_its_fill_follows() {
    let mut regs = RegisterSet::new();
    regs.set_picture(1920, 1080).unwrap();
    // 1920 x 1088 encoded: 240 and 136 in 8-pixel units, minus one; fill 0 and 8.
    let w = regs.words();
    assert_eq!(w[29] & 0x1fff, 239);
    assert_eq!((w[29] >> 16) & 0x1fff, 135);
    assert_eq!(w[30] & 0x3f, 0);
    assert_eq!((w[30] >> 16) & 0x3f, 8);
}

#[test]
fn a_picture_outside_the_block_is_refused() {
    let mut regs = RegisterSet::new();
    assert!(matches!(
        regs.set_picture(0, 8),
        Err(SetError::Picture { .. })
    ));
    assert!(matches!(
        regs.set_picture(MAX_SIDE + 1, 8),
        Err(SetError::Picture { .. })
    ));
    regs.set_picture(MAX_SIDE, MAX_SIDE).unwrap();
}

#[test]
fn a_source_needs_its_picture_and_rows_that_fit() {
    let mut regs = RegisterSet::new();
    assert_eq!(
        regs.set_source(Source::Yuv420Sp, 64, 64),
        Err(SetError::NeedsPicture)
    );
    regs.set_picture(64, 48).unwrap();
    // A luma row is 64 bytes; 63 holds less than a row.
    assert_eq!(
        regs.set_source(Source::Yuv420Sp, 63, 64),
        Err(SetError::Stride {
            plane: 0,
            need: 64,
            stride: 63
        })
    );
    // Packed 4:2:2 is two bytes a pixel.
    assert_eq!(
        regs.set_source(Source::Yuyv, 64, 0),
        Err(SetError::Stride {
            plane: 0,
            need: 128,
            stride: 64
        })
    );
    // A refused call leaves the source unset.
    assert_eq!((regs.words()[32] >> 2) & 0xf, 0);
    regs.set_source(Source::Yuv420Sp, 64, 64).unwrap();
    let w = regs.words();
    assert_eq!((w[32] >> 2) & 0xf, 6);
    assert_eq!((w[32] >> 6) & 0x3, 1);
    assert_eq!(w[34] & 0xf_ffff, 64);
    assert_eq!(w[35] & 0x7_ffff, 64);
}

#[test]
fn an_address_takes_a_buffer_at_an_aligned_offset_below_its_size() {
    let buf = buffer(MB);
    let mut regs = RegisterSet::new();
    regs.set_address(reg022_adr_src0::ADDRESS, buf, 0).unwrap();
    regs.set_address(reg016_adr_qtbl::ADDRESS, buf, 128)
        .unwrap();
    assert_eq!(regs.addresses().len(), 2);
    assert_eq!(regs.addresses()[1].offset, 128);
}

#[test]
fn an_address_is_refused_when_the_header_or_the_buffer_says_so() {
    let buf = buffer(MB);
    let mut regs = RegisterSet::new();
    // The link table is refused outright.
    assert_eq!(
        regs.set_address(reg009_lkt_base_addr::ADDRESS, buf, 0),
        Err(SetError::LinkTable)
    );
    // The quantisation table is 128-byte aligned.
    assert_eq!(
        regs.set_address(reg016_adr_qtbl::ADDRESS, buf, 64),
        Err(SetError::Misaligned { align: 128 })
    );
    // The bitstream top is 16-byte aligned; the read pointer is byte aligned.
    assert_eq!(
        regs.set_address(reg017_adr_bsbt::ADDRESS, buf, 8),
        Err(SetError::Misaligned { align: 16 })
    );
    regs.set_address(reg019_adr_bsbr::ADDRESS, buf, 3).unwrap();
    // An offset at the buffer's size is refused, as rk-mpp refuses it.
    assert_eq!(
        regs.set_address(reg022_adr_src0::ADDRESS, buf, MB as u32),
        Err(SetError::OffsetOutside {
            offset: MB as u32,
            size: MB
        })
    );
    // Not an address register of the block: a plain word.
    let plain = AddressReg::new(1, 1);
    assert_eq!(
        regs.set_address(plain, buf, 0),
        Err(SetError::NotAnAddress { word: 1 })
    );
    // A register holds one buffer.
    regs.set_address(reg022_adr_src0::ADDRESS, buf, 0).unwrap();
    assert_eq!(
        regs.set_address(reg022_adr_src0::ADDRESS, buf, 0),
        Err(SetError::DuplicateAddress { word: 22 })
    );
    // A refused address is not placed.
    assert_eq!(regs.addresses().len(), 2);
}

/// A complete set for a 64x48 4:2:0 picture: luma and chroma in their own buffers, the
/// quantisation table in a third, and the bitstream in a fourth.
fn full_set() -> RegisterSet {
    let mut regs = RegisterSet::new();
    regs.set_picture(64, 48).unwrap();
    regs.set_source(Source::Yuv420Sp, 64, 64).unwrap();
    let luma = buffer(MB);
    let chroma = buffer(MB);
    let qtbl = buffer(MB);
    let bits = buffer(MB);
    regs.set_address(reg022_adr_src0::ADDRESS, luma, 0).unwrap();
    regs.set_address(reg023_adr_src1::ADDRESS, chroma, 0)
        .unwrap();
    regs.set_address(reg016_adr_qtbl::ADDRESS, qtbl, 0).unwrap();
    regs.set_address(reg017_adr_bsbt::ADDRESS, bits, 0).unwrap();
    regs.set_address(reg018_adr_bsbb::ADDRESS, bits, 65_520)
        .unwrap();
    regs.set_address(reg019_adr_bsbr::ADDRESS, bits, 0).unwrap();
    regs.set_address(reg020_adr_bsbs::ADDRESS, bits, 600)
        .unwrap();
    regs
}

#[test]
fn a_job_names_the_whole_write_side_its_addresses_and_the_read_back() {
    let regs = full_set();
    let job = regs.job(7).unwrap();
    assert_eq!(job.identity, 7);
    assert_eq!((job.first, job.registers.len()), (0, WRITE_WORDS));
    assert!(job.more.is_empty());
    assert_eq!(job.addresses.len(), 7);
    assert_eq!(job.read, Some(READ_BACK));
    assert_eq!((READ_BACK.first, READ_BACK.count), (7, 72));
}

#[test]
fn a_job_is_refused_until_its_picture_source_and_pointers_are_placed() {
    let buf = buffer(MB);
    let regs = RegisterSet::new();
    assert_eq!(regs.job(0).unwrap_err(), JobError::NoPicture);

    let mut regs = RegisterSet::new();
    regs.set_picture(64, 48).unwrap();
    assert_eq!(regs.job(0).unwrap_err(), JobError::NoSource);

    regs.set_source(Source::Yuv420Sp, 64, 64).unwrap();
    assert_eq!(regs.job(0).unwrap_err(), JobError::Missing { word: 22 });
    regs.set_address(reg022_adr_src0::ADDRESS, buf, 0).unwrap();
    // The chroma plane is not placed.
    assert_eq!(regs.job(0).unwrap_err(), JobError::Missing { word: 23 });
}

#[test]
fn a_plane_that_reaches_past_its_buffer_is_refused() {
    let mut regs = RegisterSet::new();
    regs.set_picture(64, 48).unwrap();
    regs.set_source(Source::Yuv420Sp, 64, 64).unwrap();
    // Luma is 64 x 48 = 3072 bytes; a 3000-byte buffer holds less.
    let small = buffer(3000);
    let big = buffer(MB);
    regs.set_address(reg022_adr_src0::ADDRESS, small, 0)
        .unwrap();
    regs.set_address(reg023_adr_src1::ADDRESS, big, 0).unwrap();
    regs.set_address(reg016_adr_qtbl::ADDRESS, big, 0).unwrap();
    regs.set_address(reg017_adr_bsbt::ADDRESS, big, 0).unwrap();
    regs.set_address(reg018_adr_bsbb::ADDRESS, big, 65_520)
        .unwrap();
    regs.set_address(reg019_adr_bsbr::ADDRESS, big, 0).unwrap();
    regs.set_address(reg020_adr_bsbs::ADDRESS, big, 0).unwrap();
    assert_eq!(
        regs.job(0).unwrap_err(),
        JobError::Plane {
            word: 22,
            need: 3072,
            size: 3000
        }
    );
}

#[test]
fn the_bitstream_is_one_ordered_range() {
    let [big, other] = held(&[MB, MB])[..] else {
        unreachable!()
    };
    let mut regs = RegisterSet::new();
    regs.set_picture(64, 48).unwrap();
    regs.set_source(Source::Yuv400, 64, 0).unwrap();
    regs.set_address(reg022_adr_src0::ADDRESS, big, 0).unwrap();
    regs.set_address(reg016_adr_qtbl::ADDRESS, big, 0).unwrap();
    // The bottom in another buffer.
    regs.set_address(reg017_adr_bsbt::ADDRESS, big, 0).unwrap();
    regs.set_address(reg018_adr_bsbb::ADDRESS, other, 65_520)
        .unwrap();
    regs.set_address(reg019_adr_bsbr::ADDRESS, big, 0).unwrap();
    regs.set_address(reg020_adr_bsbs::ADDRESS, big, 0).unwrap();
    assert_eq!(regs.job(0).unwrap_err(), JobError::BitstreamBuffers);

    // Top not below bottom, in one buffer.
    let mut regs = RegisterSet::new();
    regs.set_picture(64, 48).unwrap();
    regs.set_source(Source::Yuv400, 64, 0).unwrap();
    regs.set_address(reg022_adr_src0::ADDRESS, big, 0).unwrap();
    regs.set_address(reg016_adr_qtbl::ADDRESS, big, 0).unwrap();
    regs.set_address(reg017_adr_bsbt::ADDRESS, big, 4096)
        .unwrap();
    regs.set_address(reg018_adr_bsbb::ADDRESS, big, 4096)
        .unwrap();
    regs.set_address(reg019_adr_bsbr::ADDRESS, big, 4096)
        .unwrap();
    regs.set_address(reg020_adr_bsbs::ADDRESS, big, 4096)
        .unwrap();
    assert_eq!(regs.job(0).unwrap_err(), JobError::BitstreamOrder);
    // A start pointer past the bottom is refused too.
    regs.clear();
    regs.set_picture(64, 48).unwrap();
    regs.set_source(Source::Yuv400, 64, 0).unwrap();
    regs.set_address(reg022_adr_src0::ADDRESS, big, 0).unwrap();
    regs.set_address(reg016_adr_qtbl::ADDRESS, big, 0).unwrap();
    regs.set_address(reg017_adr_bsbt::ADDRESS, big, 0).unwrap();
    regs.set_address(reg018_adr_bsbb::ADDRESS, big, 1024)
        .unwrap();
    regs.set_address(reg019_adr_bsbr::ADDRESS, big, 0).unwrap();
    regs.set_address(reg020_adr_bsbs::ADDRESS, big, 2048)
        .unwrap();
    assert_eq!(regs.job(0).unwrap_err(), JobError::BitstreamOrder);
}

#[test]
fn clear_empties_the_set_for_the_next_job() {
    let mut regs = full_set();
    regs.set(reg001_enc_strt::LKT_NUM, 9).unwrap();
    regs.clear();
    assert!(regs.addresses().is_empty());
    assert!(regs.words().iter().all(|&w| w == 0));
    assert_eq!(regs.job(0).unwrap_err(), JobError::NoPicture);
}

#[test]
fn the_steady_state_allocates_nothing() {
    let buf = buffer(MB);
    let mut regs = RegisterSet::new();
    // The first job sizes what the set keeps; the measured one must not allocate.
    regs.set_address(reg022_adr_src0::ADDRESS, buf, 0).unwrap();
    regs.clear();
    let (_, n) = allocations(|| {
        for i in 0..64u32 {
            regs.set_picture(64, 48).unwrap();
            regs.set_source(Source::Yuv400, 64, 0).unwrap();
            regs.set(reg001_enc_strt::LKT_NUM, i64::from(i)).unwrap();
            regs.set_address(reg022_adr_src0::ADDRESS, buf, 0).unwrap();
            let job = regs.job(u64::from(i));
            std::hint::black_box(job.is_err());
            regs.clear();
        }
    });
    assert_eq!(n, 0);
}

#[test]
fn a_job_runs_end_to_end_on_the_jpeg_encoder_block() {
    let (device, kernel) = PlayedDevice::new();
    let mut service = Service::new(device);
    let mut claim = service.claim(Block::JpegEncoder, 0).unwrap();
    let luma = claim
        .add_buffer(dma_buf::sealed_memfd(c"k1b-luma", MB).unwrap())
        .unwrap();
    let chroma = claim
        .add_buffer(dma_buf::sealed_memfd(c"k1b-chroma", MB).unwrap())
        .unwrap();
    let qtbl = claim
        .add_buffer(dma_buf::sealed_memfd(c"k1b-qtbl", MB).unwrap())
        .unwrap();
    let bits = claim
        .add_buffer(dma_buf::sealed_memfd(c"k1b-bits", MB).unwrap())
        .unwrap();
    let size = |id| claim.buffer_size(id).unwrap();
    let mut regs = RegisterSet::new();
    regs.set_picture(64, 48).unwrap();
    regs.set_source(Source::Yuv420Sp, 64, 64).unwrap();
    regs.set(reg001_enc_strt::LKT_NUM, 0).unwrap();
    regs.set_address(
        reg022_adr_src0::ADDRESS,
        Buffer {
            id: luma,
            size: size(luma),
        },
        0,
    )
    .unwrap();
    regs.set_address(
        reg023_adr_src1::ADDRESS,
        Buffer {
            id: chroma,
            size: size(chroma),
        },
        4096,
    )
    .unwrap();
    regs.set_address(
        reg016_adr_qtbl::ADDRESS,
        Buffer {
            id: qtbl,
            size: size(qtbl),
        },
        0,
    )
    .unwrap();
    let b = Buffer {
        id: bits,
        size: size(bits),
    };
    regs.set_address(reg017_adr_bsbt::ADDRESS, b, 0).unwrap();
    regs.set_address(reg018_adr_bsbb::ADDRESS, b, 65_520)
        .unwrap();
    regs.set_address(reg019_adr_bsbr::ADDRESS, b, 0).unwrap();
    regs.set_address(reg020_adr_bsbs::ADDRESS, b, 600).unwrap();
    claim.submit(&regs.job(1).unwrap()).unwrap();
    let mut back = vec![0u32; 72];
    let done = claim.complete(&mut back).unwrap();
    assert_eq!((done.identity, done.registers), (1, 72));

    // The kernel gave each buffer its device address, in the order they were placed; each
    // address register reads back as its buffer's address plus its offset.
    let fds: Vec<u32> = kernel.lock().unwrap().translated[..4].to_vec();
    let at = |word: u16| back[usize::from(word) - 7];
    assert_eq!(at(22), iova(fds[0]));
    assert_eq!(at(23), iova(fds[1]).wrapping_add(4096));
    assert_eq!(at(16), iova(fds[2]));
    assert_eq!(at(17), iova(fds[3]));
    assert_eq!(at(18), iova(fds[3]).wrapping_add(65_520));
    assert_eq!(at(19), iova(fds[3]));
    assert_eq!(at(20), iova(fds[3]).wrapping_add(600));
    // The picture size and the source format are the values the builder set.
    assert_eq!(at(29) & 0x1fff, 7);
    assert_eq!((at(29) >> 16) & 0x1fff, 5);
    assert_eq!((at(32) >> 2) & 0xf, 6);
}
