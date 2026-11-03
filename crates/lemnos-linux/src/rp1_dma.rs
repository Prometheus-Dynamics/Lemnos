//! The RP1 DMA engine: the DesignWare AXI DMAC in RP1 as a [`TransferEngine`]
//! (feature `rp1-dma`). It copies from one dma-buf to another, one block per
//! transfer, on channels the RP1 PIO does not use.
//!
//! Root only: the registers are reached through `/dev/mem`. The kernel's
//! `dw-axi-dmac` driver owns the controller, so the engine takes only the free
//! channels (3 to 7), and refuses to start a transfer when the one it would use
//! is not idle, or when the controller is not running.
//!
//! Bus addresses: a DMA engine addresses memory in the RP1's view, not the
//! CPU's. The caller supplies that translation through [`BusAddress`]. Nothing
//! here derives it: a wrong address moves data to the wrong place.
//!
//! Untested on board for transfers. The version and channel-state reads are
//! verified (see `docs/dma.md`).

use std::io;
use std::marker::PhantomData;
use std::os::fd::BorrowedFd;
use std::time::{Duration, Instant};

use lemnos_bus::{
    Completion, Sink, Source, Transfer, TransferEngine, TransferError, TransferResult,
};
use lemnos_hal::ErrorKind;
use lemnos_linux_sys::dw_axi_dmac::{
    CFG, CFG_DMAC_EN, CH_BLOCK_TS, CH_CFG_H, CH_CFG_L, CH_CTL_H, CH_CTL_L, CH_DAR, CH_INTCLEAR,
    CH_INTSTATUS, CH_LLP, CH_SAR, CHEN, COMPVER, IRQ_ALL_ERR, IRQ_DMA_TRF, Registers,
    channel_offset,
};

/// The first channel the engine may use. Channels 0 to 2 belong to the RP1 PIO
/// driver in the kernel and are never touched here.
pub const FIRST_FREE_CHANNEL: u8 = 3;
/// The channels the controller has (`dma2`: 8).
pub const CHANNELS: u8 = 8;
/// The largest block the engine programs, in transfers (`CH_BLOCK_TS` is 22 bits).
const MAX_BLOCK_TRANSFERS: u64 = 1 << 22;

/// Translates a dma-buf into the address the DMAC uses for it (the RP1's view
/// of host memory). The caller provides it. Any closure of the same shape
/// implements it.
pub trait BusAddress {
    /// The bus address of the start of `buf`.
    fn bus_address(&self, buf: BorrowedFd<'_>) -> io::Result<u64>;
}

impl<F> BusAddress for F
where
    F: Fn(BorrowedFd<'_>) -> io::Result<u64>,
{
    fn bus_address(&self, buf: BorrowedFd<'_>) -> io::Result<u64> {
        self(buf)
    }
}

/// What the controller reports, read without changing anything.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Report {
    /// `DMAC_COMPVER`.
    pub version: u32,
    /// Whether `DMAC_CFG.DMAC_EN` is set (the controller runs).
    pub controller_enabled: bool,
    /// The channels that are enabled (`DMAC_CHEN`, bit `n` for channel `n`).
    pub enabled: u8,
}

/// Reads the controller's version and channel state. Read only.
pub fn report<R: Registers>(regs: &R) -> Report {
    let cfg = regs.read32(CFG);
    let chen = regs.read32(CHEN);
    Report {
        version: regs.read32(COMPVER),
        controller_enabled: cfg & CFG_DMAC_EN != 0,
        enabled: (chen & 0xff) as u8,
    }
}

/// One block copy in flight. It borrows both buffers for `'a`; drop it only
/// after it completed, or cancel it.
#[derive(Debug)]
pub struct Ticket<'a> {
    channel: u8,
    len: usize,
    _buffers: PhantomData<&'a ()>,
}

/// The RP1 DMAC as a transfer engine over the channels `channels`.
pub struct Rp1DmaEngine<R, A> {
    regs: R,
    channels: Vec<u8>,
    addresses: A,
}

impl<R: Registers, A: BusAddress> Rp1DmaEngine<R, A> {
    /// An engine over `channels` (each in `FIRST_FREE_CHANNEL..CHANNELS`, no
    /// duplicates). Fails with `InvalidInput` otherwise, so the PIO's channels
    /// cannot be named.
    pub fn new(regs: R, channels: &[u8], addresses: A) -> io::Result<Self> {
        let mut seen = 0u8;
        for &ch in channels {
            if !(FIRST_FREE_CHANNEL..CHANNELS).contains(&ch) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "channel {ch} is not free to use (channels {FIRST_FREE_CHANNEL} to {} are)",
                        CHANNELS - 1
                    ),
                ));
            }
            if seen & (1 << ch) != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("channel {ch} listed twice"),
                ));
            }
            seen |= 1 << ch;
        }
        Ok(Self {
            regs,
            channels: channels.to_vec(),
            addresses,
        })
    }

    /// What the controller reports now (see [`report`]).
    pub fn report(&self) -> Report {
        report(&self.regs)
    }

    /// The configured channels that are idle now, in the order they were given.
    pub fn idle_channels(&self) -> Vec<u8> {
        let enabled = self.report().enabled;
        self.channels
            .iter()
            .copied()
            .filter(|&ch| enabled & (1 << ch) == 0)
            .collect()
    }

    /// The register access, for the caller's own checks.
    pub fn registers(&self) -> &R {
        &self.regs
    }

    fn finish(&mut self, ticket: &Ticket<'_>, status: u32) {
        let base = channel_offset(ticket.channel);
        let _ = self.regs.write32(base + CH_INTCLEAR, status);
    }
}

/// The widest of 8, 4, 2 and 1 bytes that divides the length and both
/// addresses, as the `CH_CTL` width code (log2 of the bytes).
fn transfer_width(len: u64, src: u64, dst: u64) -> u32 {
    [8u64, 4, 2, 1]
        .into_iter()
        .find(|w| (len | src | dst) % w == 0)
        .map_or(0, |w| w.trailing_zeros())
}

fn failed(kind: ErrorKind, reason: impl Into<String>) -> TransferError {
    TransferError::Failed {
        kind,
        reason: reason.into(),
    }
}

impl<R: Registers, A: BusAddress> TransferEngine for Rp1DmaEngine<R, A> {
    type Ticket<'a> = Ticket<'a>;

    fn submit<'a>(&mut self, transfer: Transfer<'a>) -> TransferResult<Ticket<'a>> {
        let (src, dst) = match (transfer.source, transfer.sink) {
            (
                Some(Source::DmaBuf { fd: src, len: slen }),
                Some(Sink::DmaBuf { fd: dst, len: dlen }),
            ) => {
                if slen != dlen {
                    return Err(TransferError::Invalid(format!(
                        "copy of {slen} bytes into {dlen}"
                    )));
                }
                (Some((src, slen)), Some(dst))
            }
            _ => {
                return Err(TransferError::Unsupported(
                    "the RP1 DMAC copies dma-buf to dma-buf",
                ));
            }
        };
        let ((src_fd, len), Some(dst_fd)) = (src.unwrap(), dst) else {
            unreachable!("both sides are checked above");
        };
        if len == 0 {
            return Err(TransferError::Invalid("an empty copy".into()));
        }
        let len = len as u64;
        let src_addr = self
            .addresses
            .bus_address(src_fd)
            .map_err(|e| failed(ErrorKind::from_io(e.kind()), e.to_string()))?;
        let dst_addr = self
            .addresses
            .bus_address(dst_fd)
            .map_err(|e| failed(ErrorKind::from_io(e.kind()), e.to_string()))?;
        let width = transfer_width(len, src_addr, dst_addr);
        let transfers = len >> width;
        if transfers == 0 || transfers > MAX_BLOCK_TRANSFERS {
            return Err(TransferError::Invalid(format!(
                "a {len}-byte copy is {transfers} transfers of {} bytes (1 to {MAX_BLOCK_TRANSFERS} allowed)",
                1u64 << width
            )));
        }

        let report = self.report();
        if !report.controller_enabled {
            return Err(failed(
                ErrorKind::Unsupported,
                "the DMAC is not running (DMAC_CFG.DMAC_EN clear)",
            ));
        }
        let Some(&channel) = self.idle_channels().first() else {
            return Err(failed(ErrorKind::Busy, "no free DMA channel is idle"));
        };

        let base = channel_offset(channel);
        let w = |regs: &mut R, offset: usize, value: u32| {
            regs.write32(base + offset, value)
                .map_err(|e| failed(ErrorKind::from_io(e.kind()), e.to_string()))
        };
        let regs = &mut self.regs;
        // The channel is idle, so none of this touches a running transfer.
        w(regs, CH_INTCLEAR, u32::MAX)?;
        w(regs, CH_SAR, src_addr as u32)?;
        w(regs, CH_SAR + 4, (src_addr >> 32) as u32)?;
        w(regs, CH_DAR, dst_addr as u32)?;
        w(regs, CH_DAR + 4, (dst_addr >> 32) as u32)?;
        w(regs, CH_BLOCK_TS, (transfers - 1) as u32)?;
        // One block, incrementing both sides, single-beat bursts, master 0 both
        // sides, no linked list. Width codes: SRC_WIDTH at bit 8, DST_WIDTH at bit 11.
        let code = width << 8 | width << 11;
        w(regs, CH_CTL_L, code)?;
        w(regs, CH_CTL_H, 0)?;
        w(regs, CH_CFG_L, 0)?;
        // Software handshaking on both sides (HS_SEL_SRC bit 3, HS_SEL_DST bit 4):
        // a memory copy has no peripheral to request it.
        w(regs, CH_CFG_H, (1 << 3) | (1 << 4))?;
        w(regs, CH_LLP, 0)?;
        // Enable: the channel bit and its write enable (bit 8 + channel).
        regs.write32(CHEN, (1 << channel) | (1 << (8 + channel)))
            .map_err(|e| failed(ErrorKind::from_io(e.kind()), e.to_string()))?;
        Ok(Ticket {
            channel,
            len: len as usize,
            _buffers: PhantomData,
        })
    }

    fn poll<'a>(&mut self, ticket: &mut Ticket<'a>) -> TransferResult<Option<Completion>> {
        let base = channel_offset(ticket.channel);
        let status = self.regs.read32(base + CH_INTSTATUS);
        if status & IRQ_ALL_ERR != 0 {
            self.finish(ticket, status);
            return Err(failed(
                ErrorKind::Failed,
                format!("the channel reported errors ({status:#010x})"),
            ));
        }
        if status & IRQ_DMA_TRF != 0 {
            self.finish(ticket, status);
            return Ok(Some(Completion { bytes: ticket.len }));
        }
        // An idle channel that never raised the transfer-done bit has stopped
        // without finishing: report it rather than wait forever.
        if self.report().enabled & (1 << ticket.channel) == 0 {
            return Err(failed(
                ErrorKind::Failed,
                "the channel stopped without finishing its copy",
            ));
        }
        Ok(None)
    }

    fn wait<'a>(
        &mut self,
        ticket: &mut Ticket<'a>,
        timeout: Option<Duration>,
    ) -> TransferResult<Option<Completion>> {
        let deadline = timeout.map(|t| Instant::now() + t);
        loop {
            if let Some(done) = self.poll(ticket)? {
                return Ok(Some(done));
            }
            if deadline.is_some_and(|d| Instant::now() >= d) {
                return Ok(None);
            }
            std::thread::sleep(Duration::from_micros(50));
        }
    }

    /// There is no abort in the tree (the controller's abort register is not
    /// wired), so a cancel waits for the copy to finish, then releases it.
    /// Dropping an unfinished ticket would leave the controller writing into
    /// buffers the caller may already have freed.
    fn cancel<'a>(&mut self, mut ticket: Ticket<'a>) -> TransferResult<()> {
        self.wait(&mut ticket, None).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::os::fd::AsRawFd;

    /// A register file that completes a copy when its channel is enabled: the
    /// transfer-done bit rises and the channel's enable clears on the next poll.
    #[derive(Default)]
    struct Fake {
        words: HashMap<usize, u32>,
        /// Channels whose status reports an error instead of completing.
        erring: u32,
        /// Whether a poll should see the channel stop without finishing.
        stop_silently: bool,
        writes: Vec<(usize, u32)>,
    }

    impl Registers for Fake {
        fn read32(&self, offset: usize) -> u32 {
            let raw = self.words.get(&offset).copied().unwrap_or(0);
            if offset == CHEN {
                // An enabled channel stays enabled until its status is written
                // (completion or error); the write-enable bits are not state.
                let mut out = 0;
                for ch in 0..8u8 {
                    let done = self
                        .words
                        .contains_key(&(channel_offset(ch) + CH_INTSTATUS));
                    if raw & (1 << ch) != 0 && !done {
                        out |= 1 << ch;
                    }
                }
                return out;
            }
            raw
        }

        fn write32(&mut self, offset: usize, value: u32) -> io::Result<()> {
            self.writes.push((offset, value));
            if offset == CHEN {
                // Completing or erroring happens when the enable is written.
                for ch in 0..8u8 {
                    let bit = 1 << ch;
                    if value & bit != 0 && value & (bit << 8) != 0 {
                        let status_at = channel_offset(ch) + CH_INTSTATUS;
                        if self.erring & bit != 0 {
                            self.words.insert(status_at, IRQ_ALL_ERR);
                        } else if self.stop_silently {
                            self.words.insert(status_at, 0);
                        } else {
                            self.words.insert(status_at, IRQ_DMA_TRF);
                        }
                    }
                }
            }
            if offset >= 0x100 && offset % 0x100 == CH_INTCLEAR {
                // A clear removes the status bits written.
                let status_at = offset - CH_INTCLEAR + CH_INTSTATUS;
                let cur = self.words.get(&status_at).copied().unwrap_or(0);
                self.words.insert(status_at, cur & !value);
                return Ok(());
            }
            self.words.insert(offset, value);
            Ok(())
        }
    }

    fn running() -> Fake {
        let mut f = Fake::default();
        f.words.insert(CFG, CFG_DMAC_EN | 2);
        f.words.insert(COMPVER, 0x3130_332a);
        f
    }

    /// A buffer stand-in: the engine only asks the address map for it, so a
    /// file serves as the descriptor.
    fn descriptor() -> std::fs::File {
        std::fs::File::open("/dev/null").unwrap()
    }

    fn copy_of<'a>(len: usize, src: BorrowedFd<'a>, dst: BorrowedFd<'a>) -> Transfer<'a> {
        Transfer {
            source: Some(Source::DmaBuf { fd: src, len }),
            sink: Some(Sink::DmaBuf { fd: dst, len }),
        }
    }

    #[test]
    fn channels_outside_the_free_range_are_refused() {
        for bad in [0u8, 1, 2, 8, 9] {
            assert!(
                Rp1DmaEngine::new(running(), &[bad], |_: BorrowedFd<'_>| Ok(0)).is_err(),
                "channel {bad}"
            );
        }
        assert!(Rp1DmaEngine::new(running(), &[3, 3], |_: BorrowedFd<'_>| Ok(0)).is_err());
        assert!(Rp1DmaEngine::new(running(), &[3, 7], |_: BorrowedFd<'_>| Ok(0)).is_ok());
    }

    #[test]
    fn the_report_reads_version_and_channel_state_only() {
        let mut f = running();
        // Channel 3 enabled, with its write enable at bit 11 (not state).
        f.words.insert(CHEN, (1 << 3) | (1 << 11));
        let r = report(&f);
        assert_eq!(r.version, 0x3130_332a);
        assert!(r.controller_enabled);
        assert_eq!(r.enabled, 0b1000);
        assert!(f.writes.is_empty(), "reading the report writes nothing");
    }

    #[test]
    fn the_engine_takes_the_first_idle_free_channel() {
        let mut f = running();
        f.words.insert(CHEN, 1 << 3); // channel 3 is busy
        let mut e = Rp1DmaEngine::new(f, &[3, 4, 5], |_: BorrowedFd<'_>| Ok(0x10_0000u64)).unwrap();
        assert_eq!(e.idle_channels(), vec![4, 5]);
        let (a, b) = (descriptor(), descriptor());
        let mut t = e.submit(copy_of(64, a.as_fd_ref(), b.as_fd_ref())).unwrap();
        assert_eq!(t.channel, 4);
        assert_eq!(
            e.wait(&mut t, Some(Duration::from_secs(1))).unwrap(),
            Some(Completion { bytes: 64 })
        );
    }

    #[test]
    fn a_copy_programs_the_addresses_width_and_block_size() {
        let mut f = running();
        f.words.insert(CHEN, 0);
        let (a, b) = (descriptor(), descriptor());
        let src_raw = a.as_raw_fd();
        let mut e = Rp1DmaEngine::new(f, &[3], move |fd: BorrowedFd<'_>| {
            // The two descriptors get distinct RP1-view addresses.
            Ok(if fd.as_raw_fd() == src_raw {
                0x1_0000_1000
            } else {
                0x1_0000_9000
            })
        })
        .unwrap();
        let t = e.submit(copy_of(4096, a.as_fd_ref(), b.as_fd_ref()));
        let mut t = t.unwrap();
        let base = channel_offset(3);
        let w = &e.regs.writes;
        let at = |off: usize| {
            w.iter()
                .rev()
                .find(|(o, _)| *o == base + off)
                .map(|(_, v)| *v)
        };
        assert_eq!(at(CH_SAR), Some(0x1000), "source low word");
        assert_eq!(at(CH_SAR + 4), Some(1), "source high word");
        assert_eq!(at(CH_DAR), Some(0x9000));
        assert_eq!(
            at(CH_BLOCK_TS),
            Some(4096 / 8 - 1),
            "8-byte transfers, minus one"
        );
        assert_eq!(
            at(CH_CTL_L),
            Some(3 << 8 | 3 << 11),
            "8-byte source and destination widths"
        );
        assert_eq!(at(CH_CFG_H), Some(0x18), "software handshaking");
        let (off, value) = *w.last().unwrap();
        assert_eq!(
            (off, value),
            (CHEN, (1 << 3) | (1 << 11)),
            "channel 3 enabled last"
        );
        e.wait(&mut t, Some(Duration::from_secs(1))).unwrap();
    }

    #[test]
    fn the_width_falls_back_to_what_the_addresses_allow() {
        assert_eq!(transfer_width(4096, 0x1000, 0x2000), 3);
        assert_eq!(
            transfer_width(4096, 0x1004, 0x2000),
            2,
            "a 4-byte aligned source"
        );
        assert_eq!(
            transfer_width(4099, 0x1000, 0x2000),
            0,
            "an odd length moves bytes"
        );
    }

    #[test]
    fn a_busy_controller_or_channel_is_refused_before_programming() {
        let mut f = running();
        f.words
            .insert(CHEN, (1 << 3) | (1 << 4) | (1 << 5) | (1 << 6) | (1 << 7));
        let mut e = Rp1DmaEngine::new(f, &[3, 4], |_: BorrowedFd<'_>| Ok(0u64)).unwrap();
        let (a, b) = (descriptor(), descriptor());
        let err = e
            .submit(copy_of(64, a.as_fd_ref(), b.as_fd_ref()))
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Busy);
        assert!(
            e.regs.writes.is_empty(),
            "nothing is written to a busy channel"
        );
    }

    #[test]
    fn a_stopped_controller_is_refused() {
        let mut f = Fake::default();
        f.words.insert(CFG, 0);
        let mut e = Rp1DmaEngine::new(f, &[3], |_: BorrowedFd<'_>| Ok(0u64)).unwrap();
        let (a, b) = (descriptor(), descriptor());
        assert_eq!(
            e.submit(copy_of(64, a.as_fd_ref(), b.as_fd_ref()))
                .unwrap_err()
                .kind(),
            ErrorKind::Unsupported
        );
    }

    #[test]
    fn a_copy_that_errors_reports_the_failure() {
        let mut f = running();
        f.erring = 1 << 3;
        let mut e = Rp1DmaEngine::new(f, &[3], |_: BorrowedFd<'_>| Ok(0u64)).unwrap();
        let (a, b) = (descriptor(), descriptor());
        let mut t = e.submit(copy_of(64, a.as_fd_ref(), b.as_fd_ref())).unwrap();
        assert!(e.poll(&mut t).is_err());
    }

    #[test]
    fn a_channel_that_stops_without_finishing_is_reported() {
        let mut f = running();
        f.stop_silently = true;
        let mut e = Rp1DmaEngine::new(f, &[3], |_: BorrowedFd<'_>| Ok(0u64)).unwrap();
        let (a, b) = (descriptor(), descriptor());
        let mut t = e.submit(copy_of(64, a.as_fd_ref(), b.as_fd_ref())).unwrap();
        assert!(e.poll(&mut t).is_err());
    }

    #[test]
    fn only_dma_buf_to_dma_buf_copies_are_taken() {
        let mut e = Rp1DmaEngine::new(running(), &[3], |_: BorrowedFd<'_>| Ok(0u64)).unwrap();
        let mut dst = [0u8; 8];
        let (a, _b) = (descriptor(), descriptor());
        let err = e
            .submit(Transfer {
                source: Some(Source::DmaBuf {
                    fd: a.as_fd_ref(),
                    len: 8,
                }),
                sink: Some(Sink::Memory(&mut dst)),
            })
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Unsupported);
    }

    trait AsFdRef {
        fn as_fd_ref(&self) -> BorrowedFd<'_>;
    }
    impl AsFdRef for std::fs::File {
        fn as_fd_ref(&self) -> BorrowedFd<'_> {
            std::os::fd::AsFd::as_fd(self)
        }
    }
}
