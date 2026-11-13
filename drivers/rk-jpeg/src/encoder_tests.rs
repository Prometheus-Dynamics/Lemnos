//! The encoder's register numbers against the header layout `lemnos-driver-rkjpege` pins, and a
//! job it builds run on rk-mpp's played kernel as the JPEG encoder block.

use std::io;
use std::os::fd::BorrowedFd;
use std::sync::{Arc, Mutex};

use lemnos_driver_rk_mpp::played::{Kernel, iova};
use lemnos_driver_rk_mpp::{Block, Device as ServiceDevice, Message, Service, Session};
use lemnos_linux_sys::dma_buf;

use crate::encoder::{Buffer, RegisterSet, Source, WRITE_WORDS, vpu720};
use crate::regs::{Variant, enc};

#[test]
fn the_encoder_numbers_are_the_header_layouts() {
    use vpu720::{
        reg001_enc_strt, reg004_int_en, reg005_int_msk, reg006_int_clr, reg029_sw_enc_rsl,
    };
    use vpu720::{reg007_int_state, reg016_adr_qtbl, reg017_adr_bsbt, reg022_adr_src0};
    use vpu720::{reg009_lkt_base_addr, reg021_adr_ecs_len};
    use vpu720::{
        reg018_adr_bsbb, reg019_adr_bsbr, reg020_adr_bsbs, reg023_adr_src1, reg024_adr_src2,
    };
    assert_eq!(reg001_enc_strt::WORD.index as u32, enc::START_EN_INDEX);
    assert_eq!(reg004_int_en::WORD.index as u32, enc::INT_EN_INDEX);
    assert_eq!(reg005_int_msk::WORD.index as u32, enc::INT_MASK_INDEX);
    assert_eq!(reg006_int_clr::WORD.index as u32, enc::INT_CLR_INDEX);
    assert_eq!(reg007_int_state::WORD.index as u32, enc::INT_STATUS_INDEX);
    assert_eq!(reg029_sw_enc_rsl::WORD.index as u32, enc::ENC_RSL_INDEX);
    // The header's address registers are the kernel's translation table, word for word.
    let table: Vec<u32> = [
        reg009_lkt_base_addr::ADDRESS.word,
        reg016_adr_qtbl::ADDRESS.word,
        reg017_adr_bsbt::ADDRESS.word,
        reg018_adr_bsbb::ADDRESS.word,
        reg019_adr_bsbr::ADDRESS.word,
        reg020_adr_bsbs::ADDRESS.word,
        reg021_adr_ecs_len::ADDRESS.word,
        reg022_adr_src0::ADDRESS.word,
        reg023_adr_src1::ADDRESS.word,
        reg024_adr_src2::ADDRESS.word,
    ]
    .map(u32::from)
    .to_vec();
    assert_eq!(table, enc::ADDRESS_REGISTERS);
    assert!(WRITE_WORDS <= Variant::EncoderV1.registers() as usize);
}

/// A played session on the JPEG encoder, as the kernel answers it.
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

impl ServiceDevice for PlayedDevice {
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

#[test]
fn a_job_the_encoder_crate_builds_runs_on_the_jpeg_encoder_block() {
    let all = Block::ALL
        .into_iter()
        .fold(0, |bits, b| bits | 1 << b.facts().client);
    let kernel = Arc::new(Mutex::new(Kernel::new(all)));
    let mut service = Service::new(PlayedDevice(Arc::clone(&kernel)));
    let mut claim = service.claim(Block::JpegEncoder, 0).unwrap();
    let mut buf = |name: &std::ffi::CStr| {
        let id = claim
            .add_buffer(dma_buf::sealed_memfd(name, 1 << 20).unwrap())
            .unwrap();
        Buffer { id, size: 1 << 20 }
    };
    let (luma, chroma, qtbl, bits) = (
        buf(c"rkjpeg-luma"),
        buf(c"rkjpeg-chroma"),
        buf(c"rkjpeg-q"),
        buf(c"rkjpeg-bs"),
    );
    let mut regs = RegisterSet::new();
    regs.set_picture(64, 48).unwrap();
    regs.set_source(Source::Yuv420Sp, 64, 64).unwrap();
    regs.set_address(vpu720::reg022_adr_src0::ADDRESS, luma, 0)
        .unwrap();
    regs.set_address(vpu720::reg023_adr_src1::ADDRESS, chroma, 0)
        .unwrap();
    regs.set_address(vpu720::reg016_adr_qtbl::ADDRESS, qtbl, 0)
        .unwrap();
    regs.set_address(vpu720::reg017_adr_bsbt::ADDRESS, bits, 0)
        .unwrap();
    regs.set_address(vpu720::reg018_adr_bsbb::ADDRESS, bits, 65_520)
        .unwrap();
    regs.set_address(vpu720::reg019_adr_bsbr::ADDRESS, bits, 0)
        .unwrap();
    regs.set_address(vpu720::reg020_adr_bsbs::ADDRESS, bits, 600)
        .unwrap();
    // Every word the job writes is inside the block's file, and every address is one the kernel
    // translates.
    let job = regs.job(9).unwrap();
    assert!(job.registers.len() <= Variant::EncoderV1.registers() as usize);
    assert!(job.addresses.iter().all(|a| {
        Variant::EncoderV1
            .address_registers()
            .contains(&u32::from(a.register))
    }));
    claim.submit(&job).unwrap();
    let mut back = vec![0u32; 72];
    let done = claim.complete(&mut back).unwrap();
    assert_eq!((done.identity, done.registers), (9, 72));
    let fds: Vec<u32> = kernel.lock().unwrap().translated[..4].to_vec();
    assert_eq!(back[22 - 7], iova(fds[0]));
    assert_eq!(back[20 - 7], iova(fds[3]).wrapping_add(600));
}
