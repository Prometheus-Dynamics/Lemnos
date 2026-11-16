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
use crate::io::{BLOCK, IoFault, Trb, sd_read_block, sd_write_block};
use crate::sd::Card;
use lemnos_flash_stage::guard::Watchdog;
use lemnos_flash_stage::link::{IoError, Storage};
use lemnos_flash_stage::wait::Clock;

/// The TRB of the OUT endpoint (the host's frames) and of the IN endpoint (the replies).
pub const OUT_SLOT: usize = 0;
pub const IN_SLOT: usize = 1;

/// The DMA memory of the bulk pair and the transfer resources: TRBs, a receive buffer the controller writes and
/// a transmit buffer the CPU writes. Addresses are the runtime addresses the controller is given. The control
/// endpoint's memory is [`crate::usb::EventMem`].
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
