//! Bench tooling for the Rockchip maskrom flasher: the MMIO primitives and the 471 watchdog stub.
//! This is the only unsafe code of that flasher. It holds no logic: the register sequences live in
//! `drivers/rockchip-maskrom-stage`, which forbids unsafe code and reaches the hardware
//! through these functions. Never linked into lemnosd.

#![no_std]

/// Reads the 32-bit register at `addr`.
///
/// # Safety
///
/// `addr` must be the address of a mapped, 4-byte-aligned register that may be read, and whose read
/// has no effect the caller does not expect.
#[inline(always)]
pub unsafe fn read32(addr: usize) -> u32 {
    // SAFETY: the caller guarantees `addr` is a mapped, aligned, readable register (see # Safety).
    unsafe { core::ptr::read_volatile(addr as *const u32) }
}

/// Writes `value` to the 32-bit register at `addr`.
///
/// # Safety
///
/// `addr` must be the address of a mapped, 4-byte-aligned register that may be written, and whose
/// write is the one the caller intends.
#[inline(always)]
pub unsafe fn write32(addr: usize, value: u32) {
    // SAFETY: the caller guarantees `addr` is a mapped, aligned, writable register (see # Safety).
    unsafe { core::ptr::write_volatile(addr as *mut u32, value) }
}

// The 471 stub, its arm-and-spin and arm-and-return images, and the 472 prologue: position-independent
// (see stage471.S). Assembled into the crate for the board's target only.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
core::arch::global_asm!(include_str!("stage471.S"));

/// The board's registers, through the volatile primitives above. The stage crate's register
/// sequences run on this; the host tests run them on a fake.
pub struct Mmio;

impl lemnos_rockchip_maskrom_stage::bind::RegAccess for Mmio {
    fn read32(&mut self, addr: usize) -> u32 {
        // SAFETY: the stage crate passes only the controller registers of bind.rs (the watchdog,
        // DWC3 and sdmmc0), which the board maps; see the addresses cited there.
        unsafe { read32(addr) }
    }

    fn write32(&mut self, addr: usize, value: u32) {
        // SAFETY: as read32: a register of bind.rs, mapped on the board.
        unsafe { write32(addr, value) }
    }
}

/// The board's DMA memory and the 472 loader's entry. Everything here is for the board's target only.
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
mod board {
    use core::arch::asm;

    use lemnos_flash_wire::frame::{MAX_SECTORS, SECTOR};
    use lemnos_rockchip_maskrom_stage::dwc3link::DmaMem;
    use lemnos_rockchip_maskrom_stage::io::Trb;

    /// One frame: a full write chunk of 128 sectors with its CRC.
    pub const FRAME_BYTES: usize = MAX_SECTORS as usize * SECTOR + 2;
    /// The cache line of the Cortex-A55 (CTR_EL0 DminLine: 64 bytes).
    const LINE: usize = 64;

    /// A buffer aligned to a cache line, so cache maintenance on it never touches a neighbour.
    #[repr(C, align(64))]
    struct Aligned<T>(T);

    /// The receive buffer: the controller writes it.
    static mut RX: Aligned<[u8; FRAME_BYTES]> = Aligned([0; FRAME_BYTES]);
    /// The transmit buffer: the CPU writes it, the controller reads it.
    static mut TX: Aligned<[u8; FRAME_BYTES]> = Aligned([0; FRAME_BYTES]);
    /// Two TRBs of four words each (bpl, bph, size, ctrl), the OUT TRB first.
    static mut TRBS: Aligned<[u32; 8]> = Aligned([0; 8]);
    /// The serve loop's own frame buffers (not DMA memory).
    static mut SERVE_BUF: Aligned<[u8; FRAME_BYTES]> = Aligned([0; FRAME_BYTES]);
    static mut SERVE_SCRATCH: Aligned<[u8; FRAME_BYTES]> = Aligned([0; FRAME_BYTES]);

    /// Clean-to-point-of-coherency for the line at `addr`. Writes the line back; no register changes.
    ///
    /// # Safety
    ///
    /// `addr` must be inside mapped memory.
    #[inline(always)]
    unsafe fn dc_cvac(addr: usize) {
        // SAFETY: the caller guarantees `addr` is mapped; DC CVAC only writes the line back.
        unsafe { asm!("dc cvac, {0}", in(reg) addr, options(nostack, preserves_flags)) }
    }

    /// Clean and invalidate to the point of coherency for the line at `addr`.
    ///
    /// # Safety
    ///
    /// `addr` must be inside mapped memory.
    #[inline(always)]
    unsafe fn dc_civac(addr: usize) {
        // SAFETY: the caller guarantees `addr` is mapped; the line's contents are the controller's.
        unsafe { asm!("dc civac, {0}", in(reg) addr, options(nostack, preserves_flags)) }
    }

    /// Waits until the cache maintenance above has completed.
    #[inline(always)]
    fn dsb() {
        // SAFETY: a barrier; it changes no state.
        unsafe { asm!("dsb sy", options(nostack, preserves_flags)) }
    }

    /// Applies `op` to every cache line that overlaps `start..start + len`.
    fn for_lines(start: usize, len: usize, mut op: impl FnMut(usize)) {
        let mut line = start & !(LINE - 1);
        let end = start + len;
        while line < end {
            op(line);
            line += LINE;
        }
    }

    /// The address of word `index` of TRB `slot`.
    fn trb_word(slot: usize, index: usize) -> *mut u32 {
        assert!(slot < 2 && index < 4, "TRB slot or word out of range");
        (&raw mut TRBS).cast::<u32>().wrapping_add(slot * 4 + index)
    }

    /// The board's DMA memory. There is one: the loader takes it once, at its entry.
    pub struct Dma;

    impl DmaMem for Dma {
        fn rx_addr(&self) -> u64 {
            (&raw const RX) as usize as u64
        }

        fn tx_addr(&self) -> u64 {
            (&raw const TX) as usize as u64
        }

        fn trb_addr(&self, slot: usize) -> u64 {
            ((&raw const TRBS) as usize + slot * 16) as u64
        }

        fn write_trb(&mut self, slot: usize, trb: Trb) {
            for (index, word) in [trb.bpl, trb.bph, trb.size, trb.ctrl]
                .into_iter()
                .enumerate()
            {
                let at = trb_word(slot, index);
                // SAFETY: `trb_word` asserts the slot and word are inside TRBS, which only this
                // loader writes; the controller reads it after the transfer is started.
                unsafe { core::ptr::write_volatile(at, word) };
            }
        }

        fn read_trb(&mut self, slot: usize) -> Trb {
            let trb_bytes = (&raw const TRBS) as usize;
            for_lines(trb_bytes + slot * 16, 16, |line| {
                // SAFETY: `line` is a line of TRBS, which is mapped; the invalidate drops the CPU's
                // copy so the next read sees the controller's update.
                unsafe { dc_civac(line) }
            });
            dsb();
            let word = |index: usize| {
                let at = trb_word(slot, index);
                // SAFETY: `trb_word` asserts the slot and word are inside TRBS.
                unsafe { core::ptr::read_volatile(at) }
            };
            Trb {
                bpl: word(0),
                bph: word(1),
                size: word(2),
                ctrl: word(3),
            }
        }

        fn rx(&mut self) -> &mut [u8] {
            // SAFETY: RX is the loader's own buffer, and the link holds one borrow of it at a time.
            unsafe { core::slice::from_raw_parts_mut((&raw mut RX).cast::<u8>(), FRAME_BYTES) }
        }

        fn tx(&mut self) -> &mut [u8] {
            // SAFETY: TX is the loader's own buffer, and the link holds one borrow of it at a time.
            unsafe { core::slice::from_raw_parts_mut((&raw mut TX).cast::<u8>(), FRAME_BYTES) }
        }

        fn clean_for_device(&mut self) {
            let trbs = (&raw const TRBS) as usize;
            for_lines(trbs, 32, |line| {
                // SAFETY: a line of TRBS, which is mapped.
                unsafe { dc_cvac(line) }
            });
            let tx = (&raw const TX) as usize;
            for_lines(tx, FRAME_BYTES, |line| {
                // SAFETY: a line of TX, which is mapped.
                unsafe { dc_cvac(line) }
            });
            dsb();
        }

        fn invalidate_rx(&mut self) {
            let rx = (&raw const RX) as usize;
            for_lines(rx, FRAME_BYTES, |line| {
                // SAFETY: a line of RX, which is mapped.
                unsafe { dc_civac(line) }
            });
            dsb();
        }
    }

    /// The serve loop's buffers. Called once, at the entry.
    pub fn serve_buffers() -> (&'static mut [u8], &'static mut [u8]) {
        // SAFETY: each buffer is handed out once, by this single call, and nothing else uses it.
        let buf = unsafe {
            core::slice::from_raw_parts_mut((&raw mut SERVE_BUF).cast::<u8>(), FRAME_BYTES)
        };
        // SAFETY: as above, for the scratch buffer.
        let scratch = unsafe {
            core::slice::from_raw_parts_mut((&raw mut SERVE_SCRATCH).cast::<u8>(), FRAME_BYTES)
        };
        (buf, scratch)
    }

    /// The 472 loader's Rust entry. The prologue in stage471.S arms the watchdog and branches here.
    #[unsafe(no_mangle)]
    pub extern "C" fn lemnos_loader472_main() -> ! {
        let mut regs = super::Mmio;
        let mut clock = lemnos_flash_stage::wait::IterClock::new();
        let (buf, scratch) = serve_buffers();
        lemnos_rockchip_maskrom_stage::entry472::run(&mut regs, &mut clock, Dma, buf, scratch)
    }

    /// The panic handler of the 472 image: it parks without feeding the watchdog.
    #[panic_handler]
    fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
        lemnos_rockchip_maskrom_stage::entry472::park()
    }
}
