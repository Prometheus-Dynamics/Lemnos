//! The second-generation encoder's builder and its played job: every refusal, the runs and the
//! read-back, the job on the rkvenc2 block (through rk-mpp's played kernel), and the same job in
//! `codec.block/1`'s form run through rk-mpp's block.

use std::io;
use std::os::fd::BorrowedFd;
use std::sync::{Arc, Mutex};

use lemnos_contract_codec_block::BufferRef;
use lemnos_contract_codec_block::work::{Builder, Work};
use lemnos_driver_codec_jobs::{Block as _, Buffers, Start};
use lemnos_driver_rk_mpp::codec::{MppBlock, TAG_READ, TAG_WRITE};
use lemnos_driver_rk_mpp::played::{Kernel, iova};
use lemnos_driver_rk_mpp::{Block, Device, Message, Run, Service, Session};
use lemnos_driver_rkvenc::layout::{AddressReg, Region};
use lemnos_driver_rkvenc::vepu580_h264::base::reg0022;
use lemnos_driver_rkvenc::vepu580_h264::osd::reg3072;
use lemnos_driver_rkvenc::vepu580_h264::pic::reg0160;
use lemnos_driver_rkvenc::vepu580_h264::{READ_BACK, RegisterSet, SetError, WRITE_RUNS};
use lemnos_hal::StreamEndpoint as _;
use lemnos_hal::mock::MockStream;
use lemnos_linux_sys::dma_buf;

/// The played kernel's session, sharing the kernel with the test.
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

impl Device for PlayedDevice {
    type Session = Played;

    fn open(&mut self) -> io::Result<Played> {
        Ok(Played(Arc::clone(&self.0)))
    }

    fn confined(&mut self, block: Block) -> io::Result<bool> {
        // The claim names its block here: two blocks share the rkvenc client type.
        self.0.lock().unwrap().bound = Some(block);
        Ok(true)
    }

    fn buffer_size(&mut self, fd: BorrowedFd<'_>) -> io::Result<u64> {
        dma_buf::size(fd)
    }
}

fn played() -> (PlayedDevice, Arc<Mutex<Kernel>>) {
    let all = Block::ALL
        .into_iter()
        .fold(0, |bits, b| bits | 1 << b.facts().client);
    let kernel = Arc::new(Mutex::new(Kernel::new(all)));
    (PlayedDevice(Arc::clone(&kernel)), kernel)
}

#[test]
fn a_field_takes_its_bits_in_its_class_and_a_status_or_osd_field_is_refused() {
    let mut regs = RegisterSet::new();
    regs.set(reg0022::TQ8_CKG, 1).unwrap();
    assert_eq!(
        regs.set(reg3072::OSD_LU_INV_EN, 1),
        Err(SetError::OutsideWrite)
    );
}

#[test]
fn an_address_is_only_taken_in_the_translated_picture_class() {
    let mut regs = RegisterSet::new();
    // A picture address is accepted; BASE and OSD addresses are not.
    let base_addr = AddressReg::new(Region::Class(0), 0, 1);
    let osd_addr = AddressReg::new(Region::Class(6), 3092, 16);
    assert_eq!(
        regs.set_stream_address(base_addr, 0, 0),
        Err(SetError::OutsideWrite)
    );
    assert_eq!(
        regs.set_stream_address(osd_addr, 0, 0),
        Err(SetError::OutsideWrite)
    );
    assert_eq!(
        regs.set_stream_address(reg0160::ADR_SRC0, 2, 1 << 22),
        Err(SetError::OffsetTooLarge)
    );
    regs.set_stream_address(reg0160::ADR_SRC0, 0, 0).unwrap();
    assert_eq!(
        regs.set_stream_address(reg0160::ADR_SRC0, 1, 0),
        Err(SetError::DuplicateAddress)
    );
}

#[test]
fn a_job_is_one_run_per_writable_class_and_reads_the_status_class() {
    let mut regs = RegisterSet::new();
    regs.set(reg0022::TQ8_CKG, 1).unwrap();
    let mut spare = [Run {
        first: 0,
        registers: &[],
    }; WRITE_RUNS - 1];
    let job = regs.job(4, &mut spare).unwrap();
    assert_eq!(job.first, 0);
    assert_eq!(job.registers.len(), 23);
    assert_eq!(job.registers[22], 1);
    let firsts: Vec<u16> = job.more.iter().map(|r| r.first).collect();
    assert_eq!(firsts, [160, 1024, 1472, 2048, 2176]);
    assert_eq!(
        job.read.map(|r| (r.first, r.count)),
        Some((READ_BACK.first, READ_BACK.count))
    );
    assert_eq!((READ_BACK.first, READ_BACK.count), (4096, 180));
}

#[test]
fn a_job_runs_end_to_end_on_the_second_generation_block() {
    let (device, kernel) = played();
    let mut service = Service::new(device);
    let mut claim = service.claim(Block::Rkvenc2, 0).unwrap();
    let source = claim
        .add_buffer(dma_buf::sealed_memfd(c"vepu580-src", 1 << 20).unwrap())
        .unwrap();
    let mut regs = RegisterSet::new();
    regs.set(reg0022::TQ8_CKG, 1).unwrap();
    regs.set_address(reg0160::ADR_SRC0, source, 4096).unwrap();
    let mut spare = [Run {
        first: 0,
        registers: &[],
    }; WRITE_RUNS - 1];
    claim.submit(&regs.job(7, &mut spare).unwrap()).unwrap();
    {
        let k = kernel.lock().unwrap();
        // The table names the picture array's index 0 (the word 160 relative to its class).
        assert_eq!(k.table[..k.table_len], [0]);
        let fd = k.translated[0];
        let flat = Block::Rkvenc2.facts().flat(160).unwrap();
        // The descriptor holds the buffer and its offset in the word's bits 10 and up.
        assert_eq!(k.tasks[k.head].regs[flat], iova(fd) + 4096);
    }
    let mut back = vec![0u32; READ_BACK.count as usize];
    let done = claim.complete(&mut back).unwrap();
    assert_eq!((done.identity, done.registers), (7, 180));
}

/// The job in `codec.block/1`'s form, with stream buffer 0 named.
#[test]
fn the_codec_form_runs_through_the_block() {
    let kernel = Arc::new(Mutex::new(Kernel::new(
        Block::ALL
            .into_iter()
            .fold(0, |bits, b| bits | 1 << b.facts().client),
    )));
    let mut block = MppBlock::new(PlayedDevice(Arc::clone(&kernel)), Block::Rkvenc2);
    let stream = MockStream::with_handles(vec![
        dma_buf::sealed_memfd(c"vepu580-codec", 1 << 20).unwrap(),
    ]);
    let mut producer = stream.clone();
    assert_eq!(producer.next_buffer(), Ok(Some(0)));

    let mut regs = RegisterSet::new();
    regs.set(reg0022::TQ8_CKG, 1).unwrap();
    regs.set_stream_address(reg0160::ADR_SRC0, 0, 4096).unwrap();
    let job = regs.codec_job().unwrap();

    let mut bytes = vec![0u8; 16384];
    let mut builder = Builder::new(&mut bytes).unwrap();
    for r in job.refs {
        builder
            .reference(BufferRef {
                slot: u32::from(r.register),
                buffer: r.buffer,
                offset: r.offset,
                length: 0,
            })
            .unwrap();
    }
    for run in &job.runs {
        let mut item = u32::from(run.first).to_le_bytes().to_vec();
        for w in run.registers {
            item.extend_from_slice(&w.to_le_bytes());
        }
        builder.item(TAG_WRITE, &item).unwrap();
    }
    let mut read = u32::from(job.read.first).to_le_bytes().to_vec();
    read.extend_from_slice(&u32::from(job.read.count).to_le_bytes());
    builder.item(TAG_READ, &read).unwrap();
    let len = builder.finish();
    let work = Work::parse(&bytes[..len]).unwrap();

    let buffers = Buffers::naming(&stream, 1);
    let mut back = vec![0u8; 4 * READ_BACK.count as usize];
    match block.start(&work, &buffers, &mut back, 0) {
        Start::Done(n) => assert_eq!(n, 4 * READ_BACK.count as usize),
        other => panic!("the job did not run: {other:?}"),
    }
    let k = kernel.lock().unwrap();
    assert_eq!(k.table[..k.table_len], [0]);
}
