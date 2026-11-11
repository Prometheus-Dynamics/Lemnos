//! The packet and frame paths allocate nothing once streaming.

use core::time::Duration;
use std::sync::atomic::{AtomicUsize, Ordering};

use lemnos_contract_camera_frames::CameraFrames;
use lemnos_core::{ErrorKind, Lifecycle};
use lemnos_hal::blocks::{BufferHandle, BufferKind, ControlSetup, IsoPacket, StreamFrame};
use lemnos_hal::{StreamEndpoint, UsbDevice};
use lemnos_measure::test_alloc::{Counting, allocations};

use super::fixture::{MODE_MJPEG, webcam};
use super::rig::{self, EOF, ERR, FID};
use crate::session::STREAMING;

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Hands back the same packets for ever, without allocating.
struct Replay {
    descriptors: Vec<u8>,
    packets: Vec<Vec<u8>>,
    cursor: usize,
}

impl UsbDevice for Replay {
    type Error = ErrorKind;

    fn config_descriptor(&mut self, _: u8, out: &mut [u8]) -> Result<usize, ErrorKind> {
        let length = self.descriptors.len().min(out.len());
        out[..length].copy_from_slice(&self.descriptors[..length]);
        Ok(length)
    }
    fn reset(&mut self) -> Result<(), ErrorKind> {
        Ok(())
    }
    fn detach_kernel_driver(&mut self, _: u8) -> Result<(), ErrorKind> {
        Ok(())
    }
    fn claim_interface(&mut self, _: u8) -> Result<(), ErrorKind> {
        Ok(())
    }
    fn release_interface(&mut self, _: u8) -> Result<(), ErrorKind> {
        Ok(())
    }
    fn set_alternate(&mut self, _: u8, _: u8) -> Result<(), ErrorKind> {
        Ok(())
    }
    fn control_in(
        &mut self,
        _: ControlSetup,
        data: &mut [u8],
        _: Duration,
    ) -> Result<usize, ErrorKind> {
        data.fill(0);
        data[2] = 2;
        data[3] = 1;
        data[22..26].copy_from_slice(&900u32.to_le_bytes());
        Ok(data.len())
    }
    fn control_out(
        &mut self,
        _: ControlSetup,
        data: &[u8],
        _: Duration,
    ) -> Result<usize, ErrorKind> {
        Ok(data.len())
    }
    fn bulk_in(&mut self, _: u8, _: &mut [u8], _: Duration) -> Result<usize, ErrorKind> {
        Err(ErrorKind::Unsupported)
    }
    fn bulk_out(&mut self, _: u8, _: &[u8], _: Duration) -> Result<usize, ErrorKind> {
        Err(ErrorKind::Unsupported)
    }
    fn interrupt_in(&mut self, _: u8, _: &mut [u8], _: Duration) -> Result<usize, ErrorKind> {
        Err(ErrorKind::Unsupported)
    }
    fn iso_in(
        &mut self,
        _: u8,
        data: &mut [u8],
        packets: &mut [IsoPacket],
        _: Duration,
    ) -> Result<(), ErrorKind> {
        let capacity = packets[0].capacity as usize;
        for (index, packet) in packets.iter_mut().enumerate() {
            let source = &self.packets[self.cursor];
            self.cursor = (self.cursor + 1) % self.packets.len();
            data[index * capacity..index * capacity + source.len()].copy_from_slice(source);
            packet.received = source.len() as u32;
            packet.ok = true;
        }
        Ok(())
    }
}

/// A stream that always has a buffer, holding what was written into it. Counts what is
/// published.
struct Endless {
    published: &'static AtomicUsize,
    buffer: Vec<u8>,
}

impl StreamEndpoint for Endless {
    type Error = ErrorKind;
    fn buffer_count(&self) -> u32 {
        1
    }

    fn buffer_kind(&self) -> BufferKind {
        BufferKind::Plain
    }
    fn publish(&mut self, _: StreamFrame) -> Result<(), ErrorKind> {
        self.published.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
    fn next_buffer(&mut self) -> Result<Option<u32>, ErrorKind> {
        Ok(Some(0))
    }
    fn next_job(&mut self, _: &mut [u8]) -> Result<Option<usize>, ErrorKind> {
        Ok(None)
    }
    fn write(&mut self, _: u32, offset: usize, data: &[u8]) -> Result<bool, ErrorKind> {
        let end = offset
            .checked_add(data.len())
            .ok_or(ErrorKind::InvalidInput)?;
        let to = self
            .buffer
            .get_mut(offset..end)
            .ok_or(ErrorKind::InvalidInput)?;
        to.copy_from_slice(data);
        Ok(true)
    }
    fn read(&mut self, _: u32, _: usize, _: &mut [u8]) -> Result<bool, ErrorKind> {
        Ok(false)
    }
    fn buffer_handle(&self, _: u32) -> Result<Option<BufferHandle<'_>>, ErrorKind> {
        Err(ErrorKind::Unsupported)
    }

    fn fault_consumer(&mut self) -> Result<(), ErrorKind> {
        Ok(())
    }
}

static PUBLISHED: AtomicUsize = AtomicUsize::new(0);

#[test]
fn a_thousand_transfers_allocate_nothing() {
    let mut packets = Vec::new();
    // A joining packet, then good frames, a damaged one, a malformed header and a short packet.
    packets.push(vec![2, EOF]);
    for frame in 0u8..64 {
        let fid = frame & 1 ^ FID;
        packets.push(vec![2, fid, frame, frame]);
        packets.push(vec![2, fid, 9, 9, 9]);
        packets.push(vec![2, fid | EOF, 7]);
    }
    packets.push(vec![200, FID, 1]);
    packets.push(vec![2, FID | ERR | EOF, 1]);
    let usb = Replay {
        descriptors: webcam().bytes,
        packets,
        cursor: 0,
    };
    let stream = Endless {
        published: &PUBLISHED,
        buffer: vec![0; 4096],
    };
    let mut uvc = rig::bind(Box::new(usb), Box::new(stream), 4096).unwrap();

    let mut now = 1_000;
    for _ in 0..16 {
        Lifecycle::tick(&mut uvc, now);
        now += 1_000;
    }
    uvc.select_format(MODE_MJPEG).unwrap();
    uvc.start().unwrap();
    for _ in 0..16 {
        Lifecycle::tick(&mut uvc, now);
        now += 1_000;
    }
    assert_eq!(uvc.session.state(), STREAMING);

    // The first transfer is measured too: everything was sized before this.
    let ((), allocated) = allocations(|| {
        for _ in 0..1000 {
            Lifecycle::tick(&mut uvc, now);
            now += 1_000;
        }
    });
    assert_eq!(allocated, 0, "the packet and frame paths allocated");
    assert!(PUBLISHED.load(Ordering::Relaxed) > 1000);
    assert!(uvc.session.stats().bad_packets > 0);
}
