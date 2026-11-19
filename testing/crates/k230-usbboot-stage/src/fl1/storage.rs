//! The SD card as the shared `Storage`. The shared rules ask for a range and a budget; this adapter
//! makes one block call per 512-byte sector and checks the budget before each one, so a card that
//! runs slow ends the call as a storage failure, not as a hang. A block call is bounded by the SD
//! host's own spin limits ([`crate::sd`]), so one sector can run past the budget by one such wait.

use core::cell::Cell;

use crate::bounds::{Bounds, Declared};
use crate::sd::{BLOCK, Card, SdHost};
use lemnos_flash_stage::link::{IoError, Storage};
use lemnos_flash_stage::wait::Clock;

/// One block at a time. The SD card's `read` and `write` are the only calls this needs, so a test
/// card can stand in for it.
pub trait Blocks {
    /// The card's size in blocks.
    fn blocks(&self) -> u64;
    fn read_block(&mut self, lba: u64, out: &mut [u8; BLOCK]) -> Result<(), IoError>;
    fn write_block(&mut self, lba: u64, data: &[u8; BLOCK]) -> Result<(), IoError>;
}

impl<H: SdHost> Blocks for Card<H> {
    fn blocks(&self) -> u64 {
        Card::blocks(self)
    }

    fn read_block(&mut self, lba: u64, out: &mut [u8; BLOCK]) -> Result<(), IoError> {
        self.read(lba, out).map_err(|_| IoError::Failed)
    }

    fn write_block(&mut self, lba: u64, data: &[u8; BLOCK]) -> Result<(), IoError> {
        self.write(lba, data).map_err(|_| IoError::Failed)
    }
}

/// How many times [`Retried`] tries one block call.
pub const TRIES: u32 = 3;

/// The card with each block call tried up to [`TRIES`] times, the host and the card brought back
/// ([`Card::recover`]) between tries, and every failed try counted. Bench, 2026-11-18 (the piece path's
/// flash): at 25 MHz on one data line a write's data phase failed its CRC about once in 14,000 sectors and the
/// card dropped the block; it took the same sector again after a stop (CMD12). The piece path tries the same
/// way. A retry writes the same sector again, so it never widens what a request may reach: the bounds are
/// checked before the first block call ([`Sectors`]), and the session compares what it reads back.
pub struct Retried<'a, H: SdHost> {
    card: Card<H>,
    retries: &'a Cell<u32>,
}

impl<'a, H: SdHost> Retried<'a, H> {
    /// The card, with its failed tries counted in `retries`.
    pub fn new(card: Card<H>, retries: &'a Cell<u32>) -> Self {
        Self { card, retries }
    }

    /// The card under the retries (for a test's look at it).
    pub fn into_inner(self) -> Card<H> {
        self.card
    }

    fn tried(
        &mut self,
        mut op: impl FnMut(&mut Card<H>) -> Result<(), crate::sd::SdError>,
    ) -> Result<(), IoError> {
        for _ in 0..TRIES {
            if op(&mut self.card).is_ok() {
                return Ok(());
            }
            self.retries.set(self.retries.get().saturating_add(1));
            if self.card.recover().is_err() {
                return Err(IoError::Failed);
            }
        }
        Err(IoError::Failed)
    }
}

impl<H: SdHost> Blocks for Retried<'_, H> {
    fn blocks(&self) -> u64 {
        self.card.blocks()
    }

    fn read_block(&mut self, lba: u64, out: &mut [u8; BLOCK]) -> Result<(), IoError> {
        self.tried(|card| card.read(lba, out))
    }

    fn write_block(&mut self, lba: u64, data: &[u8; BLOCK]) -> Result<(), IoError> {
        self.tried(|card| card.write(lba, data))
    }
}

/// The card, with the clock its budgets are read on.
pub struct Sectors<B: Blocks, K: Clock> {
    card: B,
    clock: K,
    /// What this storage may write: the declared range, less the protected sectors.
    bounds: Bounds,
}

impl<B: Blocks, K: Clock> Sectors<B, K> {
    /// The card as the session's storage, writing only inside `declared` and never a protected sector
    /// ([`crate::bounds`]). The card's primary GPT header is read here; if it cannot be read, nothing is ever
    /// written. Reads are not limited.
    pub fn new(mut card: B, clock: K, declared: Declared) -> Self {
        let mut header = [0u8; BLOCK];
        let blocks = card.blocks();
        let bounds = match card.read_block(1, &mut header) {
            Ok(()) => Bounds::new(declared, &header, blocks),
            Err(_) => Bounds::closed(declared, blocks),
        };
        Self {
            card,
            clock,
            bounds,
        }
    }

    /// The card under the storage (for a test's look at it).
    pub fn into_inner(self) -> B {
        self.card
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
        // The whole request, before its first sector: a range that touches anything outside the bounds is not
        // written at all, whatever the request's compared mark said.
        if self
            .bounds
            .check(u64::from(lba), sectors.len() as u64)
            .is_err()
        {
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
