//! The SD card as the shared `Storage`. The shared rules ask for a range and a budget; this adapter
//! makes one block call per 512-byte sector and checks the budget before each one, so a card that
//! runs slow ends the call as a storage failure, not as a hang. A block call is bounded by the SD
//! host's own spin limits ([`crate::sd`]), so one sector can run past the budget by one such wait.

use crate::sd::{BLOCK, Card, SdHost};
use lemnos_flash_stage::link::{IoError, Storage};
use lemnos_flash_stage::wait::Clock;

/// One block at a time. The SD card's `read` and `write` are the only calls this needs, so a test
/// card can stand in for it.
pub trait Blocks {
    fn read_block(&mut self, lba: u64, out: &mut [u8; BLOCK]) -> Result<(), IoError>;
    fn write_block(&mut self, lba: u64, data: &[u8; BLOCK]) -> Result<(), IoError>;
}

impl<H: SdHost> Blocks for Card<H> {
    fn read_block(&mut self, lba: u64, out: &mut [u8; BLOCK]) -> Result<(), IoError> {
        self.read(lba, out).map_err(|_| IoError::Failed)
    }

    fn write_block(&mut self, lba: u64, data: &[u8; BLOCK]) -> Result<(), IoError> {
        self.write(lba, data).map_err(|_| IoError::Failed)
    }
}

/// The card, with the clock its budgets are read on.
pub struct Sectors<B: Blocks, K: Clock> {
    card: B,
    clock: K,
}

impl<B: Blocks, K: Clock> Sectors<B, K> {
    pub fn new(card: B, clock: K) -> Self {
        Self { card, clock }
    }

    fn deadline(&mut self, budget_us: u64) -> u64 {
        self.clock.now_us().saturating_add(budget_us)
    }
}

impl<B: Blocks, K: Clock> Storage for Sectors<B, K> {
    fn write(&mut self, lba: u32, data: &[u8], budget_us: u64) -> Result<(), IoError> {
        let (sectors, rest) = data.as_chunks::<BLOCK>();
        if !rest.is_empty() {
            return Err(IoError::Failed);
        }
        let deadline = self.deadline(budget_us);
        for (i, sector) in sectors.iter().enumerate() {
            if self.clock.now_us() >= deadline {
                return Err(IoError::Timeout);
            }
            self.card.write_block(u64::from(lba) + i as u64, sector)?;
        }
        Ok(())
    }

    fn read(&mut self, lba: u32, out: &mut [u8], budget_us: u64) -> Result<(), IoError> {
        let (sectors, rest) = out.as_chunks_mut::<BLOCK>();
        if !rest.is_empty() {
            return Err(IoError::Failed);
        }
        let deadline = self.deadline(budget_us);
        for (i, sector) in sectors.iter_mut().enumerate() {
            if self.clock.now_us() >= deadline {
                return Err(IoError::Timeout);
            }
            self.card.read_block(u64::from(lba) + i as u64, sector)?;
        }
        Ok(())
    }
}
