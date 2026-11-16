//! The DWC3 bulk link the 472 loader serves frames over. The OUT endpoint carries the host's frames to
//! the loader; the IN endpoint carries the loader's replies. A receive posts one TRB for the length the
//! caller asked for and waits for the controller to hand it back: a short count is a cut frame, and a
//! TRB that never comes back is a timeout (a bus reset mid-transfer looks the same at this level). A
//! send posts one TRB for the reply and waits for it to be taken.
//!
//! The TRBs and the buffers are the board's DMA memory, reached through [`DmaMem`]: the -sys crate
//! implements it with volatile access and cache maintenance; the models in `models` implement it for
//! the tests. The register sequences are [`crate::io`]'s, through [`RegAccess`]. Register access and
//! the clock are shared through [`Bus`], so the link, the card and the watchdog never nest a call.
//!
//! Limits: a frame longer than its receive TRB is not split (the host's frames are exact by the
//! protocol, so a longer one is a fault the caller's next receive will see as a desync); the `ack`
//! is the status byte the serve loop sends, not a packet of its own.

use core::cell::RefCell;

use crate::bind::{RegAccess, feed};
use crate::io::{BLOCK, IoFault, Trb, sd_read_block, sd_write_block, start_transfer};
use crate::sd::Card;
use crate::wdt::LONGEST_PERIOD_US;
use lemnos_flash_stage::guard::Watchdog;
use lemnos_flash_stage::link::{IoError, Link, Storage};
use lemnos_flash_stage::wait::{Clock, wait_for};

/// The TRB of the OUT endpoint (the host's frames) and of the IN endpoint (the replies).
pub const OUT_SLOT: usize = 0;
pub const IN_SLOT: usize = 1;

/// The DMA memory the link needs: two TRBs, a receive buffer the controller writes and a transmit
/// buffer the CPU writes. Addresses are the runtime addresses the controller is given.
pub trait DmaMem {
    fn rx_addr(&self) -> u64;
    fn tx_addr(&self) -> u64;
    /// The runtime address of TRB `slot`.
    fn trb_addr(&self, slot: usize) -> u64;
    /// Writes TRB `slot` (the CPU's write; [`DmaMem::clean_for_device`] makes it visible).
    fn write_trb(&mut self, slot: usize, trb: Trb);
    /// Reads TRB `slot` as the controller last left it: the CPU's copy of the TRB's line is
    /// invalidated first, so each poll sees the controller's update.
    fn read_trb(&mut self, slot: usize) -> Trb;
    /// The receive buffer (the controller writes it; read it after [`DmaMem::invalidate_rx`]).
    fn rx(&mut self) -> &mut [u8];
    /// The transmit buffer (the CPU writes it, then [`DmaMem::clean_for_device`]).
    fn tx(&mut self) -> &mut [u8];
    /// Makes the CPU's writes to the TRBs and the transmit buffer visible to the controller.
    fn clean_for_device(&mut self);
    /// Makes the controller's writes to the receive buffer visible to the CPU.
    fn invalidate_rx(&mut self);
}

/// Maps a register-sequence fault to the link's error: a bounded wait that ran out is a timeout,
/// everything else is a failure the caller ends the round on.
pub fn io_error(fault: IoFault) -> IoError {
    match fault {
        IoFault::Timeout => IoError::Timeout,
        _ => IoError::Failed,
    }
}

/// The registers and the clock, shared by everything that drives the controller. Each call of
/// [`Bus::with`] borrows both for one register sequence; calls never nest.
pub struct Bus<'a, C: Clock> {
    regs: RefCell<&'a mut dyn RegAccess>,
    clock: RefCell<&'a mut C>,
}

impl<'a, C: Clock> Bus<'a, C> {
    pub fn new(regs: &'a mut dyn RegAccess, clock: &'a mut C) -> Self {
        Self {
            regs: RefCell::new(regs),
            clock: RefCell::new(clock),
        }
    }

    /// Runs one register sequence with the registers and the clock.
    pub fn with<T>(&self, f: impl FnOnce(&mut dyn RegAccess, &mut C) -> T) -> T {
        let mut regs = self.regs.borrow_mut();
        let mut clock = self.clock.borrow_mut();
        f(&mut **regs, &mut **clock)
    }
}

/// The bulk link over the DWC3 endpoints `out_ep` (receive) and `in_ep` (send) of the controller at
/// `base`, with its DMA memory `M`.
pub struct DwcLink<'b, 'a, C: Clock, M: DmaMem> {
    bus: &'b Bus<'a, C>,
    dma: M,
    base: usize,
    out_ep: usize,
    in_ep: usize,
}

impl<'b, 'a, C: Clock, M: DmaMem> DwcLink<'b, 'a, C, M> {
    pub fn new(bus: &'b Bus<'a, C>, dma: M, base: usize, out_ep: usize, in_ep: usize) -> Self {
        Self {
            bus,
            dma,
            base,
            out_ep,
            in_ep,
        }
    }

    /// The DMA memory (the tests read what was sent through it).
    pub fn dma(&self) -> &M {
        &self.dma
    }

    pub fn into_dma(self) -> M {
        self.dma
    }

    /// Starts the transfer on `ep` with the TRB in `slot`.
    fn start(&self, ep: usize, slot: usize) -> Result<(), IoError> {
        let trb = self.dma.trb_addr(slot);
        let base = self.base;
        self.bus
            .with(|regs, clock| start_transfer(regs, clock, base, ep, trb))
            .map_err(io_error)
    }

    /// Waits until the controller hands TRB `slot` back, and returns its remaining byte count.
    fn await_trb(&mut self, slot: usize, budget_us: u64) -> Result<u32, IoError> {
        let dma = &mut self.dma;
        self.bus
            .with(|_, clock| {
                wait_for(clock, budget_us, LONGEST_PERIOD_US, || {
                    let trb = dma.read_trb(slot);
                    (!trb.owned_by_controller()).then_some(trb.size & 0x00FF_FFFF)
                })
            })
            .map_err(|_| IoError::Timeout)
    }
}

impl<C: Clock, M: DmaMem> Link for DwcLink<'_, '_, C, M> {
    fn recv(&mut self, buf: &mut [u8], budget_us: u64) -> Result<usize, IoError> {
        let want = buf.len();
        if want == 0 || want > self.dma.rx().len() || want > 0x00FF_FFFF {
            return Err(IoError::Failed);
        }
        let addr = self.dma.rx_addr();
        self.dma.write_trb(OUT_SLOT, Trb::normal(addr, want as u32));
        self.dma.clean_for_device();
        self.start(self.out_ep, OUT_SLOT)?;
        let remaining = self.await_trb(OUT_SLOT, budget_us)?;
        if remaining != 0 {
            // A cut frame: the host stopped short of the length the frame's header promised.
            return Err(IoError::Failed);
        }
        self.dma.invalidate_rx();
        buf.copy_from_slice(&self.dma.rx()[..want]);
        Ok(want)
    }

    fn ack(&mut self) -> Result<(), IoError> {
        // The status byte the serve loop sends after a write is the acknowledgement.
        Ok(())
    }

    fn send(&mut self, data: &[u8], budget_us: u64) -> Result<(), IoError> {
        if data.is_empty() || data.len() > self.dma.tx().len() || data.len() > 0x00FF_FFFF {
            return Err(IoError::Failed);
        }
        self.dma.tx()[..data.len()].copy_from_slice(data);
        let addr = self.dma.tx_addr();
        self.dma
            .write_trb(IN_SLOT, Trb::normal(addr, data.len() as u32));
        self.dma.clean_for_device();
        self.start(self.in_ep, IN_SLOT)?;
        let remaining = self.await_trb(IN_SLOT, budget_us)?;
        if remaining != 0 {
            return Err(IoError::Failed);
        }
        Ok(())
    }
}

/// The SD card as the loader's storage: sectors at `lba`, through the host's FIFO.
pub struct SdStorage<'b, 'a, C: Clock> {
    bus: &'b Bus<'a, C>,
    base: usize,
    card: Card,
}

impl<'b, 'a, C: Clock> SdStorage<'b, 'a, C> {
    pub fn new(bus: &'b Bus<'a, C>, base: usize, card: Card) -> Self {
        Self { bus, base, card }
    }

    /// The command argument for sector `lba`: the block address on a high-capacity card, the byte
    /// address otherwise.
    fn argument(&self, lba: u32) -> Result<u32, IoError> {
        if self.card.high_capacity {
            Ok(lba)
        } else {
            lba.checked_mul(BLOCK as u32).ok_or(IoError::Failed)
        }
    }
}

impl<C: Clock> Storage for SdStorage<'_, '_, C> {
    fn write(&mut self, lba: u32, data: &[u8], _budget_us: u64) -> Result<(), IoError> {
        let (sectors, rest) = data.as_chunks::<BLOCK>();
        if !rest.is_empty() {
            return Err(IoError::Failed);
        }
        for (index, sector) in sectors.iter().enumerate() {
            let sector_lba = lba.checked_add(index as u32).ok_or(IoError::Failed)?;
            let arg = self.argument(sector_lba)?;
            let block: &[u8; BLOCK] = sector;
            let base = self.base;
            self.bus
                .with(|regs, clock| sd_write_block(regs, clock, base, arg, block))
                .map_err(io_error)?;
        }
        Ok(())
    }

    fn read(&mut self, lba: u32, out: &mut [u8], _budget_us: u64) -> Result<(), IoError> {
        let (sectors, rest) = out.as_chunks_mut::<BLOCK>();
        if !rest.is_empty() {
            return Err(IoError::Failed);
        }
        for (index, sector) in sectors.iter_mut().enumerate() {
            let sector_lba = lba.checked_add(index as u32).ok_or(IoError::Failed)?;
            let arg = self.argument(sector_lba)?;
            let base = self.base;
            let block = self
                .bus
                .with(|regs, clock| sd_read_block(regs, clock, base, arg))
                .map_err(io_error)?;
            *sector = block;
        }
        Ok(())
    }
}

/// The watchdog as the serve loop drives it: a kick through the shared registers.
pub struct WdtFeed<'b, 'a, C: Clock> {
    bus: &'b Bus<'a, C>,
}

impl<'b, 'a, C: Clock> WdtFeed<'b, 'a, C> {
    pub fn new(bus: &'b Bus<'a, C>) -> Self {
        Self { bus }
    }
}

impl<C: Clock> Watchdog for WdtFeed<'_, '_, C> {
    fn feed(&mut self) {
        self.bus.with(|regs, _clock| feed(regs));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bind::DWC3;
    use crate::models::{DwcEndpoints, SimDma};
    use lemnos_flash_stage::serve::{ServeError, serve};
    use lemnos_flash_stage::wait::IterClock;
    use lemnos_flash_wire::frame::ResetKind;
    use lemnos_flash_wire::frame::crc16_ccitt;
    use lemnos_flash_wire::frame::{MAX_SECTORS, Op, Request, SECTOR, STATUS_CRC};

    const OUT: usize = 2;
    const IN: usize = 3;

    /// Sixteen sectors of a card.
    struct CardStore(Vec<u8>);

    impl Storage for CardStore {
        fn write(&mut self, lba: u32, data: &[u8], _budget_us: u64) -> Result<(), IoError> {
            let at = lba as usize * SECTOR;
            self.0
                .get_mut(at..at + data.len())
                .ok_or(IoError::Failed)?
                .copy_from_slice(data);
            Ok(())
        }
        fn read(&mut self, lba: u32, out: &mut [u8], _budget_us: u64) -> Result<(), IoError> {
            let at = lba as usize * SECTOR;
            out.copy_from_slice(self.0.get(at..at + out.len()).ok_or(IoError::Failed)?);
            Ok(())
        }
    }

    #[derive(Default)]
    struct Feeds(u32);

    impl Watchdog for Feeds {
        fn feed(&mut self) {
            self.0 += 1;
        }
    }

    fn request(op: Op, reserved: u8, count: u16, lba: u32) -> Vec<u8> {
        Request {
            op,
            reserved,
            count,
            lba,
        }
        .encode()
        .to_vec()
    }

    fn with_crc(data: &[u8]) -> Vec<u8> {
        let mut out = data.to_vec();
        out.extend(crc16_ccitt(data).to_be_bytes());
        out
    }

    fn pattern(seed: u8) -> Vec<u8> {
        (0..SECTOR).map(|i| (i as u8) ^ seed).collect()
    }

    /// Serves `frames` through the link over a fresh endpoint model; returns the result, the card and
    /// the feeds, and what the device sent.
    fn serve_frames(
        frames: Vec<Vec<u8>>,
        stall_after: Option<usize>,
        card: &mut CardStore,
        feeds: &mut Feeds,
    ) -> (Result<ResetKind, ServeError>, Vec<Vec<u8>>) {
        let mut regs = DwcEndpoints::new(DWC3);
        let mut clock = IterClock::new();
        let bus = Bus::new(&mut regs, &mut clock);
        let mut sim = SimDma::new(frames);
        sim.stall_after = stall_after;
        let mut link = DwcLink::new(&bus, sim, DWC3, OUT, IN);
        let mut buf = vec![0u8; usize::from(MAX_SECTORS) * SECTOR + 2];
        let mut scratch = vec![0u8; usize::from(MAX_SECTORS) * SECTOR];
        let result = serve(
            &mut link,
            card,
            feeds,
            &crate::entry472::REGIONS,
            &mut buf,
            &mut scratch,
        );
        (result, link.into_dma().outbound)
    }

    fn blank() -> CardStore {
        CardStore(vec![0; 16 * SECTOR])
    }

    #[test]
    fn a_read_round_sends_the_sector_with_its_crc_then_the_end_status() {
        let mut card = CardStore(pattern(7).into_iter().chain(vec![0; 15 * SECTOR]).collect());
        let mut feeds = Feeds::default();
        let frames = vec![request(Op::Read, 0, 1, 0), request(Op::End, 0, 1, 0)];
        let (result, sent) = serve_frames(frames, None, &mut card, &mut feeds);
        assert_eq!(result, Ok(ResetKind::Normal));
        assert_eq!(sent, vec![with_crc(&pattern(7)), vec![0]]);
        assert_eq!(feeds.0, 1, "one feed for the verified reply");
    }

    #[test]
    fn a_write_round_stores_the_verified_sector_and_acknowledges_it() {
        let mut card = blank();
        let mut feeds = Feeds::default();
        let frames = vec![
            request(Op::Write, 0, 1, 0),
            with_crc(&pattern(9)),
            request(Op::End, 0, 1, 0),
        ];
        let (result, sent) = serve_frames(frames, None, &mut card, &mut feeds);
        assert_eq!(result, Ok(ResetKind::Normal));
        assert_eq!(&card.0[..SECTOR], &pattern(9)[..]);
        assert_eq!(
            sent,
            vec![vec![0], vec![0]],
            "write status, then END status"
        );
        assert_eq!(feeds.0, 1);
    }

    #[test]
    fn a_bad_crc_is_refused_with_its_status_and_never_reaches_the_card() {
        let mut card = blank();
        let mut feeds = Feeds::default();
        let mut bad = with_crc(&pattern(9));
        bad[100] ^= 0x01;
        let frames = vec![request(Op::Write, 0, 1, 0), bad, request(Op::End, 0, 1, 0)];
        let (result, sent) = serve_frames(frames, None, &mut card, &mut feeds);
        assert_eq!(result, Ok(ResetKind::Normal));
        assert_eq!(sent, vec![vec![STATUS_CRC], vec![0]]);
        assert!(card.0.iter().all(|&b| b == 0), "the card is untouched");
        assert_eq!(feeds.0, 0, "a refused chunk never feeds");
    }

    #[test]
    fn a_cut_payload_ends_the_loop_with_a_link_failure_and_no_feed() {
        let mut card = blank();
        let mut feeds = Feeds::default();
        let whole = with_crc(&pattern(9));
        let frames = vec![request(Op::Write, 0, 1, 0), whole[..300].to_vec()];
        let (result, sent) = serve_frames(frames, None, &mut card, &mut feeds);
        assert_eq!(result, Err(ServeError::Link(IoError::Failed)));
        assert!(sent.is_empty());
        assert!(card.0.iter().all(|&b| b == 0));
        assert_eq!(feeds.0, 0);
    }

    #[test]
    fn the_end_frame_with_the_maskrom_reset_kind_returns_maskrom() {
        let mut card = blank();
        let mut feeds = Feeds::default();
        let frames = vec![request(Op::End, 3, 1, 0)];
        let (result, sent) = serve_frames(frames, None, &mut card, &mut feeds);
        assert_eq!(result, Ok(ResetKind::Maskrom));
        assert_eq!(sent, vec![vec![0]]);
    }

    #[test]
    fn a_bus_reset_mid_transfer_stalls_the_payload_and_ends_without_a_feed() {
        let mut card = blank();
        let mut feeds = Feeds::default();
        let frames = vec![request(Op::Write, 0, 1, 0), with_crc(&pattern(9))];
        let (result, sent) = serve_frames(frames, Some(1), &mut card, &mut feeds);
        assert_eq!(result, Err(ServeError::Link(IoError::Timeout)));
        assert!(sent.is_empty());
        assert!(card.0.iter().all(|&b| b == 0));
        assert_eq!(feeds.0, 0);
    }

    #[test]
    fn a_reply_the_host_never_takes_times_out_as_a_link_timeout() {
        let mut regs = DwcEndpoints::new(DWC3);
        let mut clock = IterClock::new();
        let bus = Bus::new(&mut regs, &mut clock);
        let mut sim = SimDma::new(vec![]);
        sim.refuse_send = true;
        let mut link = DwcLink::new(&bus, sim, DWC3, OUT, IN);
        assert_eq!(link.send(&[1, 2, 3], 1_000), Err(IoError::Timeout));
    }

    #[test]
    fn a_reply_longer_than_the_transmit_buffer_is_refused_before_any_transfer() {
        let mut regs = DwcEndpoints::new(DWC3);
        let mut clock = IterClock::new();
        let bus = Bus::new(&mut regs, &mut clock);
        let mut link = DwcLink::new(&bus, SimDma::new(vec![]), DWC3, OUT, IN);
        let too_long = vec![0u8; usize::from(MAX_SECTORS) * SECTOR + 3];
        assert_eq!(link.send(&too_long, 1_000), Err(IoError::Failed));
        assert!(link.dma().outbound.is_empty());
    }
}
