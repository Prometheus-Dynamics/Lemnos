//! The job in `codec.block/1`'s form, built from `codec_job()` alone, run through rk-mpp's block
//! on the played kernel: the first window's words, the second window's as its own item, the
//! stream buffer's address placed by the driver, and the read-back.

use std::io;
use std::os::fd::BorrowedFd;
use std::sync::{Arc, Mutex};

use lemnos_contract_codec_block::BufferRef;
use lemnos_contract_codec_block::work::{Builder, Work};
use lemnos_driver_codec_jobs::{Block as _, Buffers, Start};
use lemnos_driver_rk_mpp::codec::{MppBlock, TAG_READ, TAG_WRITE};
use lemnos_driver_rk_mpp::played::Kernel;
use lemnos_driver_rk_mpp::{Block, Device, Message, Session};
use lemnos_driver_rkvenc::layout::{AddressReg, Region};
use lemnos_driver_rkvenc::vepu541_h264::{CodecJob, READ_BACK, RegisterSet, l2, set};
use lemnos_hal::StreamEndpoint as _;
use lemnos_hal::mock::MockStream;
use lemnos_linux_sys::dma_buf;

/// The played kernel, shared with the test.
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

    fn confined(&mut self, _: Block) -> io::Result<bool> {
        Ok(true)
    }

    fn buffer_size(&mut self, fd: BorrowedFd<'_>) -> io::Result<u64> {
        dma_buf::size(fd)
    }
}

/// The work the caller sends for `job`: a reference per stream address, then the write items,
/// one per run, then the read-back.
fn work_for(job: &CodecJob<'_>) -> Vec<u8> {
    let mut bytes = vec![0u8; 8192];
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
    let mut write = (0u32).to_le_bytes().to_vec();
    for w in job.write {
        write.extend_from_slice(&w.to_le_bytes());
    }
    builder.item(TAG_WRITE, &write).unwrap();
    if let Some(run) = job.l2 {
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
    bytes.truncate(len);
    bytes
}

#[test]
fn a_codec_job_with_a_second_window_runs_on_the_rkvenc_block() {
    let kernel = Arc::new(Mutex::new(Kernel::new(
        Block::ALL
            .into_iter()
            .fold(0, |bits, b| bits | 1 << b.facts().client),
    )));
    let mut block = MppBlock::new(PlayedDevice(Arc::clone(&kernel)), Block::Rkvenc);
    let stream = MockStream::with_handles(vec![
        dma_buf::sealed_memfd(c"rkvenc-codec", 1 << 20).unwrap(),
    ]);

    // The producer takes buffer 0, its source picture, which the job names: a buffer is held
    // once the producer has taken it from the consumer's free list.
    let mut producer = stream.clone();
    assert_eq!(producer.next_buffer(), Ok(Some(0)));
    let mut regs = RegisterSet::new();
    regs.set(set::reg017::SRC_CFMT, 4).unwrap();
    regs.set_l2(l2::iprd_tthdy4::IPRD_TTHDY4[1], 0x1234)
        .unwrap();
    let address: AddressReg = set::reg070::ADR_SRC0;
    assert_eq!(address.region, Region::Set);
    regs.set_stream_address(address, 0, 0).unwrap();
    let job = regs.codec_job().unwrap();
    let bytes = work_for(&job);
    let work = Work::parse(&bytes).unwrap();

    let buffers = Buffers::naming(&stream, 1);
    let mut back = vec![0u8; 4 * usize::from(READ_BACK.count)];
    match block.start(&work, &buffers, &mut back, 0) {
        Start::Done(n) => assert_eq!(n, 4 * usize::from(READ_BACK.count)),
        other => panic!("the job did not run: {other:?}"),
    }
    // The second window's word 1 reached the kernel's task, after the first window's 780 words.
    assert_eq!(kernel.lock().unwrap().tasks[0].regs[781], 0x1234_0000);
    // The stream buffer's address went in the first window's register, translated.
    assert_eq!(kernel.lock().unwrap().translated_len, 1);
}
