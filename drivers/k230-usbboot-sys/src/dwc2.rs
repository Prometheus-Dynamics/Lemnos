//! The stage's device-mode driver over the DesignWare OTG controller (`dwc2`) at `0x91500000`: the
//! register sequences, and nothing else. The device logic that uses them is
//! `lemnos_k230_usbboot_stage::usb_dev`; this file implements its [`UsbHw`] trait with MMIO and keeps
//! the packet reassembly the device logic does not see. Polling, slave mode (no DMA, no interrupts).
//!
//! Register facts (kernel source at kendryte/k230_sdk 7e302f7, `src/little/linux/drivers/usb/dwc2/hw.h`
//! unless noted; base `board.h:240-241`, `USB2_BASE_ADDR` 0x91500000, window 0x80000):
//! GUSBCFG `0x00c` (FORCEDEVMODE bit 30, :94-95), GRSTCTL `0x010` (CSFTRST bit 0, :126-139),
//! GINTSTS `0x014` (USBRST bit 12, RXFLVL bit 4, :141-175), GRXSTSP `0x020` (PKTSTS bits 20:17, EPNUM
//! 3:0, BCNT 14:4, :178-199), GRXFSIZ `0x024` (:201), GNPTXFSIZ `0x028` and DPTXFSIZN(n) `0x104+4(n-1)`
//! (start in 15:0, depth in 31:16, :433-443), GNPTXSTS `0x02c` (:208-216), DCFG `0x800` (DEVADDR bits
//! 10:4, DEVSPD 1:0, :447-468), DCTL `0x804` (SFTDISCON bit 1, :470-482), DIEPCTL(n) `0x900+0x20n`
//! and DOEPCTL(n) `0xb00+0x20n` (:536-540), DIEPTSIZ(n) `0x910+0x20n` and DOEPTSIZ(n) `0xb10+0x20n`
//! (:610-636), DTXFSTS(n) `0x918+0x20n` (:649), the endpoint FIFOs EPFIFO(n) `0x1000+0x1000n` (:683).
//! DXEPCTL: EPENA 31, SNAK 27, CNAK 26, STALL 21, EPTYPE 19:18 (bulk 2), USBACTEP 15, MPS 10:0,
//! TXFNUM 25:22 (:14-45). The packet status values: OUTRX 2, OUTDONE 3, SETUPDONE 4, SETUPRX 6 (:183-190).
//!
//! The IN endpoint is `0x81` (bulk, endpoint 1) and its DIEPCTL is `diepctl(1)`.
//!
//! UNVERIFIED on hardware: the reset's completion wait, the FIFO space field's width (DTXFSTS's low
//! 16 bits, as the core uses it), the packet count of a transfer, and the order of the first enable.
//! Every wait is bounded by [`SPIN`] reads.

use lemnos_k230_usbboot_stage::usb_dev::{BULK_OUT, Event, HwError, RX_MAX, UsbHw};

/// The controller's base (`USB2_BASE_ADDR`, `board.h:240`).
pub const BASE: usize = 0x9150_0000;
/// The controller's register window (`USB2_IO_SIZE`, `board.h:241`).
pub const WINDOW: usize = 0x8_0000;
/// Reads a wait makes before it gives up.
pub const SPIN: u32 = 1_000_000;

const GUSBCFG: usize = 0x00c;
const GRSTCTL: usize = 0x010;
const GINTSTS: usize = 0x014;
const GRXSTSP: usize = 0x020;
const GRXFSIZ: usize = 0x024;
const GNPTXFSIZ: usize = 0x028;
const DPTXFSIZN1: usize = 0x104;
const DCFG: usize = 0x800;
const DCTL: usize = 0x804;
const DAINTMSK: usize = 0x81c;

const fn diepctl(n: u8) -> usize {
    0x900 + 0x20 * n as usize
}
const fn doepctl(n: u8) -> usize {
    0xb00 + 0x20 * n as usize
}
const fn dieptsiz(n: u8) -> usize {
    0x910 + 0x20 * n as usize
}
const fn doeptsiz(n: u8) -> usize {
    0xb10 + 0x20 * n as usize
}
const fn dtxfsts(n: u8) -> usize {
    0x918 + 0x20 * n as usize
}
const fn epfifo(n: u8) -> usize {
    0x1000 + 0x1000 * n as usize
}

const GUSBCFG_FORCEDEVMODE: u32 = 1 << 30;
const GUSBCFG_FORCEHOSTMODE: u32 = 1 << 29;
const GRSTCTL_CSFTRST: u32 = 1 << 0;
const GINTSTS_USBRST: u32 = 1 << 12;
const GINTSTS_RXFLVL: u32 = 1 << 4;
const DCTL_SFTDISCON: u32 = 1 << 1;
const DXEPCTL_EPENA: u32 = 1 << 31;
const DXEPCTL_CNAK: u32 = 1 << 26;
const DXEPCTL_STALL: u32 = 1 << 21;
const DXEPCTL_USBACTEP: u32 = 1 << 15;
const DXEPCTL_SETD0PID: u32 = 1 << 28;
const EPTYPE_BULK: u32 = 2 << 18;
const PKTSTS_OUTRX: u32 = 2;
const PKTSTS_OUTDONE: u32 = 3;
const PKTSTS_SETUPDONE: u32 = 4;
const PKTSTS_SETUPRX: u32 = 6;

/// The OTG controller as the stage's device logic sees it.
pub struct Dwc2 {
    /// A setup packet waiting for [`UsbHw::poll`].
    setup: Option<[u8; 8]>,
    /// A bulk OUT transfer, reassembled from its packets, waiting for [`UsbHw::read_out`].
    rx: [u8; RX_MAX],
    rx_len: usize,
    /// A completed bulk OUT transfer that `poll` has reported.
    out_ready: bool,
    /// A bus reset seen by the last interrupt read.
    reset: bool,
}

/// True when `addr` is an aligned register of the controller's window.
fn in_window(addr: usize) -> bool {
    addr.is_multiple_of(4) && (BASE..BASE + WINDOW).contains(&addr)
}

impl Dwc2 {
    /// The device driver over the controller at [`BASE`]. Nothing is touched until the device logic
    /// calls [`UsbHw::core_reset`].
    pub fn new() -> Self {
        Self {
            setup: None,
            rx: [0; RX_MAX],
            rx_len: 0,
            out_ready: false,
            reset: false,
        }
    }

    fn rd(&self, offset: usize) -> u32 {
        let addr = BASE + offset;
        if !in_window(addr) {
            return 0;
        }
        // SAFETY: `in_window` holds, so `addr` is an aligned register of the controller; a read of an
        // OTG register has no effect beyond the documented status it returns.
        unsafe { crate::read32(addr) }
    }

    fn wr(&self, offset: usize, value: u32) {
        let addr = BASE + offset;
        if !in_window(addr) {
            return;
        }
        // SAFETY: `in_window` holds, so `addr` is an aligned register of the controller; the callers
        // write only the documented control values.
        unsafe { crate::write32(addr, value) }
    }

    fn set(&self, offset: usize, bits: u32) {
        self.wr(offset, self.rd(offset) | bits);
    }

    fn clear(&self, offset: usize, bits: u32) {
        self.wr(offset, self.rd(offset) & !bits);
    }

    /// Writes words to the FIFO of endpoint `n`, little-endian.
    fn fifo_write(&self, n: u8, data: &[u8]) {
        for chunk in data.chunks(4) {
            let mut word = [0u8; 4];
            word[..chunk.len()].copy_from_slice(chunk);
            self.wr(epfifo(n), u32::from_le_bytes(word));
        }
    }

    /// Pops one received packet: its data goes to the OUT buffer (EP0 setups to `setup`).
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
                self.setup = Some(setup);
            }
            PKTSTS_OUTRX => {
                let mut left = bytes;
                while left > 0 {
                    let word = self.rd(epfifo(ep)).to_le_bytes();
                    let take = left.min(4);
                    if self.rx_len + take <= self.rx.len() && ep == BULK_OUT & 0x0f {
                        self.rx[self.rx_len..self.rx_len + take].copy_from_slice(&word[..take]);
                    }
                    self.rx_len += take;
                    left -= take;
                }
            }
            PKTSTS_OUTDONE if ep == BULK_OUT & 0x0f => {
                self.out_ready = true;
            }
            PKTSTS_OUTDONE | PKTSTS_SETUPDONE => {}
            _ => {}
        }
    }

    /// Arms bulk OUT endpoint 1 for the next transfer.
    fn arm_bulk_out(&self) {
        let packets = RX_MAX.div_ceil(BULK_MPS) as u32;
        self.wr(doeptsiz(1), (packets << 19) | RX_MAX as u32);
        self.set(doepctl(1), DXEPCTL_EPENA | DXEPCTL_CNAK);
    }
}

const BULK_MPS: usize = 512;

impl Default for Dwc2 {
    fn default() -> Self {
        Self::new()
    }
}

impl UsbHw for Dwc2 {
    fn core_reset(&mut self) -> Result<(), HwError> {
        self.set(GRSTCTL, GRSTCTL_CSFTRST);
        let mut done = false;
        for _ in 0..SPIN {
            if self.rd(GRSTCTL) & GRSTCTL_CSFTRST == 0 {
                done = true;
                break;
            }
        }
        if !done {
            return Err(HwError::Timeout);
        }
        // Device mode, not host mode (GUSBCFG, hw.h:94-96).
        self.wr(
            GUSBCFG,
            (self.rd(GUSBCFG) | GUSBCFG_FORCEDEVMODE) & !GUSBCFG_FORCEHOSTMODE,
        );
        // FIFOs: receive 2 KiB (512 words); endpoint 0 IN from 0x200, 64 words; bulk IN from 0x240,
        // 512 words (the layout is the device path's own, UNVERIFIED against the core's limits).
        self.wr(GRXFSIZ, 512);
        self.wr(GNPTXFSIZ, (64 << 16) | 0x200);
        self.wr(DPTXFSIZN1, (512 << 16) | 0x240);
        // Endpoint 0 at 64 bytes, and the bulk pair at 512 (DXEPCTL MPS, hw.h:42-45).
        self.wr(DAINTMSK, (1 << 0) | (1 << 1) | (1 << 16) | (1 << 17));
        // High speed (DEVSPD 0), no address yet, then connect (clear SFTDISCON, hw.h:447-482).
        self.wr(DCFG, 0);
        self.clear(DCTL, DCTL_SFTDISCON);
        // Endpoint 0 accepts its setup: DOEPTSIZ0 with three setup packets and one 64-byte packet.
        self.wr(doeptsiz(0), (3 << 29) | (1 << 19) | 64);
        self.set(doepctl(0), DXEPCTL_EPENA | DXEPCTL_CNAK);
        // The bulk pair.
        self.wr(
            doepctl(1),
            BULK_MPS as u32 | EPTYPE_BULK | DXEPCTL_USBACTEP | DXEPCTL_SETD0PID,
        );
        self.wr(
            diepctl(1),
            BULK_MPS as u32 | EPTYPE_BULK | DXEPCTL_USBACTEP | DXEPCTL_SETD0PID | (1 << 22),
        );
        self.arm_bulk_out();
        Ok(())
    }

    fn poll(&mut self) -> Event {
        let status = self.rd(GINTSTS);
        if status & GINTSTS_USBRST != 0 {
            self.wr(GINTSTS, GINTSTS_USBRST);
            self.reset = true;
        }
        if self.reset {
            self.reset = false;
            self.setup = None;
            self.out_ready = false;
            self.rx_len = 0;
            return Event::BusReset;
        }
        if let Some(setup) = self.setup.take() {
            return Event::Setup(setup);
        }
        if self.rd(GINTSTS) & GINTSTS_RXFLVL != 0 {
            self.take_packet();
            if let Some(setup) = self.setup.take() {
                return Event::Setup(setup);
            }
        }
        if self.out_ready {
            self.out_ready = false;
            return Event::Out {
                ep: BULK_OUT,
                len: self.rx_len,
            };
        }
        Event::Idle
    }

    fn read_out(&mut self, ep: u8, buf: &mut [u8]) -> Result<usize, HwError> {
        // The bytes of the last `Out` are in `rx`, reassembled by `take_packet`. Taking them frees
        // the endpoint for the next transfer.
        if ep != BULK_OUT {
            return Err(HwError::Stalled);
        }
        let len = self.rx_len;
        if len > buf.len() {
            return Err(HwError::Overrun);
        }
        buf[..len].copy_from_slice(&self.rx[..len]);
        self.rx_len = 0;
        self.arm_bulk_out();
        Ok(len)
    }

    fn write_in(&mut self, ep: u8, data: &[u8]) -> Result<(), HwError> {
        let n = ep & 0x0f;
        let packets = if data.is_empty() {
            1
        } else {
            data.len().div_ceil(if n == 0 { 64 } else { BULK_MPS })
        };
        let words = data.len().div_ceil(4) as u32;
        if n == 0 {
            self.wr(dieptsiz(0), (1 << 19) | data.len() as u32);
            self.set(diepctl(0), DXEPCTL_EPENA | DXEPCTL_CNAK);
        } else {
            // Wait for the FIFO to have room for the whole transfer (bounded).
            let mut room = false;
            for _ in 0..SPIN {
                if (self.rd(dtxfsts(n)) & 0xffff) >= words {
                    room = true;
                    break;
                }
            }
            if !room {
                return Err(HwError::Timeout);
            }
            self.wr(dieptsiz(n), ((packets as u32) << 19) | data.len() as u32);
            self.set(diepctl(n), DXEPCTL_EPENA | DXEPCTL_CNAK);
        }
        self.fifo_write(n, data);
        Ok(())
    }

    fn in_ready(&self, ep: u8) -> bool {
        self.rd(diepctl(ep & 0x0f)) & DXEPCTL_EPENA == 0
    }

    fn stall(&mut self, ep: u8) {
        let n = ep & 0x0f;
        let offset = if ep & 0x80 != 0 {
            diepctl(n)
        } else {
            doepctl(n)
        };
        self.set(offset, DXEPCTL_STALL);
    }

    fn set_address(&mut self, address: u8) {
        let dcfg = self.rd(DCFG) & !(0x7f << 4);
        self.wr(DCFG, dcfg | (u32::from(address & 0x7f) << 4));
    }
}
