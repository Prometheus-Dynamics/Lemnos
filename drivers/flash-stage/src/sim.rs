//! A simulated device for the stage: a card in memory, a link that carries the frames a host queues, and
//! the real serve loop running over them. A PC side runs its host against it to check its own side of
//! the conformance list (see `README.md`). This crate's tests run the list's scenarios.
//!
//! The device is a model. It answers each frame as the stage does and holds no timing: a read or a write
//! is done when the call returns, and a link with no frame queued is a timeout.

use std::collections::VecDeque;
use std::vec::Vec;

use crate::guard::Watchdog;
use crate::link::{IoError, Link, Storage};
use crate::region::Region;
use crate::serve::{ServeError, serve};
use lemnos_flash_wire::frame::{MAX_CHUNK, ResetKind, SECTOR};

/// How a run of the device ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// An END was served, and the reset kind it asked for.
    Ended(ResetKind),
    /// No frame was queued: the host has not sent the next one yet.
    Idle,
    /// The loop ended before an END.
    Stopped(ServeError),
}

/// The frames in both directions.
#[derive(Default)]
pub struct Queues {
    inbound: VecDeque<Vec<u8>>,
    outbound: Vec<Vec<u8>>,
}

/// The card: sectors in memory.
pub struct Card {
    bytes: Vec<u8>,
}

/// The watchdog's counter.
#[derive(Default)]
pub struct Counter {
    feeds: u32,
}

/// A simulated stage.
pub struct Device {
    link: Queues,
    card: Card,
    dog: Counter,
    regions: Vec<Region>,
}

impl Device {
    /// A device with `sectors` zeroed sectors and the chip's `regions`.
    pub fn new(sectors: u32, regions: &[Region]) -> Self {
        Self {
            link: Queues::default(),
            card: Card {
                bytes: vec![0; sectors as usize * SECTOR],
            },
            dog: Counter::default(),
            regions: regions.to_vec(),
        }
    }

    /// Queues a frame the host sent, as one transfer.
    pub fn send(&mut self, frame: &[u8]) {
        self.link.inbound.push_back(frame.to_vec());
    }

    /// Serves the queued frames until an END, a stop, or the queue runs dry.
    pub fn run(&mut self) -> Outcome {
        if self.link.inbound.is_empty() {
            return Outcome::Idle;
        }
        let mut buf = vec![0u8; MAX_CHUNK];
        let mut scratch = vec![0u8; MAX_CHUNK];
        match serve(
            &mut self.link,
            &mut self.card,
            &mut self.dog,
            &self.regions,
            &mut buf,
            &mut scratch,
        ) {
            Ok(kind) => Outcome::Ended(kind),
            Err(e) => Outcome::Stopped(e),
        }
    }

    /// The frames the device sent since the last call, in order.
    pub fn replies(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.link.outbound)
    }

    /// The card's bytes.
    pub fn card(&self) -> &[u8] {
        &self.card.bytes
    }

    /// How many times the watchdog was fed.
    pub fn feeds(&self) -> u32 {
        self.dog.feeds
    }
}

impl Link for Queues {
    fn recv(&mut self, buf: &mut [u8], _budget_us: u64) -> Result<usize, IoError> {
        let frame = self.inbound.pop_front().ok_or(IoError::Timeout)?;
        if frame.len() > buf.len() {
            return Err(IoError::Failed);
        }
        buf[..frame.len()].copy_from_slice(&frame);
        Ok(frame.len())
    }

    fn ack(&mut self) -> Result<(), IoError> {
        Ok(())
    }

    fn send(&mut self, data: &[u8], _budget_us: u64) -> Result<(), IoError> {
        self.outbound.push(data.to_vec());
        Ok(())
    }
}

impl Storage for Card {
    fn write(&mut self, lba: u32, data: &[u8], _budget_us: u64) -> Result<(), IoError> {
        let at = lba as usize * SECTOR;
        let end = at.checked_add(data.len()).ok_or(IoError::Failed)?;
        self.bytes
            .get_mut(at..end)
            .ok_or(IoError::Failed)?
            .copy_from_slice(data);
        Ok(())
    }

    fn read(&mut self, lba: u32, out: &mut [u8], _budget_us: u64) -> Result<(), IoError> {
        let at = lba as usize * SECTOR;
        let end = at.checked_add(out.len()).ok_or(IoError::Failed)?;
        out.copy_from_slice(self.bytes.get(at..end).ok_or(IoError::Failed)?);
        Ok(())
    }
}

impl Watchdog for Counter {
    fn feed(&mut self) {
        self.feeds += 1;
    }
}

/// The conformance scenarios of the shared stage. Each is a frame exchange with the simulated device;
/// a PC side's host passes the same scenario when its frames and its reading of the replies agree.
#[cfg(test)]
mod scenarios {
    use super::*;
    use crate::region::Class;
    use lemnos_flash_wire::frame::{
        FrameError, Op, RESET_MASKROM, Request, ResetKind, STATUS_CRC, STATUS_NOT_COMPARED,
        STATUS_OK, payload_crc,
    };
    use lemnos_flash_wire::identity::{IDENTITY, check};

    // A small boot area, so the plain sectors used below (500, and the card's tail) lie outside it.
    const REGIONS: [Region; 2] = [
        Region {
            first: 64,
            end: 256,
            class: Class::BootArea,
        },
        Region {
            first: 64,
            end: 65,
            class: Class::Header,
        },
    ];

    fn device() -> Device {
        Device::new(1024, &REGIONS)
    }

    fn chunk(payload: &[u8], crc_flip: bool) -> Vec<u8> {
        let mut out = payload.to_vec();
        let mut crc = payload_crc(payload);
        if crc_flip {
            crc[1] ^= 1;
        }
        out.extend_from_slice(&crc);
        out
    }

    fn sector(fill: u8) -> Vec<u8> {
        vec![fill; SECTOR]
    }

    fn card_sector(dev: &Device, lba: u32) -> &[u8] {
        &dev.card()[lba as usize * SECTOR..(lba as usize + 1) * SECTOR]
    }

    // S01
    #[test]
    fn identify_answers_this_builds_identity_and_feeds_nothing() {
        let mut dev = device();
        dev.send(&Request::identify().encode());
        dev.send(&Request::end(ResetKind::Normal).encode());
        assert_eq!(dev.run(), Outcome::Ended(ResetKind::Normal));
        let replies = dev.replies();
        assert_eq!(replies[0], IDENTITY.to_vec());
        assert_eq!(check(&replies[0]), Ok(()));
        assert_eq!(dev.feeds(), 0);
    }

    // S02
    #[test]
    fn read_returns_the_sectors_with_their_crc_and_feeds_once() {
        let mut dev = device();
        dev.card.bytes[SECTOR..3 * SECTOR].fill(0x5A);
        dev.send(&Request::read(1, 2).encode());
        dev.send(&Request::end(ResetKind::Normal).encode());
        assert_eq!(dev.run(), Outcome::Ended(ResetKind::Normal));
        let replies = dev.replies();
        assert_eq!(replies[0].len(), 2 * SECTOR + 2);
        assert_eq!(&replies[0][..2 * SECTOR], &vec![0x5A; 2 * SECTOR][..]);
        assert_eq!(
            &replies[0][2 * SECTOR..],
            &payload_crc(&replies[0][..2 * SECTOR])[..]
        );
        assert_eq!(dev.feeds(), 1);
        assert_eq!(replies[1], vec![STATUS_OK]);
    }

    // S03
    #[test]
    fn a_plain_write_stores_the_verified_chunk_and_feeds_once() {
        let mut dev = device();
        dev.send(&Request::write(500, 1, false).encode());
        dev.send(&chunk(&sector(0x3C), false));
        dev.send(&Request::end(ResetKind::Normal).encode());
        assert_eq!(dev.run(), Outcome::Ended(ResetKind::Normal));
        assert_eq!(card_sector(&dev, 500), &sector(0x3C)[..]);
        assert_eq!(dev.replies()[0], vec![STATUS_OK]);
        assert_eq!(dev.feeds(), 1);
    }

    // S04
    #[test]
    fn a_write_with_a_bad_crc_is_refused_with_status_one_and_neither_stored_nor_fed() {
        let mut dev = device();
        dev.send(&Request::write(500, 1, false).encode());
        dev.send(&chunk(&sector(0x3C), true));
        dev.send(&Request::end(ResetKind::Normal).encode());
        assert_eq!(
            dev.run(),
            Outcome::Ended(ResetKind::Normal),
            "the loop goes on"
        );
        assert_eq!(dev.replies()[0], vec![STATUS_CRC]);
        assert_eq!(card_sector(&dev, 500), &sector(0)[..]);
        assert_eq!(dev.feeds(), 0);
    }

    // S05
    #[test]
    fn a_cut_before_the_payload_stops_the_session_with_nothing_stored_and_no_feed() {
        let mut dev = device();
        dev.send(&Request::write(500, 1, false).encode());
        assert_eq!(
            dev.run(),
            Outcome::Stopped(ServeError::Link(IoError::Timeout))
        );
        assert_eq!(card_sector(&dev, 500), &sector(0)[..]);
        assert_eq!(dev.feeds(), 0);
    }

    // S06
    #[test]
    fn a_boot_area_write_without_the_mark_is_refused_with_status_four() {
        let mut dev = device();
        dev.send(&Request::write(100, 1, false).encode());
        dev.send(&chunk(&sector(0x77), false));
        dev.send(&Request::end(ResetKind::Normal).encode());
        assert_eq!(dev.run(), Outcome::Ended(ResetKind::Normal));
        assert_eq!(dev.replies()[0], vec![STATUS_NOT_COMPARED]);
        assert_eq!(
            card_sector(&dev, 100),
            &sector(0)[..],
            "nothing was written"
        );
        assert_eq!(dev.feeds(), 0);
    }

    // S07
    #[test]
    fn a_boot_area_write_with_the_mark_is_stored() {
        let mut dev = device();
        dev.send(&Request::write(100, 1, true).encode());
        dev.send(&chunk(&sector(0x77), false));
        dev.send(&Request::end(ResetKind::Normal).encode());
        assert_eq!(dev.run(), Outcome::Ended(ResetKind::Normal));
        assert_eq!(dev.replies()[0], vec![STATUS_OK]);
        assert_eq!(card_sector(&dev, 100), &sector(0x77)[..]);
        assert_eq!(dev.feeds(), 1);
    }

    // S08
    #[test]
    fn a_header_write_with_a_bad_crc_is_refused_before_any_write_even_with_the_mark() {
        let mut dev = device();
        dev.send(&Request::write(64, 1, true).encode());
        dev.send(&chunk(&sector(0xEE), true));
        dev.send(&Request::end(ResetKind::Normal).encode());
        assert_eq!(dev.run(), Outcome::Ended(ResetKind::Normal));
        assert_eq!(
            dev.replies()[0],
            vec![STATUS_CRC],
            "the payload is verified first"
        );
        assert_eq!(
            card_sector(&dev, 64),
            &sector(0)[..],
            "the boot record was not touched"
        );
        assert_eq!(dev.feeds(), 0);
    }

    // S09
    #[test]
    fn a_header_write_without_the_mark_is_refused_with_status_four() {
        let mut dev = device();
        dev.send(&Request::write(64, 1, false).encode());
        dev.send(&chunk(&sector(0xEE), false));
        dev.send(&Request::end(ResetKind::Normal).encode());
        assert_eq!(dev.run(), Outcome::Ended(ResetKind::Normal));
        assert_eq!(dev.replies()[0], vec![STATUS_NOT_COMPARED]);
        assert_eq!(card_sector(&dev, 64), &sector(0)[..]);
    }

    // S10
    #[test]
    fn a_header_write_with_the_mark_and_a_good_crc_is_stored() {
        let mut dev = device();
        dev.send(&Request::write(64, 1, true).encode());
        dev.send(&chunk(&sector(0xEE), false));
        dev.send(&Request::end(ResetKind::Normal).encode());
        assert_eq!(dev.run(), Outcome::Ended(ResetKind::Normal));
        assert_eq!(dev.replies()[0], vec![STATUS_OK]);
        assert_eq!(card_sector(&dev, 64), &sector(0xEE)[..]);
    }

    // S11
    #[test]
    fn a_write_that_straddles_into_the_boot_area_needs_the_mark() {
        let mut dev = device();
        let payload = vec![0x11; 2 * SECTOR];
        dev.send(&Request::write(63, 2, false).encode());
        dev.send(&chunk(&payload, false));
        dev.send(&Request::end(ResetKind::Normal).encode());
        assert_eq!(dev.run(), Outcome::Ended(ResetKind::Normal));
        assert_eq!(dev.replies()[0], vec![STATUS_NOT_COMPARED]);
        assert_eq!(card_sector(&dev, 63), &sector(0)[..]);
    }

    // S12
    #[test]
    fn a_chunk_that_is_not_the_length_its_request_makes_it_stops_the_session_with_nothing_stored() {
        let mut dev = device();
        dev.send(&Request::write(500, 2, false).encode());
        dev.send(&chunk(&sector(0x3C), false));
        assert_eq!(
            dev.run(),
            Outcome::Stopped(ServeError::Link(IoError::Failed))
        );
        assert_eq!(card_sector(&dev, 500), &sector(0)[..]);
        assert_eq!(dev.feeds(), 0);
    }

    // S13
    #[test]
    fn end_with_the_maskrom_reset_returns_maskrom_after_an_ok_status() {
        let mut dev = device();
        dev.send(&Request::end(ResetKind::Maskrom).encode());
        assert_eq!(dev.run(), Outcome::Ended(ResetKind::Maskrom));
        assert_eq!(dev.replies(), vec![vec![STATUS_OK]]);
        assert_eq!(RESET_MASKROM, 3);
    }

    // S14
    #[test]
    fn a_request_with_a_bad_crc_ends_the_session_with_a_frame_error() {
        let mut dev = device();
        let mut wire = Request::read(1, 1).encode();
        wire[7] ^= 0x40;
        dev.send(&wire);
        assert_eq!(
            dev.run(),
            Outcome::Stopped(ServeError::Frame(FrameError::Crc))
        );
    }

    // S15
    #[test]
    fn a_reserved_bit_a_read_does_not_define_ends_the_session() {
        let mut dev = device();
        let mut request = Request::read(1, 1);
        request.reserved = 1;
        dev.send(&request.encode());
        assert_eq!(
            dev.run(),
            Outcome::Stopped(ServeError::Frame(FrameError::Reserved {
                op: Op::Read,
                reserved: 1
            }))
        );
    }

    // S16
    #[test]
    fn an_empty_queue_is_idle_not_a_stop() {
        let mut dev = device();
        assert_eq!(dev.run(), Outcome::Idle);
    }
}
