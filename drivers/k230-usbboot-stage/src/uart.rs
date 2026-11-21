//! The first UART, the console the stage prints its banner on: `serial0 = serial@91400000`
//! (`k230.dtsi:84-92` at kendryte/k230_sdk 7e302f7: `snps,dw-apb-uart`, `clock-frequency = <50000000>`,
//! `reg-shift = <2>`, so each register is 4 bytes apart). U-Boot's own copy of the node is at
//! `src/little/uboot/arch/riscv/dts/k230.dtsi:99-101`, and `platform.h:58` (`UART0_BASE_ADDR`).
//!
//! The state the ROM leaves this UART in is UNVERIFIED. The stage's rule, in order:
//! - it reads the line control register: if the line is already 8N1 (`LCR == 0x03`), the stage writes nothing to
//!   the UART block and prints on it as the ROM left it;
//! - otherwise it programs the divisor and the line format on the UART block itself (`LCR` with DLAB, `DLL`,
//!   `DLM`, then `LCR` 8N1). The divisor for 115200 at 50 MHz is `50e6 / (16 * 115200)` = 27 (`DLL = 27`, `DLM = 0`).
//! - it never writes a pad. The UART's pins are muxed, if they are, by the ROM; a stage that finds them unmuxed
//!   prints nothing and reports that (the banner write returns an error after its bounded wait).
//!
//! Registers (16550-compatible, `dw-apb-uart`), offset = index * 4: `THR`/`DLL` 0, `DLM` 4, `FCR` 8, `LCR` 12,
//! The stage writes the UART with TFNF (USR bit 1) and clears PTIME (IER bit 7); see the constants below.

use crate::wdt::RegAccess;

/// The UART's base (`serial0`).
pub const BASE: usize = 0x9140_0000;
/// The baud divisor for 115200 at 50 MHz, rounded (`50e6 / (16 * 115200)`).
pub const DIVISOR_115200: u32 = 27;
const THR: usize = 0x00;
const DLL: usize = 0x00;
const DLM: usize = 0x04;
const LCR: usize = 0x0c;
const IER: usize = 0x04;
const LSR: usize = 0x14;
/// Line control: 8 data bits, no parity, one stop bit (8N1), and DLAB (bit 7) to reach the divisor.
const LCR_8N1: u32 = 0x03;
const LCR_DLAB: u32 = 0x80;
/// Line status: transmitter empty (TEMT, bit 6). A write waits on this, the way the vendor's own UART driver does
/// (`src/big/rt-smart/kernel/bsp/maix3/board/interdrv/uart_canaan/drv_uart.c`, the TEMT wait, line 373). THRE is not
/// used: with PTIME set it means "FIFO full", and the USR status bits are not implemented on this instance (R1d read
/// USR as 0x01, BUSY only), so they cannot say the FIFO has room.
const LSR_TEMT: u32 = 0x40;
/// Interrupt enable: bit 7 is PTIME, the programmable-THRE mode of a DesignWare UART, which the ROM leaves set (the
/// vendor driver writes IER `0x81` at init: lines 101 and 277 of the same file). It is cleared here.
const IER_PTIME: u32 = 0x80;
/// The bounded wait for TEMT per byte, in reads.
const SPIN: u32 = 100_000;

/// Whether the stage programmed the UART itself (true) or found it as the ROM left it (false).
pub fn configure_if_unset<R: RegAccess>(regs: &mut R) -> bool {
    if regs.read32(BASE + LCR) & 0xff == LCR_8N1 {
        return false;
    }
    regs.write32(BASE + LCR, LCR_DLAB | LCR_8N1);
    regs.write32(BASE + DLL, DIVISOR_115200 & 0xff);
    regs.write32(BASE + DLM, DIVISOR_115200 >> 8);
    regs.write32(BASE + LCR, LCR_8N1);
    true
}

/// Clears PTIME, so LSR.THRE means "holding register empty" again. Done only while DLAB (LCR bit 7) is clear: with
/// DLAB set, offset 0x04 is the divisor's high byte, not IER. Returns whether IER was written. It reads the bit and
/// clears it, whatever the ROM left.
pub fn clear_programmable_thre<R: RegAccess>(regs: &mut R) -> bool {
    if regs.read32(BASE + LCR) & LCR_DLAB != 0 {
        return false;
    }
    let ier = regs.read32(BASE + IER);
    regs.write32(BASE + IER, ier & !IER_PTIME);
    true
}

/// The transmitter did not empty within its bound: the UART is unusable (the ROM left it unclocked or unconfigured).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stuck;

/// Writes one byte, after the transmitter has emptied (LSR.TEMT, as the vendor driver waits). An error when it never
/// empties (a UART the ROM left unusable, or unclocked): the caller reports it and prints nothing more.
pub fn write_byte<R: RegAccess>(regs: &mut R, byte: u8) -> Result<(), Stuck> {
    for _ in 0..SPIN {
        if regs.read32(BASE + LSR) & LSR_TEMT != 0 {
            regs.write32(BASE + THR, u32::from(byte));
            return Ok(());
        }
    }
    Err(Stuck)
}

/// Writes several strings as one line, then a newline. Stops at the first byte that cannot be sent.
pub fn write_line_parts<R: RegAccess>(regs: &mut R, parts: &[&str]) -> Result<(), Stuck> {
    for part in parts {
        for &byte in part.as_bytes() {
            write_byte(regs, byte)?;
        }
    }
    write_byte(regs, b'\r')?;
    write_byte(regs, b'\n')
}

/// Writes a line and a newline. Stops at the first byte that cannot be sent.
pub fn write_line<R: RegAccess>(regs: &mut R, line: &str) -> Result<(), Stuck> {
    for &byte in line.as_bytes() {
        write_byte(regs, byte)?;
    }
    write_byte(regs, b'\r')?;
    write_byte(regs, b'\n')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A UART model with the registers the stage reads. The device's USR status bits read 0 (not implemented on this
    /// instance: R1d read USR as 0x01 with BUSY only), so the model keeps `usr` at 0. The PTIME case is `ier` with bit 7
    /// set, `lsr` 0x41 (THRE clear, TEMT set), as read on the board.
    struct Uart {
        lcr: u32,
        ier: u32,
        lsr: u32,
        usr: u32,
        writes: Vec<(usize, u32)>,
    }

    impl Uart {
        fn ready() -> Self {
            Uart {
                lcr: 0x03,
                ier: 0x00,
                lsr: 0x60,
                usr: 0,
                writes: Vec::new(),
            }
        }

        /// The ROM's state on the board: PTIME set, THRE reading 0 with TEMT set (LSR 0x41), USR 0 (no FIFO status).
        fn ptime() -> Self {
            Uart {
                lcr: 0x03,
                ier: 0x81,
                lsr: 0x41,
                usr: 0,
                writes: Vec::new(),
            }
        }

        fn sent(&self) -> Vec<u32> {
            self.writes
                .iter()
                .filter(|(a, _)| *a == BASE + THR)
                .map(|(_, v)| *v)
                .collect()
        }
    }

    impl RegAccess for Uart {
        fn read32(&mut self, addr: usize) -> u32 {
            match addr - BASE {
                LCR => self.lcr,
                IER => self.ier,
                LSR => self.lsr,
                0x7c => self.usr,
                _ => 0,
            }
        }
        fn write32(&mut self, addr: usize, value: u32) {
            match addr - BASE {
                IER => self.ier = value,
                LCR => self.lcr = value,
                _ => {}
            }
            self.writes.push((addr, value));
        }
    }

    #[test]
    fn a_uart_the_rom_left_in_8n1_is_not_reprogrammed_and_the_banner_goes_out_in_order() {
        let mut uart = Uart::ready();
        assert!(!configure_if_unset(&mut uart));
        write_line(&mut uart, "ok").unwrap();
        assert_eq!(
            uart.sent(),
            vec![b'o' as u32, b'k' as u32, b'\r' as u32, b'\n' as u32]
        );
        assert!(
            uart.writes.iter().all(|(a, _)| *a == BASE + THR),
            "only the transmit register was written"
        );
    }

    #[test]
    fn an_unset_uart_gets_its_divisor_and_line_format_and_nothing_else() {
        let mut uart = Uart {
            lcr: 0,
            ..Uart::ready()
        };
        assert!(configure_if_unset(&mut uart));
        assert_eq!(uart.lcr, LCR_8N1);
        // Offset 0x04 is the divisor's high byte (DLM) while DLAB is set: the only write there is that one.
        assert_eq!(
            uart.writes.iter().filter(|(a, _)| *a == BASE + IER).count(),
            1
        );
    }

    #[test]
    fn a_programmable_thre_uart_takes_its_bytes_when_thre_reads_zero() {
        // The board's state: PTIME set, LSR.THRE 0, TEMT 1 (LSR 0x41), and USR 0 (no FIFO status). The TEMT wait sends.
        // A wait on USR.TFNF, which reads 0 here, would give up: `Err(Stuck)`. The R1d code waited on TFNF.
        let mut uart = Uart::ptime();
        write_line(&mut uart, "ok").expect("the FIFO has room, so the write must not give up");
        assert_eq!(
            uart.sent(),
            vec![b'o' as u32, b'k' as u32, b'\r' as u32, b'\n' as u32]
        );
    }

    #[test]
    fn clearing_ptime_clears_only_bit_7_and_is_refused_while_dlab_is_set() {
        let mut uart = Uart::ptime();
        assert!(clear_programmable_thre(&mut uart));
        assert_eq!(
            uart.ier, 0x01,
            "bit 7 is cleared, the other enables are kept"
        );
        let mut dlab = Uart {
            lcr: 0x83,
            ..Uart::ptime()
        };
        assert!(
            !clear_programmable_thre(&mut dlab),
            "with DLAB set, offset 0x04 is the divisor, not IER"
        );
        assert!(dlab.writes.is_empty());
    }

    #[test]
    fn a_transmitter_that_never_empties_is_a_stuck_error_not_a_hang() {
        // The transmitter never empties: TEMT stays clear, so no byte is sent and the wait gives up.
        let mut uart = Uart {
            lsr: 0x20,
            ..Uart::ptime()
        };
        assert_eq!(write_byte(&mut uart, b'x'), Err(Stuck));
        assert!(uart.sent().is_empty());
    }
}
