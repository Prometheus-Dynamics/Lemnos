//! The B2 rehearsal's board: the check build's own code (`lemnos_driver_k230_usbboot_sys::b2`, the same
//! `check` and `end` the image runs, over the same `Dwc2` driver and the same FL1 device and session) in a
//! thread, over a register model of what that code touches:
//!
//! - the dwc2 core ([`Core`]): the register semantics the driver depends on (soft reset, the receive FIFO's
//!   status queue, endpoint enables, NAK and STALL, transfer sizes and packet counts, the transmit FIFOs'
//!   room, the device address latched only after a status stage, as the core does) and the host's side of
//!   the wire (setup, data and status stages; bulk packets, short-packet ends);
//! - the two watchdogs (`lemnos_k230_usbboot_stage::wdt::Model`, at 750 kHz, the clock the bench measured
//!   in C1b), run on the PC's clock; a watchdog that runs out resets the board;
//! - the console (the UART's registers; lines are kept with their time) and the SoC reset register;
//! - a kernel ([`kernel`]): it sees the pull-up, resets the bus, enumerates as Linux does (64-byte device
//!   descriptor read, a second reset, SET_ADDRESS, the descriptors and strings, SET_CONFIGURATION), and
//!   publishes what it read as the sysfs view [`crate::b2::Board::scan`] returns.
//!
//! The host check ([`crate::b2::run`]) talks to this board through the same [`Board`] and [`Link`] traits
//! it uses on the bench. A reset (the SoC reset the stage writes, or a watchdog) ends the stage's thread;
//! the boot ROM then shows at the port again under a new device number.
//!
//! What it is not: a timing model. The core accepts packets as fast as the threads run, the host's
//! scheduling is the PC's, and the register read cost is the PC's, not the board's.

use std::collections::{HashMap, VecDeque};
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use lemnos_driver_k230_usbboot_sys::b2::{self as stage_b2, Buffers, Known, RX_LEN};
use lemnos_driver_k230_usbboot_sys::dwc2::{Dwc2Regs, Stats, bits::*, reg::*};
use lemnos_flash_stage::serve::BUFFER_LEN;
use lemnos_flash_stage::wait::Clock;
use lemnos_flash_wire::usb::LANGUAGE;
use lemnos_k230_usbboot_stage::fl1::check::{TIMEBASE_HZ, WINDOW_LEN};
use lemnos_k230_usbboot_stage::pads::PadGuard;
use lemnos_k230_usbboot_stage::sysctl::{SOC_GLB_RST, SOC_RESET_VALUE, STC_ENABLES};
use lemnos_k230_usbboot_stage::wdt::{self, RegAccess};

use crate::b2::{Access, Board, Link, LinkError, PORT, Seen};
use crate::bootrom::{DOWNLOAD_PID, DOWNLOAD_VID};

/// The watchdogs' clock in the model: the bench's C1b measurement.
pub const WDT_HZ: u64 = 750_000;
/// GSNPSID the model reports (a core before 4.20a: CSFTRST clears itself).
pub const GSNPSID_PRE_420A: u32 = 0x4f54_400a;
/// GSNPSID of a core from 4.20a (CSFTRST_DONE sets and is written back).
pub const GSNPSID_420A: u32 = 0x4f54_420a;
/// GHWCFG3: 2048 words of FIFO RAM, 10-bit packet counts, 19-bit transfer sizes.
pub const HWCFG3_VALUE: u32 = (0x0800 << 16) | (6 << 4) | 8;
/// GAHBCFG as the model's ROM leaves it: DMA on (the driver must turn it off and record it).
pub const GAHBCFG_ROM: u32 = 0x27;
/// The board's bus in the model (the bench's bus 3).
pub const BUS: u32 = 3;
/// The boot ROM's device number before the first load.
pub const ROM_DEVNUM: u32 = 117;

/// Ways the rehearsal's board can differ from a good one.
#[derive(Clone, Debug, Default)]
pub struct Faults {
    /// The kernel reports this product string (a device with the stage's ids that is not the stage).
    pub product: Option<String>,
    /// The stage shows at this port instead of the board's.
    pub port: Option<String>,
    /// The core's reset never completes and its register read never returns (a bus hang): the stage
    /// never pulls up, and only the watchdog ends it.
    pub hang: bool,
    /// The wire flips one bit of the n-th bulk OUT packet (1-based), host to device.
    pub corrupt_out: Option<u32>,
    /// The wire flips one bit of the n-th bulk IN packet (1-based), device to host.
    pub corrupt_in: Option<u32>,
    /// The core is 4.20a or later (CSFTRST_DONE).
    pub rev_420a: bool,
    /// This user may not open the node (no udev rule grants it).
    pub access_denied: bool,
    /// The core's `time` CSR. The board's boot ROM leaves it at 0, not moving (B2 run 1); the default is that.
    pub time: TimeCsr,
    /// A driver fault for the model to catch: endpoint 0 OUT is never enabled again after the first
    /// time (the old driver's fault). The kernel cannot enumerate it.
    pub no_ep0_rearm: bool,
}

/// How the model's `time` CSR behaves.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TimeCsr {
    /// Reads 0 and does not move until the system timer's clock and its counter are enabled (the
    /// writes the vendor's SPL makes first, `STC_ENABLES`); then counts at 27 MHz. The board's ROM state.
    #[default]
    StoppedUntilEnabled,
    /// Reads 0 and never moves, enabled or not.
    Dead,
    /// Counts at 27 MHz from the start, enabled or not (a ROM that left it running).
    Running,
}

/// The model's `time` CSR: 27 MHz from the moment it starts.
struct Timebase {
    started: std::sync::OnceLock<Instant>,
}

impl Timebase {
    fn ticks(&self) -> u64 {
        self.started
            .get()
            .map_or(0, |s| s.elapsed().as_nanos() as u64 * 27 / 1000)
    }
}

/// What reset the board.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reset {
    /// The stage wrote the SoC reset.
    Soc,
    /// A watchdog ran out.
    Watchdog,
    /// The rehearsal stopped the board (not a reset the board makes).
    Stopped,
}

#[derive(Debug)]
struct Entry {
    status: u32,
    data: VecDeque<u32>,
}

#[derive(Default, Clone, Copy)]
struct EpState {
    ctl: u32,
    tsiz: u32,
    remaining: usize,
    pkts: u32,
    nak: bool,
}

/// The dwc2 core's state.
struct CoreState {
    regs: HashMap<usize, u32>,
    ins: [EpState; 2],
    outs: [EpState; 2],
    rxq: VecDeque<Entry>,
    cur: VecDeque<u32>,
    tx: [VecDeque<u32>; 2],
    reset_reads: u32,
    reset_done_bit: bool,
    resets_done: u32,
    gintsts: u32,
    active_addr: u8,
    out_packets: u32,
    in_packets: u32,
    ep0_out_enables: u32,
}

/// The core, shared by the stage's thread (registers) and the host's (the wire).
pub struct Core {
    state: Mutex<CoreState>,
    moved: Condvar,
    faults: Faults,
    wdt: Mutex<Watchdogs>,
    stopped: AtomicBool,
}

struct Watchdogs {
    dogs: [wdt::Model; 2],
    last: Instant,
}

fn le_words(bytes: &[u8]) -> VecDeque<u32> {
    bytes
        .chunks(4)
        .map(|c| {
            let mut w = [0u8; 4];
            w[..c.len()].copy_from_slice(c);
            u32::from_le_bytes(w)
        })
        .collect()
}

fn packet_bytes(words: &mut VecDeque<u32>, len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    for _ in 0..len.div_ceil(4) {
        out.extend_from_slice(&words.pop_front().unwrap_or(0).to_le_bytes());
    }
    out.truncate(len);
    out
}

impl Core {
    fn new(faults: Faults) -> Arc<Self> {
        let mut regs = HashMap::new();
        regs.insert(
            GSNPSID,
            if faults.rev_420a {
                GSNPSID_420A
            } else {
                GSNPSID_PRE_420A
            },
        );
        regs.insert(GHWCFG3, HWCFG3_VALUE);
        regs.insert(GAHBCFG, GAHBCFG_ROM);
        // The ROM's device is on the bus and in device mode.
        regs.insert(GUSBCFG, GUSBCFG_FORCEDEVMODE);
        regs.insert(GRXFSIZ, 0x200);
        Arc::new(Self {
            state: Mutex::new(CoreState {
                regs,
                ins: [EpState::default(); 2],
                outs: [EpState::default(); 2],
                rxq: VecDeque::new(),
                cur: VecDeque::new(),
                tx: [VecDeque::new(), VecDeque::new()],
                reset_reads: 0,
                reset_done_bit: false,
                resets_done: 0,
                gintsts: 0,
                active_addr: 0,
                out_packets: 0,
                in_packets: 0,
                ep0_out_enables: 0,
            }),
            moved: Condvar::new(),
            wdt: Mutex::new(Watchdogs {
                dogs: [
                    wdt::Model::new(wdt::BASE0, WDT_HZ, wdt::Fault::None).timed(),
                    wdt::Model::new(wdt::BASE1, WDT_HZ, wdt::Fault::None).timed(),
                ],
                last: Instant::now(),
            }),
            faults,
            stopped: AtomicBool::new(false),
        })
    }

    fn lock(&self) -> MutexGuard<'_, CoreState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The board's time passes: the watchdogs count. A watchdog that ran out resets the board, which
    /// ends the stage's thread here.
    fn tick(&self) {
        if self.stopped.load(Ordering::Relaxed) {
            resume_unwind(Box::new(Reset::Stopped));
        }
        let mut w = self.wdt.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        // Whole cycles of the 750 kHz clock, the part of a cycle carried to the next tick: a tick per read
        // in a tight loop must not run the counter fast or slow.
        let ns = now.duration_since(w.last).as_nanos();
        let cycles = (ns * u128::from(WDT_HZ) / 1_000_000_000) as u64;
        if cycles == 0 {
            return;
        }
        w.last +=
            Duration::from_nanos((u128::from(cycles) * 1_000_000_000 / u128::from(WDT_HZ)) as u64);
        let fired = w
            .dogs
            .iter_mut()
            .fold(false, |f, d| d.advance_cycles(cycles) | f);
        drop(w);
        if fired {
            resume_unwind(Box::new(Reset::Watchdog));
        }
    }

    fn wdt_access(&self, addr: usize, write: Option<u32>) -> u32 {
        self.tick();
        let mut w = self.wdt.lock().unwrap_or_else(|e| e.into_inner());
        let dog = if (wdt::BASE0..wdt::BASE0 + 0x800).contains(&addr) {
            &mut w.dogs[0]
        } else {
            &mut w.dogs[1]
        };
        match write {
            Some(v) => {
                dog.write32(addr, v);
                0
            }
            None => dog.read32(addr),
        }
    }

    // ---- the stage's side: registers ----

    fn read(&self, offset: usize) -> u32 {
        self.tick();
        if offset == GRSTCTL && self.faults.hang && self.lock().resets_done == 0 {
            // A bus that never answers: only a reset ends this.
            loop {
                self.tick();
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        let mut s = self.lock();
        match offset {
            GRSTCTL => {
                let mut v = GRSTCTL_AHBIDLE;
                if s.reset_reads > 0 {
                    s.reset_reads -= 1;
                    if s.reset_reads == 0 {
                        s.resets_done += 1;
                        if self.faults.rev_420a {
                            s.reset_done_bit = true;
                        }
                    }
                }
                let pending = s.reset_reads > 0 || (self.faults.rev_420a && s.reset_done_bit);
                if pending {
                    v |= GRSTCTL_CSFTRST;
                }
                if s.reset_done_bit {
                    v |= GRSTCTL_CSFTRST_DONE;
                }
                v
            }
            GINTSTS => {
                let mut v = s.gintsts;
                if !s.rxq.is_empty() {
                    v |= GINTSTS_RXFLVL;
                }
                if s.regs.get(&GUSBCFG).copied().unwrap_or(0) & GUSBCFG_FORCEDEVMODE == 0 {
                    v |= GINTSTS_CURMOD_HOST;
                }
                v
            }
            GRXSTSP => match s.rxq.pop_front() {
                Some(entry) => {
                    s.cur = entry.data;
                    drop(s);
                    self.moved.notify_all();
                    entry.status
                }
                None => 0,
            },
            DSTS => 0, // ENUMSPD 0: high speed
            o if o == epfifo(0) || o == epfifo(1) => s.cur.pop_front().unwrap_or(0),
            o if (0x900..0x940).contains(&o) || (0xb00..0xb40).contains(&o) => {
                let out = o >= 0xb00;
                let i = (o - if out { 0xb00 } else { 0x900 }) / 0x20;
                let ep = if out { &s.outs[i] } else { &s.ins[i] };
                match o & 0x1f {
                    0x00 => ep.ctl,
                    0x10 => ep.tsiz,
                    0x18 if !out => {
                        let size = if i == 0 {
                            s.regs.get(&GNPTXFSIZ).copied().unwrap_or(0) >> 16
                        } else {
                            s.regs.get(&DPTXFSIZN1).copied().unwrap_or(0) >> 16
                        };
                        size.saturating_sub(s.tx[i].len() as u32)
                    }
                    _ => 0,
                }
            }
            o => s.regs.get(&o).copied().unwrap_or(0),
        }
    }

    fn write(&self, offset: usize, value: u32) {
        self.tick();
        let mut s = self.lock();
        match offset {
            GRSTCTL => {
                if value & GRSTCTL_CSFTRST != 0 && s.reset_reads == 0 && !s.reset_done_bit {
                    // The soft reset: the queues, the FIFOs and the endpoints are cleared.
                    s.rxq.clear();
                    s.cur.clear();
                    s.tx = [VecDeque::new(), VecDeque::new()];
                    s.ins = [EpState::default(); 2];
                    s.outs = [EpState::default(); 2];
                    s.gintsts = 0;
                    s.reset_reads = 50;
                }
                if value & GRSTCTL_CSFTRST_DONE != 0 && s.reset_done_bit {
                    s.reset_done_bit = false;
                }
                if value & GRSTCTL_TXFFLSH != 0 {
                    let n = (value >> 6) & 0x1f;
                    for i in 0..2 {
                        if n == 0x10 || n as usize == i {
                            s.tx[i].clear();
                        }
                    }
                }
                if value & GRSTCTL_RXFFLSH != 0 {
                    s.rxq.clear();
                    s.cur.clear();
                }
            }
            GINTSTS => s.gintsts &= !value,
            o if o == epfifo(0) || o == epfifo(1) => {
                let i = if o == epfifo(0) { 0 } else { 1 };
                s.tx[i].push_back(value);
            }
            o if (0x900..0x940).contains(&o) || (0xb00..0xb40).contains(&o) => {
                let out = o >= 0xb00;
                let i = (o - if out { 0xb00 } else { 0x900 }) / 0x20;
                if out && i == 0 && o & 0x1f == 0 && value & DXEPCTL_EPENA != 0 {
                    s.ep0_out_enables += 1;
                    if self.faults.no_ep0_rearm && s.ep0_out_enables > 1 {
                        return;
                    }
                }
                let ep = if out { &mut s.outs[i] } else { &mut s.ins[i] };
                match o & 0x1f {
                    0x00 => {
                        let was = ep.ctl & DXEPCTL_EPENA != 0;
                        if value & DXEPCTL_SNAK != 0 {
                            ep.nak = true;
                        }
                        if value & DXEPCTL_CNAK != 0 {
                            ep.nak = false;
                        }
                        let mut ctl = value
                            & !(DXEPCTL_SNAK | DXEPCTL_CNAK | DXEPCTL_SETD0PID | DXEPCTL_EPDIS);
                        if value & DXEPCTL_EPDIS != 0 {
                            ctl &= !DXEPCTL_EPENA;
                        }
                        if !was && ctl & DXEPCTL_EPENA != 0 {
                            let (size, pkts) = if i == 0 {
                                (
                                    (ep.tsiz & 0x7f) as usize,
                                    if out {
                                        (ep.tsiz >> 19) & 1
                                    } else {
                                        (ep.tsiz >> 19) & 3
                                    },
                                )
                            } else {
                                ((ep.tsiz & 0x7ffff) as usize, (ep.tsiz >> 19) & 0x3ff)
                            };
                            ep.remaining = size;
                            ep.pkts = pkts;
                        }
                        ep.ctl = ctl;
                    }
                    0x10 => ep.tsiz = value,
                    _ => {}
                }
            }
            o => {
                s.regs.insert(o, value);
            }
        }
        drop(s);
        self.moved.notify_all();
    }

    // ---- the host's side: the wire ----

    /// The device is on the bus: its pull-up is on after a completed core reset.
    fn pulled_up(&self) -> bool {
        let s = self.lock();
        s.resets_done > 0
            && s.reset_reads == 0
            && s.regs.get(&DCTL).copied().unwrap_or(0) & DCTL_SFTDISCON == 0
    }

    /// A bus reset, then the end of the speed handshake.
    fn usb_reset(&self) {
        {
            let mut s = self.lock();
            s.gintsts |= GINTSTS_USBRST;
            s.active_addr = 0;
        }
        let until = Instant::now() + Duration::from_millis(50);
        let mut s = self.lock();
        while s.gintsts & GINTSTS_USBRST != 0 && Instant::now() < until {
            s = self
                .moved
                .wait_timeout(s, Duration::from_millis(5))
                .map(|(g, _)| g)
                .unwrap_or_else(|e| e.into_inner().0);
        }
        s.gintsts |= GINTSTS_ENUMDONE;
    }

    /// Waits, until `deadline`, for `ready` to hold on the core; `Err` when it never does.
    fn wait_for<T>(
        &self,
        deadline: Instant,
        mut ready: impl FnMut(&mut CoreState) -> Option<T>,
    ) -> Result<T, LinkError> {
        let mut s = self.lock();
        loop {
            if let Some(v) = ready(&mut s) {
                drop(s);
                self.moved.notify_all();
                return Ok(v);
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(LinkError::Timeout);
            }
            s = self
                .moved
                .wait_timeout(s, (deadline - now).min(Duration::from_millis(5)))
                .map(|(g, _)| g)
                .unwrap_or_else(|e| e.into_inner().0);
        }
    }

    fn setup(&self, addr: u8, setup: [u8; 8]) -> Result<(), LinkError> {
        let mut s = self.lock();
        if addr != s.active_addr || s.resets_done == 0 {
            return Err(LinkError::Timeout);
        }
        s.rxq.push_back(Entry {
            status: (PKTSTS_SETUPRX << 17) | (8 << 4),
            data: le_words(&setup),
        });
        s.rxq.push_back(Entry {
            status: PKTSTS_SETUPDONE << 17,
            data: VecDeque::new(),
        });
        // The core clears endpoint 0's stalls and ends its OUT transfer at a setup.
        s.ins[0].ctl &= !DXEPCTL_STALL;
        s.outs[0].ctl &= !(DXEPCTL_STALL | DXEPCTL_EPENA);
        drop(s);
        self.moved.notify_all();
        Ok(())
    }

    /// One IN packet of at most `mps` from endpoint `i`, or a stall.
    fn in_packet(&self, i: usize, mps: usize, deadline: Instant) -> Result<Vec<u8>, LinkError> {
        let corrupt = if i == 1 { self.faults.corrupt_in } else { None };
        self.wait_for(deadline, |s| {
            let ep = s.ins[i];
            if ep.ctl & DXEPCTL_STALL != 0 {
                return Some(Err(LinkError::Stall));
            }
            if ep.ctl & DXEPCTL_EPENA == 0 || ep.nak || ep.pkts == 0 {
                return None;
            }
            let len = ep.remaining.min(mps);
            if s.tx[i].len() < len.div_ceil(4) {
                return None;
            }
            let mut packet = packet_bytes(&mut s.tx[i], len);
            if i == 1 {
                s.in_packets += 1;
                if corrupt == Some(s.in_packets) && !packet.is_empty() {
                    packet[0] ^= 0x01;
                }
            }
            let ep = &mut s.ins[i];
            ep.remaining -= len;
            ep.pkts -= 1;
            if ep.pkts == 0 || (len < mps && ep.remaining == 0) {
                ep.ctl &= !DXEPCTL_EPENA;
            }
            Some(Ok(packet))
        })?
    }

    /// One OUT packet to endpoint `i`, or a stall.
    fn out_packet(
        &self,
        i: usize,
        mps: usize,
        data: &[u8],
        deadline: Instant,
    ) -> Result<(), LinkError> {
        let corrupt = if i == 1 {
            self.faults.corrupt_out
        } else {
            None
        };
        self.wait_for(deadline, |s| {
            let ep = s.outs[i];
            if ep.ctl & DXEPCTL_STALL != 0 {
                return Some(Err(LinkError::Stall));
            }
            if ep.ctl & DXEPCTL_EPENA == 0 || ep.nak || ep.pkts == 0 {
                return None;
            }
            if i == 1 && ep.ctl & DXEPCTL_USBACTEP == 0 {
                return None;
            }
            let held: usize = s.rxq.iter().map(|e| e.data.len() + 1).sum::<usize>() + s.cur.len();
            let room = s.regs.get(&GRXFSIZ).copied().unwrap_or(0) as usize;
            if held + data.len().div_ceil(4) + 2 > room {
                return None;
            }
            let mut bytes = data.to_vec();
            if i == 1 {
                s.out_packets += 1;
                if corrupt == Some(s.out_packets) && !bytes.is_empty() {
                    let mid = bytes.len() / 2;
                    bytes[mid] ^= 0x04;
                }
            }
            s.rxq.push_back(Entry {
                status: (i as u32) | ((data.len() as u32) << 4) | (PKTSTS_OUTRX << 17),
                data: le_words(&bytes),
            });
            let ep = &mut s.outs[i];
            ep.remaining = ep.remaining.saturating_sub(data.len());
            ep.pkts -= 1;
            if data.len() < mps || ep.pkts == 0 {
                ep.ctl &= !DXEPCTL_EPENA;
                s.rxq.push_back(Entry {
                    status: (i as u32) | (PKTSTS_OUTDONE << 17),
                    data: VecDeque::new(),
                });
            }
            Some(Ok(()))
        })?
    }

    fn latch_address(&self) {
        let mut s = self.lock();
        s.active_addr = ((s.regs.get(&DCFG).copied().unwrap_or(0) >> 4) & 0x7f) as u8;
    }

    fn control_in(
        &self,
        addr: u8,
        setup: [u8; 8],
        timeout: Duration,
    ) -> Result<Vec<u8>, LinkError> {
        let deadline = Instant::now() + timeout;
        self.setup(addr, setup)?;
        let want = usize::from(u16::from_le_bytes([setup[6], setup[7]]));
        let mut data = Vec::new();
        while data.len() < want {
            let packet = self.in_packet(0, 64, deadline)?;
            let short = packet.len() < 64;
            data.extend_from_slice(&packet);
            if short {
                break;
            }
        }
        // The status stage: a zero-length OUT.
        self.out_packet(0, 64, &[], deadline)?;
        self.latch_address();
        Ok(data)
    }

    fn control_none(&self, addr: u8, setup: [u8; 8], timeout: Duration) -> Result<(), LinkError> {
        let deadline = Instant::now() + timeout;
        self.setup(addr, setup)?;
        // The status stage: a zero-length IN.
        self.in_packet(0, 64, deadline)?;
        // The core answers at the new address only after the status stage.
        self.latch_address();
        Ok(())
    }

    fn bulk_out(&self, data: &[u8], timeout: Duration) -> Result<(), LinkError> {
        let deadline = Instant::now() + timeout;
        for packet in data.chunks(512) {
            self.out_packet(1, 512, packet, deadline)?;
        }
        if data.is_empty() {
            self.out_packet(1, 512, &[], deadline)?;
        }
        Ok(())
    }

    fn bulk_in(&self, max: usize, timeout: Duration) -> Result<Vec<u8>, LinkError> {
        let deadline = Instant::now() + timeout;
        let mut data = Vec::new();
        loop {
            let packet = self.in_packet(1, 512, deadline)?;
            let short = packet.len() < 512;
            data.extend_from_slice(&packet);
            if short || data.len() >= max {
                return Ok(data);
            }
        }
    }
}

/// The controller's registers as the driver sees them.
pub struct ModelRegs(Arc<Core>);

impl Dwc2Regs for ModelRegs {
    fn read(&self, offset: usize) -> u32 {
        self.0.read(offset)
    }
    fn write(&self, offset: usize, value: u32) {
        self.0.write(offset, value)
    }
}

/// The stage's other buses: the watchdogs, the console, the SoC reset.
struct Bus {
    core: Arc<Core>,
    shared: Arc<Mutex<Shared>>,
    started: Instant,
    line: Vec<u8>,
    lcr: u32,
    timebase: &'static Timebase,
    time: TimeCsr,
    /// The system timer writes seen, in order.
    stc: Vec<(usize, u32)>,
}

impl RegAccess for Bus {
    fn read32(&mut self, addr: usize) -> u32 {
        if (wdt::BASE0..wdt::BASE1 + 0x800).contains(&addr) {
            return self.core.wdt_access(addr, None);
        }
        self.core.tick();
        match addr {
            a if a == lemnos_k230_usbboot_stage::uart::BASE + 0x14 => 0x60,
            a if a == lemnos_k230_usbboot_stage::uart::BASE + 0x0c => self.lcr,
            a if (lemnos_k230_usbboot_stage::uart::BASE
                ..lemnos_k230_usbboot_stage::uart::BASE + 0x100)
                .contains(&a) =>
            {
                0
            }
            other => {
                self.note(format!("read of {other:#x}, outside the stage's windows"));
                0
            }
        }
    }

    fn write32(&mut self, addr: usize, value: u32) {
        if (wdt::BASE0..wdt::BASE1 + 0x800).contains(&addr) {
            self.core.wdt_access(addr, Some(value));
            return;
        }
        self.core.tick();
        let uart = lemnos_k230_usbboot_stage::uart::BASE;
        match addr {
            a if a == SOC_GLB_RST && value == SOC_RESET_VALUE => {
                resume_unwind(Box::new(Reset::Soc))
            }
            a if STC_ENABLES.iter().any(|&(at, _)| at == a) => {
                self.stc.push((a, value));
                // The hart's timer clock and the counter itself: both are needed for `time` to move.
                if self.time == TimeCsr::StoppedUntilEnabled
                    && self.stc.contains(&STC_ENABLES[0])
                    && self.stc.contains(&STC_ENABLES[2])
                {
                    let _ = self.timebase.started.set(Instant::now());
                }
            }
            a if a == uart => {
                if self.lcr & 0x80 != 0 {
                    return;
                }
                match value as u8 {
                    b'\n' => {
                        let text = String::from_utf8_lossy(&self.line).trim_end().to_string();
                        self.line.clear();
                        let at = self.started.elapsed().as_secs_f64();
                        let mut shared = self.shared.lock().unwrap_or_else(|e| e.into_inner());
                        shared.console.push((at, text));
                    }
                    byte => self.line.push(byte),
                }
            }
            a if a == uart + 0x0c => self.lcr = value,
            a if (uart..uart + 0x100).contains(&a) => {}
            other => self.note(format!(
                "write of {value:#x} to {other:#x}, outside the stage's windows"
            )),
        }
    }
}

impl Bus {
    fn note(&self, text: String) {
        let mut shared = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        shared.unexpected.push(text);
    }
}

/// The stage's clock, as the image has it: the `time` CSR over 27 (the image's `Micros`).
#[derive(Clone, Copy)]
struct Micros(&'static Timebase);

impl Clock for Micros {
    fn now_us(&mut self) -> u64 {
        self.0.ticks() / (TIMEBASE_HZ / 1_000_000)
    }
}

/// What the board shows and has done.
#[derive(Default)]
struct Shared {
    seen: Vec<Seen>,
    next_devnum: u32,
    console: Vec<(f64, String)>,
    kernel: Vec<(f64, String)>,
    resets: Vec<(f64, Reset)>,
    unexpected: Vec<String>,
    pad_writes_refused: u32,
    core: Option<Arc<Core>>,
}

/// The rehearsal's board.
pub struct SimBoard {
    shared: Arc<Mutex<Shared>>,
    faults: Faults,
    started: Instant,
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl SimBoard {
    /// A board at the boot ROM, at the board's port.
    pub fn new(faults: Faults) -> Self {
        let shared = Shared {
            next_devnum: ROM_DEVNUM + 1,
            seen: vec![rom_seen(ROM_DEVNUM)],
            ..Default::default()
        };
        Self {
            shared: Arc::new(Mutex::new(shared)),
            faults,
            started: Instant::now(),
            threads: Vec::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The boot ROM's device now, if it shows.
    pub fn rom(&self) -> Option<Seen> {
        self.lock()
            .seen
            .iter()
            .find(|s| s.vid == DOWNLOAD_VID && s.pid == DOWNLOAD_PID)
            .cloned()
    }

    /// The load and the start: the boot ROM leaves the bus and the check build runs from its entry (both
    /// watchdogs armed first, as the image's entry does). Returns when the start went out.
    pub fn load_and_start(&mut self) -> Instant {
        let core = Core::new(self.faults.clone());
        let at = Instant::now();
        {
            let mut shared = self.lock();
            shared
                .seen
                .retain(|s| !(s.vid == DOWNLOAD_VID && s.pid == DOWNLOAD_PID));
            shared.core = Some(core.clone());
        }
        let shared = self.shared.clone();
        let stage_core = core.clone();
        let started = self.started;
        let done = Arc::new(AtomicBool::new(false));
        let stage_done = done.clone();
        let time = self.faults.time;
        self.threads.push(std::thread::spawn(move || {
            let reset = run_stage(stage_core, shared.clone(), at, time);
            let when = started.elapsed().as_secs_f64();
            let mut s = shared.lock().unwrap_or_else(|e| e.into_inner());
            s.resets.push((when, reset));
            stage_done.store(true, Ordering::SeqCst);
        }));
        let shared = self.shared.clone();
        let faults = self.faults.clone();
        self.threads.push(std::thread::spawn(move || {
            kernel(core, shared, faults, done, started);
        }));
        at
    }

    /// The console's lines so far, with their time from the board's start.
    pub fn console(&self) -> Vec<(f64, String)> {
        self.lock().console.clone()
    }

    /// What the kernel logged.
    pub fn kernel_log(&self) -> Vec<(f64, String)> {
        self.lock().kernel.clone()
    }

    /// The resets so far.
    pub fn resets(&self) -> Vec<(f64, Reset)> {
        self.lock().resets.clone()
    }

    /// Accesses the stage made outside its windows (none is expected).
    pub fn unexpected(&self) -> Vec<String> {
        self.lock().unexpected.clone()
    }

    /// Pad writes the stage's guard refused (none is expected).
    pub fn pad_writes_refused(&self) -> u32 {
        self.lock().pad_writes_refused
    }

    /// Ends every thread: the stage (if it still runs) is stopped, not reset.
    pub fn stop(mut self) {
        if let Some(core) = self.lock().core.clone() {
            core.stopped.store(true, Ordering::SeqCst);
        }
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

fn rom_seen(devnum: u32) -> Seen {
    Seen {
        port: PORT.to_string(),
        vid: DOWNLOAD_VID,
        pid: DOWNLOAD_PID,
        product: None,
        interface: None,
        speed: Some("480".to_string()),
        configuration: Some(1),
        busnum: BUS,
        devnum,
    }
}

/// The check build from its entry, to its reset.
fn run_stage(core: Arc<Core>, shared: Arc<Mutex<Shared>>, at: Instant, time: TimeCsr) -> Reset {
    // One small leak per simulated boot: the stage's clock is `Copy`, as the image's is.
    let timebase: &'static Timebase = Box::leak(Box::new(Timebase {
        started: std::sync::OnceLock::new(),
    }));
    if time == TimeCsr::Running {
        let _ = timebase.started.set(at);
    }
    let bus = Bus {
        core: core.clone(),
        shared: shared.clone(),
        started: at,
        line: Vec::new(),
        lcr: 0x03,
        timebase,
        time,
        stc: Vec::new(),
    };
    let refused = Arc::new(Mutex::new(0u32));
    let refused_out = refused.clone();
    let outcome = catch_unwind(AssertUnwindSafe(move || {
        let mut regs = PadGuard::new(bus);
        // The image's entry: both watchdogs armed and read back before any Rust runs.
        if wdt::arm(&mut regs).is_err() {
            resume_unwind(Box::new(Reset::Soc));
        }
        let ticks = move || timebase.ticks();
        let mut known = Known::new(ticks());
        let stats = Stats::new();
        let (mut buf, mut scratch) = (vec![0u8; BUFFER_LEN], vec![0u8; BUFFER_LEN]);
        let (mut rx, mut window) = (vec![0u8; RX_LEN], vec![0u8; WINDOW_LEN]);
        let mut clock = Micros(timebase);
        let cause = stage_b2::check(
            &mut regs,
            ModelRegs(core.clone()),
            &mut clock,
            &ticks,
            Buffers {
                buf: &mut buf,
                scratch: &mut scratch,
                rx: &mut rx,
                window: &mut window,
            },
            &stats,
            &mut known,
        );
        *refused_out.lock().unwrap_or_else(|e| e.into_inner()) = regs.refused();
        stage_b2::end(&mut regs, &stats, &known, ticks(), cause)
    }));
    let reset = match outcome {
        Ok(never) => never,
        Err(payload) => payload.downcast_ref::<Reset>().copied().unwrap_or_else(|| {
            let mut s = shared.lock().unwrap_or_else(|e| e.into_inner());
            let text = payload
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_default();
            s.unexpected.push(format!("the stage panicked: {text}"));
            Reset::Stopped
        }),
    };
    let mut s = shared.lock().unwrap_or_else(|e| e.into_inner());
    s.pad_writes_refused = *refused.lock().unwrap_or_else(|e| e.into_inner());
    // The stage's device leaves the bus with the reset.
    s.seen
        .retain(|seen| !(seen.vid == lemnos_flash_wire::usb::VENDOR_ID));
    s.core = None;
    reset
}

/// The kernel's side: sees the pull-up, resets the bus, enumerates, publishes the device; after the
/// board's reset, the boot ROM shows again under a new number.
fn kernel(
    core: Arc<Core>,
    shared: Arc<Mutex<Shared>>,
    faults: Faults,
    done: Arc<AtomicBool>,
    started: Instant,
) {
    let log = |text: String| {
        let mut s = shared.lock().unwrap_or_else(|e| e.into_inner());
        s.kernel.push((started.elapsed().as_secs_f64(), text));
    };
    let mut enumerated = false;
    let mut tries = 0;
    while !done.load(Ordering::SeqCst) {
        if !enumerated && tries < 3 && core.pulled_up() {
            std::thread::sleep(Duration::from_millis(100));
            tries += 1;
            let devnum = {
                let mut s = shared.lock().unwrap_or_else(|e| e.into_inner());
                let n = s.next_devnum;
                s.next_devnum += 1;
                n
            };
            log(format!(
                "new high-speed USB device number {devnum} (try {tries})"
            ));
            match enumerate(&core, devnum as u8) {
                Ok((product, interface)) => {
                    let seen = Seen {
                        port: faults.port.clone().unwrap_or_else(|| PORT.to_string()),
                        vid: lemnos_flash_wire::usb::VENDOR_ID,
                        pid: lemnos_flash_wire::usb::PRODUCT_ID,
                        product: faults.product.clone().or(Some(product)),
                        interface: Some(interface),
                        speed: Some("480".to_string()),
                        configuration: Some(1),
                        busnum: BUS,
                        devnum,
                    };
                    log(format!(
                        "New USB device found, idVendor=1209, idProduct=0001; product {:?}",
                        seen.product
                    ));
                    shared
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .seen
                        .push(seen);
                    enumerated = true;
                }
                Err(text) => log(format!(
                    "device descriptor read or set address failed: {text}"
                )),
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    log("USB disconnect".to_string());
    // The boot ROM after the reset.
    std::thread::sleep(Duration::from_millis(300));
    let mut s = shared.lock().unwrap_or_else(|e| e.into_inner());
    let n = s.next_devnum;
    s.next_devnum += 1;
    s.seen.push(rom_seen(n));
    s.kernel.push((
        started.elapsed().as_secs_f64(),
        format!("new high-speed USB device number {n}: 29f1:0230"),
    ));
}

fn setup_packet(request_type: u8, request: u8, value: u16, index: u16, len: u16) -> [u8; 8] {
    let mut s = [0u8; 8];
    s[0] = request_type;
    s[1] = request;
    s[2..4].copy_from_slice(&value.to_le_bytes());
    s[4..6].copy_from_slice(&index.to_le_bytes());
    s[6..8].copy_from_slice(&len.to_le_bytes());
    s
}

fn utf16(descriptor: &[u8]) -> String {
    let units: Vec<u16> = descriptor
        .get(2..)
        .unwrap_or(&[])
        .chunks(2)
        .map(|c| u16::from_le_bytes([c[0], *c.get(1).unwrap_or(&0)]))
        .collect();
    String::from_utf16_lossy(&units)
}

/// Linux's enumeration of a high-speed device (`hub_port_init`, `usb_new_device`).
fn enumerate(core: &Core, devnum: u8) -> Result<(String, String), String> {
    let t = Duration::from_secs(1);
    fn e(what: &'static str) -> impl Fn(LinkError) -> String {
        move |err| format!("{what}: {err}")
    }
    core.usb_reset();
    let first = core
        .control_in(0, setup_packet(0x80, 6, 0x0100, 0, 64), t)
        .map_err(e("device descriptor (64)"))?;
    if first.len() < 8 {
        return Err(format!("device descriptor (64): {} bytes", first.len()));
    }
    core.usb_reset();
    core.control_none(0, setup_packet(0x00, 5, u16::from(devnum), 0, 0), t)
        .map_err(e("SET_ADDRESS"))?;
    std::thread::sleep(Duration::from_millis(2));
    let device = core
        .control_in(devnum, setup_packet(0x80, 6, 0x0100, 0, 18), t)
        .map_err(e("device descriptor (18)"))?;
    let head = core
        .control_in(devnum, setup_packet(0x80, 6, 0x0200, 0, 9), t)
        .map_err(e("configuration (9)"))?;
    let total = u16::from_le_bytes([head[2], head[3]]);
    core.control_in(devnum, setup_packet(0x80, 6, 0x0200, 0, total), t)
        .map_err(e("configuration (all)"))?;
    core.control_in(devnum, setup_packet(0x80, 6, 0x0300, 0, 255), t)
        .map_err(e("string 0"))?;
    let product = core
        .control_in(
            devnum,
            setup_packet(0x80, 6, 0x0300 | u16::from(device[15]), LANGUAGE, 255),
            t,
        )
        .map_err(e("product string"))?;
    core.control_none(devnum, setup_packet(0x00, 9, 1, 0, 0), t)
        .map_err(e("SET_CONFIGURATION"))?;
    let interface = core
        .control_in(devnum, setup_packet(0x80, 6, 0x0303, LANGUAGE, 255), t)
        .map_err(e("interface string"))?;
    Ok((utf16(&product), utf16(&interface)))
}

/// The opened stage in the model.
struct SimLink {
    core: Arc<Core>,
    addr: u8,
}

impl Link for SimLink {
    fn control_in(
        &mut self,
        request_type: u8,
        request: u8,
        value: u16,
        index: u16,
        len: u16,
        timeout: Duration,
    ) -> Result<Vec<u8>, LinkError> {
        self.core.control_in(
            self.addr,
            setup_packet(request_type, request, value, index, len),
            timeout,
        )
    }

    fn control_none(
        &mut self,
        request_type: u8,
        request: u8,
        value: u16,
        index: u16,
        timeout: Duration,
    ) -> Result<(), LinkError> {
        self.core.control_none(
            self.addr,
            setup_packet(request_type, request, value, index, 0),
            timeout,
        )
    }

    fn bulk_out(&mut self, data: &[u8], timeout: Duration) -> Result<(), LinkError> {
        self.core.bulk_out(data, timeout)
    }

    fn bulk_in(&mut self, max: usize, timeout: Duration) -> Result<Vec<u8>, LinkError> {
        self.core.bulk_in(max, timeout)
    }
}

impl Board for SimBoard {
    fn scan(&mut self) -> Vec<Seen> {
        self.lock().seen.clone()
    }

    fn access(&mut self, seen: &Seen) -> Access {
        if self.faults.access_denied {
            Access::Denied(format!(
                "access(R|W): Permission denied (os error 13); {} mode 664 owner 0:0, no ACL (the model's stand-in)",
                seen.devnode()
            ))
        } else {
            Access::Granted
        }
    }

    fn open(&mut self, seen: &Seen) -> Result<Box<dyn Link>, String> {
        let s = self.lock();
        if !s.seen.contains(seen) {
            return Err("the device is not on the bus".to_string());
        }
        let core = s.core.clone().ok_or("the stage is not running")?;
        Ok(Box::new(SimLink {
            core,
            addr: seen.devnum as u8,
        }))
    }
}
