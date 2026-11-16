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
//! `LSR` 20. The `LSR` transmitter-empty bit is bit 5 (`THRE`).

use crate::wdt::RegAccess;

/// The UART's base (`serial0`).
pub const BASE: usize = 0x9140_0000;
/// The baud divisor for 115200 at 50 MHz, rounded (`50e6 / (16 * 115200)`).
pub const DIVISOR_115200: u32 = 27;
const THR: usize = 0x00;
const DLL: usize = 0x00;
const DLM: usize = 0x04;
const LCR: usize = 0x0c;
const LSR: usize = 0x14;
/// Line control: 8 data bits, no parity, one stop bit (8N1), and DLAB (bit 7) to reach the divisor.
const LCR_8N1: u32 = 0x03;
const LCR_DLAB: u32 = 0x80;
/// Line status: transmitter holding register empty (THRE, bit 5).
const LSR_THRE: u32 = 0x20;
/// The bounded wait for THRE per byte, in reads.
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

/// Writes one byte, after the transmitter is empty. An error when the transmitter never empties (a UART the
/// ROM left unusable, or unclocked): the caller reports it and prints nothing more.
pub fn write_byte<R: RegAccess>(regs: &mut R, byte: u8) -> Result<(), ()> {
    for _ in 0..SPIN {
        if regs.read32(BASE + LSR) & LSR_THRE != 0 {
            regs.write32(BASE + THR, u32::from(byte));
            return Ok(());
        }
    }
    Err(())
}

/// Writes several strings as one line, then a newline. Stops at the first byte that cannot be sent.
pub fn write_line_parts<R: RegAccess>(regs: &mut R, parts: &[&str]) -> Result<(), ()> {
    for part in parts {
        for &byte in part.as_bytes() {
            write_byte(regs, byte)?;
        }
    }
    write_byte(regs, b'\r')?;
    write_byte(regs, b'\n')
}

/// Writes a line and a newline. Stops at the first byte that cannot be sent.
pub fn write_line<R: RegAccess>(regs: &mut R, line: &str) -> Result<(), ()> {
    for &byte in line.as_bytes() {
        write_byte(regs, byte)?;
    }
    write_byte(regs, b'\r')?;
    write_byte(regs, b'\n')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A UART model: the line status says the transmitter is empty, or never is.
    struct Uart {
        lcr: u32,
        lsr: u32,
        writes: Vec<(usize, u32)>,
    }

    impl RegAccess for Uart {
        fn read32(&mut self, addr: usize) -> u32 {
            match addr - BASE {
                LCR => self.lcr,
                LSR => self.lsr,
                _ => 0,
            }
        }
        fn write32(&mut self, addr: usize, value: u32) {
            self.writes.push((addr, value));
        }
    }

    #[test]
    fn a_uart_the_rom_left_in_8n1_is_not_reprogrammed_and_the_banner_goes_out_in_order() {
        let mut uart = Uart { lcr: 0x03, lsr: LSR_THRE, writes: Vec::new() };
        assert!(!configure_if_unset(&mut uart));
        write_line(&mut uart, "ok").unwrap();
        let sent: Vec<u32> = uart.writes.iter().filter(|(a, _)| *a == BASE + THR).map(|(_, v)| *v).collect();
        assert_eq!(sent, vec![b'o' as u32, b'k' as u32, b'\r' as u32, b'\n' as u32]);
        assert!(uart.writes.iter().all(|(a, _)| *a == BASE + THR), "only the transmit register was written");
    }

    #[test]
    fn an_unset_uart_gets_its_divisor_and_line_format_and_nothing_else() {
        let mut uart = Uart { lcr: 0, lsr: LSR_THRE, writes: Vec::new() };
        assert!(configure_if_unset(&mut uart));
        assert_eq!(uart.writes[0], (BASE + LCR, LCR_DLAB | LCR_8N1));
        assert!(uart.writes.contains(&(BASE + DLL, DIVISOR_115200)));
        assert!(uart.writes.contains(&(BASE + DLM, 0)));
        assert_eq!(uart.writes.last(), Some(&(BASE + LCR, LCR_8N1)));
    }

    #[test]
    fn a_transmitter_that_never_empties_is_an_error_not_a_hang() {
        let mut uart = Uart { lcr: 0x03, lsr: 0, writes: Vec::new() };
        assert_eq!(write_byte(&mut uart, b'x'), Err(()));
        assert!(uart.writes.is_empty());
    }
}
