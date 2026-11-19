//! The stage's device-mode driver over the DesignWare OTG controller (`dwc2`) at `0x91500000`: the
//! register sequences, and nothing else. The device logic that uses them is
//! `lemnos_k230_usbboot_stage::usb_dev` (the ROM-ID stage) and `fl1::usb` (the FL1 stage); this file
//! implements their [`UsbHw`] trait and keeps the packet reassembly they do not see. Polling, slave
//! mode (no DMA, no interrupts).
//!
//! Register facts. Base `board.h:240-241` (`USB2_BASE_ADDR` 0x91500000, window 0x80000). Offsets and
//! fields from Linux 7.3.3 `drivers/usb/dwc2/hw.h` (the kernel tarball on the bench, read only):
//! GAHBCFG `0x008` (DMA_EN bit 5, :46-64), GUSBCFG `0x00c` (FORCEDEVMODE bit 30, FORCEHOSTMODE bit 29,
//! :66-67), GRSTCTL `0x010` (AHBIDLE 31, CSFTRST_DONE 29, TXFNUM 10:6, TXFFLSH 5, RXFFLSH 4, CSFTRST 0,
//! :98-118), GINTSTS `0x014` (ENUMDONE 13, USBRST 12, RXFLVL 4, CURMOD 0, :124-158), GRXSTSP `0x020`
//! (PKTSTS 20:17, BCNT 14:4, EPNUM 3:0, :161-174), GRXFSIZ `0x024` (:184), GNPTXFSIZ `0x028`,
//! GSNPSID `0x040` (:224), GHWCFG3 `0x04c` (FIFO depth 31:16, packet-count width 6:4, transfer-size width
//! 3:0, :272-287), DPTXFSIZN(n) `0x104+4(n-1)` (:420), DCFG `0x800` (DEVADDR 10:4, DEVSPD 1:0, :432),
//! DCTL `0x804` (SFTDISCON 1, :455), DSTS `0x808` (ENUMSPD 2:1: 0 high, 1 full, 2 low, 3 full at 48 MHz,
//! :469-480), DIEPCTL(n) `0x900+0x20n` and DOEPCTL(n) `0xb00+0x20n` (:522-525; EPENA 31, EPDIS 30,
//! SETD0PID 28, SNAK 27, CNAK 26, TXFNUM 25:22, STALL 21, EPTYPE 19:18, USBACTEP 15, MPS 10:0 or, on
//! endpoint 0, 1:0 with 0 = 64 bytes, :532-551), DIEPTSIZ0 (PKTCNT 20:19, XFERSIZE 6:0, :595-603),
//! DOEPTSIZ0 (SUPCNT 30:29, PKTCNT 19, XFERSIZE 6:0, :605-612), DIEPTSIZ(n)/DOEPTSIZ(n) `0x910/0xb10+0x20n`
//! (PKTCNT 28:19, XFERSIZE 18:0, :614-629), DTXFSTS(n) `0x918+0x20n` (:634), EPFIFO(n) `0x1000+0x1000n`
//! (:668). The counter widths: Linux `params.c:993,996` (`(1 << (width + 11)) - 1` bytes and
//! `(1 << (width + 4)) - 1` packets).
//!
//! The sequences follow the Linux driver (`core.c` `dwc2_core_reset`, `gadget.c` USB reset handling):
//! a soft disconnect before the core reset so the host sees the ROM's device leave; the reset's
//! completion by either bit the core revision uses (CSFTRST clears before 4.20a, CSFTRST_DONE sets
//! after); the AHB idle wait; device mode forced and waited for; DMA off (the ROM's own setting is
//! not trusted); endpoint 0's OUT re-armed after every setup and status stage; on a bus reset the
//! address cleared and the bulk pair deactivated, then activated again by SET_CONFIGURATION
//! ([`UsbHw::configure`]); bulk OUT transfers armed in whole packets and continued until a short
//! packet; bulk IN written one packet at a time as the FIFO has room.
//!
//! UNVERIFIED on hardware: all of it. The B2 run records what the bench must see ([`Stats`]).
//! Every wait is bounded by [`SPIN`] reads.

use core::sync::atomic::{AtomicU32, Ordering};

use lemnos_k230_usbboot_stage::usb_dev::{BULK_OUT, Event, HwError, RX_MAX, UsbHw};

/// The controller's base (`USB2_BASE_ADDR`, `board.h:240`).
pub const BASE: usize = 0x9150_0000;
/// The controller's register window (`USB2_IO_SIZE`, `board.h:241`).
pub const WINDOW: usize = 0x8_0000;
/// Reads a wait makes before it gives up.
pub const SPIN: u32 = 1_000_000;
/// Reads a bulk or control IN packet waits for transmit FIFO room: the host takes the packets only
/// when it asks for the reply, so this wait includes the host's own latency. The read's cost on the
/// board is not measured; the record's `in-fifo-waits` shows how much of this was used.
pub const IN_SPIN: u32 = 20_000_000;
/// Reads the soft disconnect is held for before the core reset (at least some tens of milliseconds
/// at any register read time the SoC can have), so the host's hub sees the ROM's device leave.
pub const DISCONNECT_READS: u32 = 400_000;

/// Register offsets (see the module docs for each source line). The names are the Linux driver's.
#[allow(missing_docs)]
pub mod reg {
    pub const GAHBCFG: usize = 0x008;
    pub const GUSBCFG: usize = 0x00c;
    pub const GRSTCTL: usize = 0x010;
    pub const GINTSTS: usize = 0x014;
    pub const GRXSTSP: usize = 0x020;
    pub const GRXFSIZ: usize = 0x024;
    pub const GNPTXFSIZ: usize = 0x028;
    pub const GSNPSID: usize = 0x040;
    pub const GHWCFG3: usize = 0x04c;
    pub const DPTXFSIZN1: usize = 0x104;
    pub const DCFG: usize = 0x800;
    pub const DCTL: usize = 0x804;
    pub const DSTS: usize = 0x808;

    pub const fn diepctl(n: u8) -> usize {
        0x900 + 0x20 * n as usize
    }
    pub const fn doepctl(n: u8) -> usize {
        0xb00 + 0x20 * n as usize
    }
    pub const fn dieptsiz(n: u8) -> usize {
        0x910 + 0x20 * n as usize
    }
    pub const fn doeptsiz(n: u8) -> usize {
        0xb10 + 0x20 * n as usize
    }
    pub const fn dtxfsts(n: u8) -> usize {
        0x918 + 0x20 * n as usize
    }
    pub const fn epfifo(n: u8) -> usize {
        0x1000 + 0x1000 * n as usize
    }
}

/// Register fields (see the module docs for each source line). The names are the Linux driver's.
#[allow(missing_docs)]
pub mod bits {
    pub const GAHBCFG_DMA_EN: u32 = 1 << 5;
    pub const GUSBCFG_FORCEDEVMODE: u32 = 1 << 30;
    pub const GUSBCFG_FORCEHOSTMODE: u32 = 1 << 29;
    pub const GRSTCTL_AHBIDLE: u32 = 1 << 31;
    pub const GRSTCTL_CSFTRST_DONE: u32 = 1 << 29;
    pub const GRSTCTL_TXFNUM_ALL: u32 = 0x10 << 6;
    pub const GRSTCTL_TXFFLSH: u32 = 1 << 5;
    pub const GRSTCTL_RXFFLSH: u32 = 1 << 4;
    pub const GRSTCTL_CSFTRST: u32 = 1 << 0;
    pub const GINTSTS_ENUMDONE: u32 = 1 << 13;
    pub const GINTSTS_USBRST: u32 = 1 << 12;
    pub const GINTSTS_RXFLVL: u32 = 1 << 4;
    pub const GINTSTS_CURMOD_HOST: u32 = 1 << 0;
    pub const DCTL_SFTDISCON: u32 = 1 << 1;
    pub const DCFG_DEVADDR_MASK: u32 = 0x7f << 4;
    pub const DXEPCTL_EPENA: u32 = 1 << 31;
    pub const DXEPCTL_EPDIS: u32 = 1 << 30;
    pub const DXEPCTL_SETD0PID: u32 = 1 << 28;
    pub const DXEPCTL_SNAK: u32 = 1 << 27;
    pub const DXEPCTL_CNAK: u32 = 1 << 26;
    pub const DXEPCTL_STALL: u32 = 1 << 21;
    pub const DXEPCTL_USBACTEP: u32 = 1 << 15;
    pub const EPTYPE_BULK: u32 = 2 << 18;
    pub const TXFNUM_1: u32 = 1 << 22;
    pub const PKTSTS_OUTRX: u32 = 2;
    pub const PKTSTS_OUTDONE: u32 = 3;
    pub const PKTSTS_SETUPDONE: u32 = 4;
    pub const PKTSTS_SETUPRX: u32 = 6;
    pub const DOEPTSIZ0_SETUP: u32 = (3 << 29) | (1 << 19) | 64;
}

use bits::*;
use reg::*;

/// The bulk endpoints' packet size (high speed).
pub const BULK_MPS: usize = 512;
/// Endpoint 0's packet size.
pub const EP0_MPS: usize = 64;

/// The FIFO layout, in 32-bit words: receive, endpoint 0 transmit, bulk IN transmit (two packets).
pub const RX_FIFO_WORDS: u32 = 512;
/// See [`RX_FIFO_WORDS`].
pub const TX0_FIFO_WORDS: u32 = 64;
/// See [`RX_FIFO_WORDS`].
pub const TX1_FIFO_WORDS: u32 = 256;
/// The FIFO RAM the layout needs. A core that reports less in GHWCFG3 is refused at bring-up.
pub const FIFO_WORDS: u32 = RX_FIFO_WORDS + TX0_FIFO_WORDS + TX1_FIFO_WORDS;

/// The default bulk OUT buffer: the ROM-ID stage's largest message, in whole packets.
pub const RX_DEFAULT: usize = RX_MAX.div_ceil(BULK_MPS) * BULK_MPS;

/// The controller's registers, as offsets from its base. The board's are [`Window`]; a test's are a
/// model of the core.
pub trait Dwc2Regs {
    /// Reads the register at `offset`.
    fn read(&self, offset: usize) -> u32;
    /// Writes `value` to the register at `offset`.
    fn write(&self, offset: usize, value: u32);
}

/// The controller's registers at [`BASE`].
pub struct Window;

impl Dwc2Regs for Window {
    fn read(&self, offset: usize) -> u32 {
        let addr = BASE + offset;
        if !in_window(addr) {
            return 0;
        }
        // SAFETY: `in_window` holds, so `addr` is an aligned register of the controller; a read of an
        // OTG register has no effect beyond the documented status it returns (GRXSTSP pops the entry
        // the caller then consumes, EPFIFO the word it reads).
        unsafe { crate::read32(addr) }
    }

    fn write(&self, offset: usize, value: u32) {
        let addr = BASE + offset;
        if !in_window(addr) {
            return;
        }
        // SAFETY: `in_window` holds, so `addr` is an aligned register of the controller; the callers
        // write only the documented control values.
        unsafe { crate::write32(addr, value) }
    }
}

/// True when `addr` is an aligned register of the controller's window.
fn in_window(addr: usize) -> bool {
    addr.is_multiple_of(4) && (BASE..BASE + WINDOW).contains(&addr)
}

/// What the bench must see, counted as it happens. The B2 run prints and serves these words
/// (`lemnos_k230_usbboot_stage::fl1::check::record`, same names). Counts saturate. Each field is the
/// record word of its name.
#[derive(Default)]
#[allow(missing_docs)]
pub struct Stats {
    pub gsnpsid: AtomicU32,
    pub ghwcfg3: AtomicU32,
    /// GAHBCFG as the ROM left it, before this driver wrote it.
    pub gahbcfg_found: AtomicU32,
    /// How the core reset ended: 1 CSFTRST cleared, 2 CSFTRST_DONE set, 0 not seen.
    pub reset_done: AtomicU32,
    /// DSTS at the last ENUMDONE, and its ENUMSPD field.
    pub dsts: AtomicU32,
    pub speed: AtomicU32,
    pub bus_resets: AtomicU32,
    pub setups: AtomicU32,
    /// The address of the last SET_ADDRESS, and DCFG read back right after it was written.
    pub set_address: AtomicU32,
    pub dcfg_after_address: AtomicU32,
    /// 1 when the SET_ADDRESS status stage (an IN zero-length packet) went out after the address
    /// was written: the order `address_before_status` states.
    pub address_status_in: AtomicU32,
    /// Endpoint 0 OUT status stages completed (the host's zero-length packet after a control IN).
    pub ep0_status_out: AtomicU32,
    pub stalls: AtomicU32,
    pub configured: AtomicU32,
    /// Bulk OUT frames completed, the arms made after a frame was taken (one per frame), the arms
    /// that continued a frame longer than one hardware transfer, the last and the longest frame, and
    /// the frames longer than the buffer.
    pub out_frames: AtomicU32,
    pub out_rearms: AtomicU32,
    pub out_continued: AtomicU32,
    pub out_last_len: AtomicU32,
    pub out_longest: AtomicU32,
    pub out_oversize: AtomicU32,
    pub in_transfers: AtomicU32,
    pub in_bytes: AtomicU32,
    /// The most reads one bulk IN packet waited for FIFO room.
    pub in_fifo_waits: AtomicU32,
}

impl Stats {
    /// All zero.
    pub const fn new() -> Self {
        Self {
            gsnpsid: AtomicU32::new(0),
            ghwcfg3: AtomicU32::new(0),
            gahbcfg_found: AtomicU32::new(0),
            reset_done: AtomicU32::new(0),
            dsts: AtomicU32::new(0),
            speed: AtomicU32::new(0),
            bus_resets: AtomicU32::new(0),
            setups: AtomicU32::new(0),
            set_address: AtomicU32::new(0),
            dcfg_after_address: AtomicU32::new(0),
            address_status_in: AtomicU32::new(0),
            ep0_status_out: AtomicU32::new(0),
            stalls: AtomicU32::new(0),
            configured: AtomicU32::new(0),
            out_frames: AtomicU32::new(0),
            out_rearms: AtomicU32::new(0),
            out_continued: AtomicU32::new(0),
            out_last_len: AtomicU32::new(0),
            out_longest: AtomicU32::new(0),
            out_oversize: AtomicU32::new(0),
            in_transfers: AtomicU32::new(0),
            in_bytes: AtomicU32::new(0),
            in_fifo_waits: AtomicU32::new(0),
        }
    }

    /// The words of the record this driver owns, by their record names.
    pub fn words(&self) -> [(&'static str, u32); 23] {
        let g = |a: &AtomicU32| a.load(Ordering::Relaxed);
        [
            ("gsnpsid", g(&self.gsnpsid)),
            ("ghwcfg3", g(&self.ghwcfg3)),
            ("gahbcfg-found", g(&self.gahbcfg_found)),
            ("reset-done", g(&self.reset_done)),
            ("dsts", g(&self.dsts)),
            ("speed", g(&self.speed)),
            ("bus-resets", g(&self.bus_resets)),
            ("setups", g(&self.setups)),
            ("set-address", g(&self.set_address)),
            ("dcfg-after-address", g(&self.dcfg_after_address)),
            ("address-status-in", g(&self.address_status_in)),
            ("ep0-status-out", g(&self.ep0_status_out)),
            ("stalls", g(&self.stalls)),
            ("configured", g(&self.configured)),
            ("out-frames", g(&self.out_frames)),
            ("out-rearms", g(&self.out_rearms)),
            ("out-continued", g(&self.out_continued)),
            ("out-last-len", g(&self.out_last_len)),
            ("out-longest", g(&self.out_longest)),
            ("out-oversize", g(&self.out_oversize)),
            ("in-transfers", g(&self.in_transfers)),
            ("in-bytes", g(&self.in_bytes)),
            ("in-fifo-waits", g(&self.in_fifo_waits)),
        ]
    }
}

/// One plain load and store: the image runs one thread, so no read-modify-write instruction is needed.
fn bump(counter: &AtomicU32) {
    counter.store(
        counter.load(Ordering::Relaxed).saturating_add(1),
        Ordering::Relaxed,
    );
}

fn put(counter: &AtomicU32, value: u32) {
    counter.store(value, Ordering::Relaxed);
}

/// The stats a driver without a record writes to.
static NO_STATS: Stats = Stats::new();

/// The OTG controller as the stage's device logic sees it. `B` is the bulk OUT buffer: a frame is
/// reassembled there, up to its length.
pub struct Dwc2<'s, R: Dwc2Regs = Window, B: AsMut<[u8]> + AsRef<[u8]> = [u8; RX_DEFAULT]> {
    regs: R,
    rx: B,
    rx_len: usize,
    /// The frame in `rx` ended with a short packet.
    rx_short: bool,
    /// A complete bulk OUT frame that `poll` has reported, until it is taken.
    out_ready: bool,
    /// A setup packet waiting for [`UsbHw::poll`].
    setup: Option<[u8; 8]>,
    /// An endpoint 0 OUT transfer (a status stage) completed, with its length.
    ep0_out: Option<usize>,
    /// The bytes endpoint 0's OUT transfer has taken so far.
    ep0_rx: usize,
    /// The last setup's next stage is an OUT data stage (see [`UsbHw::stall`]).
    ep0_out_stage: bool,
    /// The largest OUT and IN transfer the core's counters take, in whole packets.
    max_xfer: usize,
    configured: bool,
    stats: &'s Stats,
}

impl Dwc2<'static> {
    /// The device driver over the controller at [`BASE`], with the default buffer and no record.
    /// Nothing is touched until the device logic calls [`UsbHw::core_reset`].
    pub fn new() -> Self {
        Dwc2::with(Window, [0; RX_DEFAULT], &NO_STATS)
    }
}

impl Default for Dwc2<'static> {
    fn default() -> Self {
        Self::new()
    }
}

impl<'s, R: Dwc2Regs, B: AsMut<[u8]> + AsRef<[u8]>> Dwc2<'s, R, B> {
    /// The driver over `regs`, reassembling bulk OUT frames in `rx` (its length rounded down to whole
    /// packets is the longest frame), counting into `stats`. Nothing is touched until
    /// [`UsbHw::core_reset`].
    pub fn with(regs: R, rx: B, stats: &'s Stats) -> Self {
        Self {
            regs,
            rx,
            rx_len: 0,
            rx_short: false,
            out_ready: false,
            setup: None,
            ep0_out: None,
            ep0_rx: 0,
            ep0_out_stage: false,
            max_xfer: BULK_MPS,
            configured: false,
            stats,
        }
    }

    /// The registers, for a test to look at.
    pub fn regs(&self) -> &R {
        &self.regs
    }

    fn rd(&self, offset: usize) -> u32 {
        self.regs.read(offset)
    }

    fn wr(&self, offset: usize, value: u32) {
        self.regs.write(offset, value);
    }

    fn set(&self, offset: usize, value: u32) {
        self.wr(offset, self.rd(offset) | value);
    }

    fn clear(&self, offset: usize, value: u32) {
        self.wr(offset, self.rd(offset) & !value);
    }

    /// Waits until `offset & mask` equals `want`, for at most [`SPIN`] reads.
    fn wait(&self, offset: usize, mask: u32, want: u32) -> Result<u32, HwError> {
        for i in 0..SPIN {
            if self.rd(offset) & mask == want {
                return Ok(i);
            }
        }
        Err(HwError::Timeout)
    }

    /// The buffer's capacity in whole packets.
    fn capacity(&self) -> usize {
        self.rx.as_ref().len() / BULK_MPS * BULK_MPS
    }

    /// Writes words to the FIFO of endpoint `n`, little-endian.
    fn fifo_write(&self, n: u8, data: &[u8]) {
        for chunk in data.chunks(4) {
            let mut word = [0u8; 4];
            word[..chunk.len()].copy_from_slice(chunk);
            self.wr(epfifo(n), u32::from_le_bytes(word));
        }
    }

    /// Arms endpoint 0's OUT for a setup or a status stage (three back-to-back setups, one packet). An
    /// endpoint that is still enabled is left as it is: its size register is not written under it.
    fn arm_ep0_out(&self) {
        if self.rd(doepctl(0)) & DXEPCTL_EPENA != 0 {
            return;
        }
        self.wr(doeptsiz(0), DOEPTSIZ0_SETUP);
        self.set(doepctl(0), DXEPCTL_EPENA | DXEPCTL_CNAK);
    }

    /// Arms bulk OUT endpoint 1 for the rest of the frame: whole packets, at most the core's counters
    /// and the buffer's room. Returns false when the buffer is full (the frame is complete at its
    /// capacity).
    fn arm_bulk_out(&self) -> bool {
        let room = self.capacity().saturating_sub(self.rx_len);
        let len = room.min(self.max_xfer);
        if len == 0 {
            return false;
        }
        let packets = (len / BULK_MPS) as u32;
        self.wr(doeptsiz(1), (packets << 19) | len as u32);
        self.set(doepctl(1), DXEPCTL_EPENA | DXEPCTL_CNAK);
        true
    }

    /// Deactivates the bulk pair: NAK both, disable an enabled OUT, flush the IN FIFO, drop the frame.
    fn deactivate_bulk(&mut self) {
        for offset in [doepctl(1), diepctl(1)] {
            let ctl = self.rd(offset);
            let mut next = (ctl | DXEPCTL_SNAK) & !(DXEPCTL_USBACTEP | DXEPCTL_EPENA);
            if ctl & DXEPCTL_EPENA != 0 {
                next |= DXEPCTL_EPDIS;
            }
            self.wr(offset, next);
        }
        self.wr(GRSTCTL, (1 << 6) | GRSTCTL_TXFFLSH);
        let _ = self.wait(GRSTCTL, GRSTCTL_TXFFLSH, 0);
        self.rx_len = 0;
        self.rx_short = false;
        self.out_ready = false;
        self.configured = false;
    }

    /// A bus reset: address 0, the bulk pair off, endpoint 0 ready for the next setup.
    fn bus_reset(&mut self) {
        self.wr(GINTSTS, GINTSTS_USBRST);
        bump(&self.stats.bus_resets);
        self.clear(DCFG, DCFG_DEVADDR_MASK);
        self.deactivate_bulk();
        self.setup = None;
        self.ep0_out = None;
        self.ep0_rx = 0;
        self.arm_ep0_out();
    }

    /// Pops one entry of the receive FIFO: its data goes to the frame (endpoint 1), the setup
    /// (endpoint 0 setup), or the status stage (endpoint 0 OUT).
    fn take_packet(&mut self) {
        let status = self.rd(GRXSTSP);
        let pktsts = (status >> 17) & 0xf;
        let ep = (status & 0xf) as u8;
        let bytes = ((status >> 4) & 0x7ff) as usize;
        match pktsts {
            PKTSTS_SETUPRX => {
                let mut setup = [0u8; 8];
                for chunk in setup.chunks_mut(4) {
                    let word = self.rd(epfifo(0)).to_le_bytes();
                    chunk.copy_from_slice(&word[..chunk.len()]);
                }
                // A later setup replaces an earlier one the device has not handled (the last
                // back-to-back setup is the one the host means).
                self.setup = Some(setup);
                // The direction of the stage after this setup: OUT only for a request with an OUT
                // data stage; every other request's next stage (data or status) is IN.
                self.ep0_out_stage =
                    setup[0] & 0x80 == 0 && u16::from_le_bytes([setup[6], setup[7]]) > 0;
                bump(&self.stats.setups);
            }
            PKTSTS_SETUPDONE => self.arm_ep0_out(),
            PKTSTS_OUTRX if ep == BULK_OUT & 0x0f => {
                let cap = self.rx.as_ref().len();
                let mut left = bytes;
                while left > 0 {
                    let word = self.rd(epfifo(1)).to_le_bytes();
                    let take = left.min(4);
                    if self.rx_len + take <= cap {
                        let at = self.rx_len;
                        self.rx.as_mut()[at..at + take].copy_from_slice(&word[..take]);
                    }
                    self.rx_len += take;
                    left -= take;
                }
                if bytes < BULK_MPS {
                    self.rx_short = true;
                }
            }
            PKTSTS_OUTRX => {
                // Endpoint 0 OUT data (the status stage has none): drain it.
                for _ in 0..bytes.div_ceil(4) {
                    let _ = self.rd(epfifo(0));
                }
                // Counted only: the status stage is reported once, at its OUTDONE entry. A
                // zero-length packet has its own OUTRX entry too, so reporting here as well would
                // report one status stage twice.
                if ep == 0 {
                    self.ep0_rx += bytes;
                }
            }
            PKTSTS_OUTDONE if ep == BULK_OUT & 0x0f => {
                if self.rx_short || !self.arm_bulk_out() {
                    self.out_ready = true;
                    bump(&self.stats.out_frames);
                    put(&self.stats.out_last_len, self.rx_len as u32);
                    if self.rx_len as u32 > self.stats.out_longest.load(Ordering::Relaxed) {
                        put(&self.stats.out_longest, self.rx_len as u32);
                    }
                    if self.rx_len > self.capacity() {
                        bump(&self.stats.out_oversize);
                    }
                } else {
                    bump(&self.stats.out_continued);
                }
            }
            PKTSTS_OUTDONE if ep == 0 => {
                self.ep0_out = Some(core::mem::take(&mut self.ep0_rx));
                bump(&self.stats.ep0_status_out);
                self.arm_ep0_out();
            }
            _ => {}
        }
    }

    /// One IN transfer on endpoint `n`, at most the core's counters, written packet by packet as the
    /// FIFO has room. It waits for the endpoint's previous transfer to end before it starts; it does
    /// not wait for this one to end (the next transfer, or [`UsbHw::in_ready`], does).
    fn in_transfer(&mut self, n: u8, data: &[u8]) -> Result<(), HwError> {
        let mps = if n == 0 { EP0_MPS } else { BULK_MPS };
        let packets = data.len().div_ceil(mps).max(1) as u32;
        // The previous transfer ends when the host has taken it: the same host-paced wait as the
        // FIFO room's.
        if !(0..IN_SPIN).any(|_| self.rd(diepctl(n)) & DXEPCTL_EPENA == 0) {
            return Err(HwError::Timeout);
        }
        self.wr(dieptsiz(n), (packets << 19) | data.len() as u32);
        self.set(diepctl(n), DXEPCTL_EPENA | DXEPCTL_CNAK);
        for packet in data.chunks(mps) {
            let words = packet.len().div_ceil(4) as u32;
            let waited = self.wait_room(n, words)?;
            if n != 0 && waited > self.stats.in_fifo_waits.load(Ordering::Relaxed) {
                put(&self.stats.in_fifo_waits, waited);
            }
            self.fifo_write(n, packet);
        }
        Ok(())
    }

    /// Waits for `words` of room in endpoint `n`'s transmit FIFO, for at most [`IN_SPIN`] reads.
    fn wait_room(&self, n: u8, words: u32) -> Result<u32, HwError> {
        for i in 0..IN_SPIN {
            if self.rd(dtxfsts(n)) & 0xffff >= words {
                return Ok(i);
            }
        }
        Err(HwError::Timeout)
    }
}

impl<R: Dwc2Regs, B: AsMut<[u8]> + AsRef<[u8]>> UsbHw for Dwc2<'_, R, B> {
    fn core_reset(&mut self) -> Result<(), HwError> {
        // The ROM's device leaves the bus first, so the host sees a new device, not a silent one.
        self.set(DCTL, DCTL_SFTDISCON);
        for _ in 0..DISCONNECT_READS {
            let _ = self.rd(GINTSTS);
        }
        self.wait(GRSTCTL, GRSTCTL_AHBIDLE, GRSTCTL_AHBIDLE)?;
        self.set(GRSTCTL, GRSTCTL_CSFTRST);
        // Before 4.20a the reset bit clears itself; from 4.20a CSFTRST_DONE sets and both are written
        // back (core.c dwc2_core_reset). Either ends the wait.
        let mut done = 0;
        for _ in 0..SPIN {
            let r = self.rd(GRSTCTL);
            if r & GRSTCTL_CSFTRST_DONE != 0 {
                self.wr(GRSTCTL, (r & !GRSTCTL_CSFTRST) | GRSTCTL_CSFTRST_DONE);
                done = 2;
                break;
            }
            if r & GRSTCTL_CSFTRST == 0 {
                done = 1;
                break;
            }
        }
        put(&self.stats.reset_done, done);
        if done == 0 {
            return Err(HwError::Timeout);
        }
        self.wait(GRSTCTL, GRSTCTL_AHBIDLE, GRSTCTL_AHBIDLE)?;
        // Stay off the bus while the core is set up.
        self.set(DCTL, DCTL_SFTDISCON);
        put(&self.stats.gsnpsid, self.rd(GSNPSID));
        let hwcfg3 = self.rd(GHWCFG3);
        put(&self.stats.ghwcfg3, hwcfg3);
        put(&self.stats.gahbcfg_found, self.rd(GAHBCFG));
        // Slave mode: DMA off, the global interrupt off (this driver polls).
        self.wr(GAHBCFG, 0);
        self.wr(
            GUSBCFG,
            (self.rd(GUSBCFG) | GUSBCFG_FORCEDEVMODE) & !GUSBCFG_FORCEHOSTMODE,
        );
        self.wait(GINTSTS, GINTSTS_CURMOD_HOST, 0)?;
        // The counters' widths and the FIFO RAM, as the core states them (GHWCFG3). A core that
        // reports no FIFO depth is taken at the layout's own size.
        let xfer_max = (1usize << ((hwcfg3 & 0xf) + 11)) - 1;
        let pkt_max = (1usize << (((hwcfg3 >> 4) & 0x7) + 4)) - 1;
        self.max_xfer = (xfer_max.min(pkt_max * BULK_MPS) / BULK_MPS * BULK_MPS).max(BULK_MPS);
        let depth = hwcfg3 >> 16;
        if depth != 0 && depth < FIFO_WORDS {
            return Err(HwError::Overrun);
        }
        self.wr(GRXFSIZ, RX_FIFO_WORDS);
        self.wr(GNPTXFSIZ, (TX0_FIFO_WORDS << 16) | RX_FIFO_WORDS);
        self.wr(
            DPTXFSIZN1,
            (TX1_FIFO_WORDS << 16) | (RX_FIFO_WORDS + TX0_FIFO_WORDS),
        );
        self.wr(GRSTCTL, GRSTCTL_TXFNUM_ALL | GRSTCTL_TXFFLSH);
        self.wait(GRSTCTL, GRSTCTL_TXFFLSH, 0)?;
        self.wr(GRSTCTL, GRSTCTL_RXFFLSH);
        self.wait(GRSTCTL, GRSTCTL_RXFFLSH, 0)?;
        // High speed, address 0; nothing pending; endpoint 0 at 64 bytes and ready for a setup.
        self.wr(DCFG, 0);
        self.wr(GINTSTS, 0xffff_ffff);
        self.wr(diepctl(0), 0);
        self.arm_ep0_out();
        self.clear(DCTL, DCTL_SFTDISCON);
        Ok(())
    }

    fn poll(&mut self) -> Event {
        let status = self.rd(GINTSTS);
        if status & GINTSTS_USBRST != 0 {
            self.bus_reset();
            return Event::BusReset;
        }
        if status & GINTSTS_ENUMDONE != 0 {
            self.wr(GINTSTS, GINTSTS_ENUMDONE);
            let dsts = self.rd(DSTS);
            put(&self.stats.dsts, dsts);
            put(&self.stats.speed, (dsts >> 1) & 0x3);
            self.arm_ep0_out();
        }
        if status & GINTSTS_RXFLVL != 0 {
            self.take_packet();
        }
        if let Some(setup) = self.setup.take() {
            return Event::Setup(setup);
        }
        if let Some(len) = self.ep0_out.take() {
            return Event::Out { ep: 0, len };
        }
        if self.out_ready {
            // Reported once; the frame stays until it is taken.
            self.out_ready = false;
            return Event::Out {
                ep: BULK_OUT,
                len: self.rx_len,
            };
        }
        Event::Idle
    }

    fn read_out(&mut self, ep: u8, buf: &mut [u8]) -> Result<usize, HwError> {
        if self.rx_len > buf.len() {
            return Err(HwError::Overrun);
        }
        self.read_out_part(ep, buf)
    }

    fn read_out_part(&mut self, ep: u8, buf: &mut [u8]) -> Result<usize, HwError> {
        if ep != BULK_OUT {
            return Err(HwError::Stalled);
        }
        let len = self.rx_len;
        let stored = len.min(self.rx.as_ref().len());
        let copy = stored.min(buf.len());
        buf[..copy].copy_from_slice(&self.rx.as_ref()[..copy]);
        self.rx_len = 0;
        self.rx_short = false;
        // The endpoint takes the next frame only now: one arm per frame taken.
        if self.configured && self.arm_bulk_out() {
            bump(&self.stats.out_rearms);
        }
        Ok(len)
    }

    fn write_in(&mut self, ep: u8, data: &[u8]) -> Result<(), HwError> {
        let n = ep & 0x0f;
        if n == 0 {
            if data.len() > EP0_MPS {
                return Err(HwError::Overrun);
            }
            self.in_transfer(0, data)?;
            if data.is_empty() && self.stats.set_address.load(Ordering::Relaxed) != 0 {
                put(&self.stats.address_status_in, 1);
            }
            return Ok(());
        }
        // A transfer longer than the counters take goes as several, each of whole packets but the last:
        // the host sees one transfer, which ends at the short packet.
        let mut rest = data;
        loop {
            let len = rest.len().min(self.max_xfer);
            let (now, later) = rest.split_at(len);
            self.in_transfer(n, now)?;
            rest = later;
            if rest.is_empty() {
                break;
            }
        }
        bump(&self.stats.in_transfers);
        put(
            &self.stats.in_bytes,
            self.stats
                .in_bytes
                .load(Ordering::Relaxed)
                .saturating_add(data.len() as u32),
        );
        Ok(())
    }

    fn in_ready(&self, ep: u8) -> bool {
        self.rd(diepctl(ep & 0x0f)) & DXEPCTL_EPENA == 0
    }

    fn stall(&mut self, ep: u8) {
        let n = ep & 0x0f;
        bump(&self.stats.stalls);
        if n == 0 {
            // The half that carries the request's next stage stalls, as Linux's
            // `dwc2_hsotg_stall_ep0` does: IN for a request with an IN data stage or none (its
            // status stage is IN), OUT for one with an OUT data stage. The core clears it at the
            // next setup. The OUT half is not stalled for the others: it is re-armed for the next
            // setup by a read-modify-write, which would carry a stall bit past the setup that
            // cleared it (found by the B2 rehearsal's register model).
            self.set(
                if self.ep0_out_stage {
                    doepctl(0)
                } else {
                    diepctl(0)
                },
                DXEPCTL_STALL,
            );
            return;
        }
        let offset = if ep & 0x80 != 0 {
            diepctl(n)
        } else {
            doepctl(n)
        };
        self.set(offset, DXEPCTL_STALL);
    }

    fn set_address(&mut self, address: u8) {
        // DCFG takes the address now; the core answers at it only after the status stage that
        // follows (the order `address_before_status` states for dwc2).
        let dcfg = self.rd(DCFG) & !DCFG_DEVADDR_MASK;
        self.wr(DCFG, dcfg | (u32::from(address & 0x7f) << 4));
        put(&self.stats.set_address, u32::from(address));
        put(&self.stats.dcfg_after_address, self.rd(DCFG));
        put(&self.stats.address_status_in, 0);
    }

    fn configure(&mut self, on: bool) {
        if !on {
            self.deactivate_bulk();
            return;
        }
        self.wr(
            doepctl(1),
            BULK_MPS as u32 | EPTYPE_BULK | DXEPCTL_USBACTEP | DXEPCTL_SETD0PID | DXEPCTL_SNAK,
        );
        self.wr(
            diepctl(1),
            BULK_MPS as u32
                | EPTYPE_BULK
                | DXEPCTL_USBACTEP
                | DXEPCTL_SETD0PID
                | DXEPCTL_SNAK
                | TXFNUM_1,
        );
        self.rx_len = 0;
        self.rx_short = false;
        self.out_ready = false;
        self.configured = true;
        put(&self.stats.configured, 1);
        self.arm_bulk_out();
    }

    fn enum_speed(&self) -> Option<u8> {
        Some(((self.rd(DSTS) >> 1) & 0x3) as u8)
    }

    fn rx_capacity(&self) -> usize {
        self.capacity()
    }
}
