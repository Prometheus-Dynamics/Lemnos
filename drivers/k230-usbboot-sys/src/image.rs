//! The stage image: one binary for the stage and for every probe run. The host patches its mode block
//! ([`lemnos_k230_usbboot_stage::image`]) before sending, and the entry below does what the mode names,
//! in this order, before anything else:
//! 1. checks the block (its inverses): a corrupt block takes the SoC reset, so a bad patch starts nothing;
//! 2. the SoC reset at once, if the mode says so;
//! 3. arms `wdt0` (TOP 11) and/or `wdt1` (TOP 11 in the stage, TOP 10 in a probe), each read back: CR
//!    enabled and CCVR moving within 16 reads; a failed readback takes the SoC reset;
//! 4. sets the stack and calls [`lemnos_image_run`] with the mode.
//!
//! The mode decides only what is armed. Once a watchdog is armed nothing in the image can disarm it (no
//! code path writes a watchdog's CR to clear it, and the host reaches no register). The deadline (the
//! time CSR against `SPIN_DEADLINE_TICKS`) and the SoC reset are the paths the image's own code takes.
//!
//! The image is linked at `LOAD_ADDRESS` (`drivers/k230-usbboot-image/image.ld`), so it is not
//! position-independent: the host loads it there and only there.

use lemnos_k230_usbboot_stage::image::{BLOCK_MAGIC, REPORT, SD_NONE, STAGE, read_block};
use lemnos_k230_usbboot_stage::medium::{Geometry, Medium, MediumError, NoMedium};
use lemnos_k230_usbboot_stage::pads::{self, PadGuard};
use lemnos_k230_usbboot_stage::probe::{Action, Clock, Probe};
use lemnos_k230_usbboot_stage::report::{self, BootAreas, NoCard};
use lemnos_k230_usbboot_stage::sd::{
    Card, SdMedium, boot_mode, controller_from_parameter, resolve_controller,
};
use lemnos_k230_usbboot_stage::sd::{SdController, SdError};
use lemnos_k230_usbboot_stage::stage::Stage;
use lemnos_k230_usbboot_stage::sysctl::{SOC_GLB_RST, SOC_RESET_VALUE};
use lemnos_k230_usbboot_stage::uart;
use lemnos_k230_usbboot_stage::usb_dev::Device;
use lemnos_k230_usbboot_stage::wdt::RegAccess;
use lemnos_k230_usbboot_stage::wdt::{self, Dog};

use crate::Mmio;
use crate::dwc2::Dwc2;
use crate::sdhci::{self, SdhciHost};

/// The mode block the host patches: magic, mode, !mode, SD parameter, !SD parameter. The host finds the
/// magic and writes the rest. Unpatched, it is mode 0 (the spin) with no SD parameter.
#[unsafe(no_mangle)]
#[used]
pub static lemnos_image_block: [u32; 5] = [BLOCK_MAGIC, 0, !0, SD_NONE, !SD_NONE];

/// The boot system controller's `soc_boot_ctl` (`0x91102040`, `sysctl_boot.h:45`), read only.
pub fn boot_ctl() -> u32 {
    // SAFETY: `0x91102040` is the boot system controller's `soc_boot_ctl`, a mapped aligned register
    // whose read has no effect (it reports the boot mode the ROM latched).
    unsafe { crate::read32(0x9110_2040) }
}

/// The SoC software reset (`soc_glb_rst`): the second path. It does not depend on a watchdog.
pub fn soc_reset() -> ! {
    // SAFETY: `SOC_GLB_RST` is the boot system controller's reset register (`sysctl_boot.h:47`); the
    // write is the documented reset value, and nothing after it runs unless the reset did not happen.
    unsafe { crate::write32(SOC_GLB_RST, SOC_RESET_VALUE) };
    loop {
        core::hint::spin_loop();
    }
}

/// The time CSR, read by one instruction.
#[unsafe(naked)]
extern "C" fn read_time() -> u64 {
    core::arch::naked_asm!("rdtime a0", "ret")
}

/// The image's time, from the time CSR.
pub struct Ticks;

impl Clock for Ticks {
    fn ticks(&self) -> u64 {
        read_time()
    }
}

/// R1b: the watchdog registers as the ROM left them (indices 0..6: WDT0 CR, TORR, CCVR; WDT1 CR, TORR, CCVR), and
/// WDT0's TORR and CR as read back after the report's arming (6 and 7). Written by the entry, read by the report.
#[unsafe(no_mangle)]
pub static mut lemnos_diag: [u32; 8] = [0; 8];

/// The entry: the first code the boot ROM runs after the image is loaded at `LOAD_ADDRESS`. Checks the
/// mode block, arms what the mode names, reads it back, sets the stack, calls [`lemnos_image_run`].
/// Only `t*` and `s0` are used before the stack exists.
///
/// # Safety
///
/// Only the boot ROM may jump here, with the image loaded at its load address. It never returns.
#[cfg(all(target_arch = "riscv64", target_os = "none"))]
#[unsafe(naked)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lemnos_image_entry() -> ! {
    core::arch::naked_asm!(
        // R1b diagnostic: markers and the ROM's watchdog state, before any arming. The values are kept in
        // lemnos_diag and printed by the report, after its first line. No write to a watchdog happens here.
        "li t0, 0x91400000",
        "li t1, 0x21",
        "sw t1, 0x00(t0)",
        "li t3, 0x40000",
        "201:",
        "addi t3, t3, -1",
        "bnez t3, 201b",
        "li t0, 0x91400000",
        "li t1, 0x42",
        "sw t1, 0x00(t0)",
        "li t3, 0x40000",
        "202:",
        "addi t3, t3, -1",
        "bnez t3, 202b",
        // R1f: .bss is NOLOAD and the ROM copies only the file, so zero it before any static is read. A stale give-up
        // flag in .bss (GAVE_UP) silenced every report line in R1e, with no T printed.
        "la t0, lemnos_bss_start",
        "la t1, lemnos_bss_end",
        "301:",
        "sw zero, 0(t0)",
        "addi t0, t0, 4",
        "bltu t0, t1, 301b",
        "la t6, lemnos_diag",
        "li t0, 0x91106000",
        "lw t1, 0x00(t0)",
        "sw t1, 0(t6)",
        "lw t1, 0x04(t0)",
        "sw t1, 4(t6)",
        "lw t1, 0x08(t0)",
        "sw t1, 8(t6)",
        "li t0, 0x91106800",
        "lw t1, 0x00(t0)",
        "sw t1, 12(t6)",
        "lw t1, 0x04(t0)",
        "sw t1, 16(t6)",
        "lw t1, 0x08(t0)",
        "sw t1, 20(t6)",
        "la t6, lemnos_image_block",
        "lw s0, 4(t6)",
        "lw t1, 8(t6)",
        "xor t1, s0, t1",
        "li t2, -1",
        "li t5, 0x42",
        "bne t1, t2, 90f",
        "andi t1, s0, 4",
        "li t5, 0x53",
        "bnez t1, 90f",
        // wdt0: TOP 11, kick, enable.
        "andi t1, s0, 1",
        "beqz t1, 10f",
        "li t0, 0x91106000",
        "li t1, 0xbb",
        "sw t1, 0x04(t0)",
        "li t1, 0x76",
        "sw t1, 0x0c(t0)",
        "li t1, 1",
        "sw t1, 0x00(t0)",
        "10:",
        // R1b: the wdt0 arming as read back: TORR (its TOP is the low nibble) and CR, then marker W.
        "la t6, lemnos_diag",
        "li t0, 0x91106000",
        "lw t1, 0x04(t0)",
        "sw t1, 24(t6)",
        "lw t1, 0x00(t0)",
        "sw t1, 28(t6)",
        "li t0, 0x91400000",
        "li t1, 0x57",
        "sw t1, 0x00(t0)",
        "li t3, 0x40000",
        "203:",
        "addi t3, t3, -1",
        "bnez t3, 203b",
        // wdt1: TOP 11 in the stage, TOP 10 in a probe; kick, enable.
        "andi t1, s0, 2",
        "beqz t1, 20f",
        "li t0, 0x91106800",
        "li t1, 0xaa",
        "andi t2, s0, 8",
        "beqz t2, 21f",
        "li t1, 0xbb",
        "21:",
        "sw t1, 0x04(t0)",
        "li t1, 0x76",
        "sw t1, 0x0c(t0)",
        "li t1, 1",
        "sw t1, 0x00(t0)",
        "20:",
        // Readback of wdt0: CR enabled, and CCVR moves within a bounded wait (2^25 reads). Bench, 2026-11-16:
        // with a bound of 16 reads the counter had not moved on a K230D and the entry took the reset path.
        "andi t1, s0, 1",
        "beqz t1, 30f",
        "li t0, 0x91106000",
        "lw t2, 0x00(t0)",
        "andi t2, t2, 1",
        "li t5, 0x63",
        "beqz t2, 90f",
        "lw t2, 0x08(t0)",
        "li t3, 0x2000000",
        "41:",
        "lw t4, 0x08(t0)",
        "bne t4, t2, 42f",
        "addi t3, t3, -1",
        "bnez t3, 41b",
        "li t5, 0x6d",
        "j 90f",
        "42:",
        "30:",
        "li t0, 0x91400000",
        "li t1, 0x4b",
        "sw t1, 0x00(t0)",
        "li t3, 0x40000",
        "204:",
        "addi t3, t3, -1",
        "bnez t3, 204b",
        // Readback of wdt1, the same.
        "andi t1, s0, 2",
        "beqz t1, 50f",
        "li t0, 0x91106800",
        "lw t2, 0x00(t0)",
        "andi t2, t2, 1",
        "li t5, 0x43",
        "beqz t2, 90f",
        "lw t2, 0x08(t0)",
        "li t3, 0x2000000",
        "61:",
        "lw t4, 0x08(t0)",
        "bne t4, t2, 62f",
        "addi t3, t3, -1",
        "bnez t3, 61b",
        "li t5, 0x4d",
        "j 90f",
        "62:",
        "50:",
        "mv a0, s0",
        "la sp, lemnos_stack_top",
        "li t0, 0x91400000",
        "li t1, 0x43",
        "sw t1, 0x00(t0)",
        "li t3, 0x40000",
        "205:",
        "addi t3, t3, -1",
        "bnez t3, 205b",
        "call lemnos_image_run",
        "li t5, 0x52",
        // The reset: a failed check, the SoC mode, or a return from the run (which does not happen).
        "90:",
        // The cause, as one character on the console UART the ROM left running (THR at 0x91400000),
        // then a short wait so it leaves the shifter, then the SoC reset. B mode block, S reset mode,
        // c/m wdt0 not enabled / counter not moving, C/M the same for wdt1, R the run returned.
        "li t0, 0x91400000",
        "li t1, 0x21",
        "sw t1, 0x00(t0)",
        "li t1, 0x40000",
        "91:",
        "addi t1, t1, -1",
        "bnez t1, 91b",
        "sw t5, 0x00(t0)",
        "li t1, 0x40000",
        "92:",
        "addi t1, t1, -1",
        "bnez t1, 92b",
        "li t0, 0x91102060",
        "li t1, 0x10001",
        "sw t1, 0x00(t0)",
        "99:",
        "j 99b",
    )
}

/// The run the entry calls once the watchdogs the mode names are armed and read back: the stage (its
/// medium, its device path and its state machine) or a probe (telemetry and the deadline).
#[unsafe(no_mangle)]
pub extern "C" fn lemnos_image_run(flags: u32) -> ! {
    if flags & REPORT != 0 {
        run_report()
    } else if flags & STAGE != 0 {
        run_stage()
    } else {
        run_probe(flags)
    }
}

/// The UART the report writes to (serial0, `0x91400000`) and its line status register.
const REPORT_UART: usize = 0x9140_0000;
const REPORT_LSR: usize = REPORT_UART + 0x14;

/// R1c: one raw byte on the UART, with no LSR poll, then a short wait. The markers use this, so they print even when
/// the Rust path's LSR poll never sees the transmitter empty.
fn raw_put(byte: u8) {
    // SAFETY: a volatile store to the UART's transmit register, the address the entry writes its markers to.
    unsafe { core::ptr::write_volatile(REPORT_UART as *mut u32, u32::from(byte)) };
    for _ in 0..0x4000 {
        core::hint::spin_loop();
    }
}

/// R1c: the line status register, read once before the first Rust write.
fn raw_read_lsr() -> u32 {
    raw_read32(REPORT_LSR)
}

/// R1d: one 32-bit read of a UART register.
fn raw_read32(address: usize) -> u32 {
    // SAFETY: a volatile load of a register of the UART block (serial0), at an address the report names.
    unsafe { core::ptr::read_volatile(address as *const u32) }
}

/// R1c: a label byte, then 8 hex digits of `value`, CR LF, all raw.
fn raw_hex(label: u8, value: u32) {
    raw_put(label);
    for i in 0..8 {
        let nibble = (value >> (28 - 4 * i)) & 0xf;
        raw_put(b"0123456789abcdef"[nibble as usize]);
    }
    raw_put(b'\r');
    raw_put(b'\n');
}

/// R1c: whether a Rust UART write has already given up. The first give-up prints a raw `T`; the rest are silent.
static mut GAVE_UP: bool = false;

fn gave_up(result: Result<(), uart::Stuck>) {
    if result.is_err() {
        // SAFETY: a read of this image's own flag; the image runs one thread.
        let seen = unsafe { GAVE_UP };
        if !seen {
            // SAFETY: the same flag, written once here.
            unsafe { GAVE_UP = true };
            raw_put(b'T');
        }
    }
}

/// Writes a line on the first UART. Nothing else is written to the UART block, and the pads are not touched.
fn say<R: RegAccess>(regs: &mut R, line: &str) {
    // R1d: after the first give-up no Rust write is tried again, so a dead UART costs one bounded wait, not one per byte.
    if !unsafe_gave_up() {
        gave_up(uart::write_line(regs, line));
    }
}

fn unsafe_gave_up() -> bool {
    // SAFETY: a read of this image's own flag; the image runs one thread.
    unsafe { GAVE_UP }
}

/// Writes a 32-bit value as eight hex digits on the UART (no formatting machinery in the image).
fn say_hex<R: RegAccess>(regs: &mut R, label: &str, value: u32) {
    if unsafe_gave_up() {
        return;
    }
    let digits = hex8(value);
    let digits = core::str::from_utf8(&digits).unwrap_or("????????");
    gave_up(uart::write_line_parts(regs, &[label, " ", digits]));
}

fn hex8(value: u32) -> [u8; 8] {
    let mut out = [b'0'; 8];
    for (i, digit) in out.iter_mut().enumerate() {
        let nibble = (value >> (28 - 4 * i)) & 0xf;
        *digit = b"0123456789abcdef"[nibble as usize];
    }
    out
}

/// R1b: the watchdog registers the entry saved, as hex lines on the UART.
fn say_diag<R: RegAccess>(regs: &mut R) {
    // A copy of the array: the entry wrote it before this run, and nothing writes it after.
    // SAFETY: a read of this image's own static; the entry wrote it before this code ran and the image runs one thread.
    let d = unsafe { lemnos_diag };
    say_hex(regs, "diag wdt0 before arm CR", d[0]);
    say_hex(regs, "diag wdt0 before arm TORR", d[1]);
    say_hex(regs, "diag wdt0 before arm CCVR", d[2]);
    say_hex(regs, "diag wdt1 before arm CR", d[3]);
    say_hex(regs, "diag wdt1 before arm TORR", d[4]);
    say_hex(regs, "diag wdt1 before arm CCVR", d[5]);
    say_hex(regs, "diag wdt0 after arm TORR", d[6]);
    say_hex(regs, "diag wdt0 after arm TOP", d[6] & 0xf);
    say_hex(regs, "diag wdt0 after arm CR", d[7]);
}

/// The report (stage L): read everything, write nothing, answer the records. The order is the reviewer's:
/// the banner, the boot mode, the pad dump, the gate, then the SD card only if the gate passed and a controller
/// was given, then the records over USB until the watchdog returns the board.
fn run_report() -> ! {
    let mut regs = PadGuard::new(Mmio);
    raw_put(b'P');
    // R1c: the LSR once, before the first Rust write, printed by the raw path.
    raw_hex(b'L', raw_read_lsr());
    // R1d: IER (PTIME is its bit 7) and USR, raw, before PTIME is cleared; then cleared while DLAB is clear.
    raw_hex(b'I', raw_read32(REPORT_UART + 0x04));
    raw_hex(b'S', raw_read32(REPORT_UART + 0x7c));
    let cleared = uart::clear_programmable_thre(&mut regs);
    raw_hex(b'C', u32::from(cleared));
    uart::configure_if_unset(&mut regs);
    raw_put(b'U');
    say_diag(&mut regs);
    say(
        &mut regs,
        "lemnos stage: report (no writes; watchdog armed before this line)",
    );
    let ctl = boot_ctl();
    say_hex(&mut regs, "step 1: boot mode soc_boot_ctl", ctl);
    let pad_words = pads::dump(&mut regs);
    say(
        &mut regs,
        "step 2: pad dump read, IO0 to IO63 (no pad written)",
    );
    for row in 0..8 {
        let mut line = [0u8; 8 * 9];
        let mut at = 0;
        for i in 0..8 {
            let v = hex8(pad_words[8 * row + i]);
            line[at..at + 8].copy_from_slice(&v);
            at += 8;
            line[at] = b' ';
            at += 1;
        }
        let _ = uart::write_line(&mut regs, core::str::from_utf8(&line[..at]).unwrap_or("?"));
    }
    let mut cid = [0u8; 16];
    let mut blocks = 0u64;
    let (areas, reason) = match pads::sd_pads_on_sd_function(&pad_words) {
        Err(pad) => {
            say(
                &mut regs,
                "SD not reachable without pad setup: an SD pad is not on the SD function as the ROM left it",
            );
            let _ = pad;
            (None, NoCard::PadGate)
        }
        Ok(()) => match read_sd_parameter().and_then(|v| controller_from_parameter(v).ok()) {
            None => {
                say(
                    &mut regs,
                    "step 3: no SD controller given; the card is not read",
                );
                (None, NoCard::NotGiven)
            }
            Some(controller) => {
                // R3c: the controller as the ROM left it, read before the stage resets it.
                say_sd(&mut regs, "sd rom", controller.base());
                say(&mut regs, "step 3: card read (CID, CSD, boot area)");
                match read_card(controller, &mut cid, &mut blocks) {
                    Ok(areas) => (Some(areas), NoCard::Failed),
                    Err((at, error)) => {
                        say(
                            &mut regs,
                            "step 3: the card did not answer or a read failed",
                        );
                        // R3c: where, which command and why, then the controller after the try.
                        let (words, command) = error.words();
                        say(&mut regs, at);
                        say(&mut regs, words);
                        if let Some(command) = command {
                            say_hex(&mut regs, "sd error command", u32::from(command));
                        }
                        say_sd(&mut regs, "sd end", controller.base());
                        (None, NoCard::Failed)
                    }
                }
            }
        },
    };
    // Console-only report (bench ruling, 2026-11-16): this image does NOT reset or touch the USB
    // controller. Its dwc2 device path has never run on hardware, and a report that depends on it
    // could never arrive if it fails. Everything is printed here, then the SoC reset returns the board.
    let _ = reason;
    match &areas {
        None => say(&mut regs, "step 3: no card report"),
        Some(areas) => {
            say_hex(
                &mut regs,
                "card cid0",
                u32::from_be_bytes([cid[0], cid[1], cid[2], cid[3]]),
            );
            say_hex(
                &mut regs,
                "card cid1",
                u32::from_be_bytes([cid[4], cid[5], cid[6], cid[7]]),
            );
            say_hex(
                &mut regs,
                "card cid2",
                u32::from_be_bytes([cid[8], cid[9], cid[10], cid[11]]),
            );
            say_hex(
                &mut regs,
                "card cid3",
                u32::from_be_bytes([cid[12], cid[13], cid[14], cid[15]]),
            );
            say_hex(&mut regs, "card blocks hi", (blocks >> 32) as u32);
            say_hex(&mut regs, "card blocks lo", blocks as u32);
            say_hex(&mut regs, "verdict code", areas.verdict.code() as u32);
            let crcs = report::crcs(areas);
            say_hex(&mut regs, "copy0 block crc32", crcs[0]);
            say_hex(&mut regs, "copy1 block crc32", crcs[1]);
            for (n, block) in areas.blocks.iter().enumerate() {
                say(
                    &mut regs,
                    if n == 0 {
                        "copy0 header, first 48 bytes (sector 2048):"
                    } else {
                        "copy1 header, first 48 bytes (sector 3072):"
                    },
                );
                for w in 0..12 {
                    let b = &block[4 * w..4 * w + 4];
                    say_hex(&mut regs, " ", u32::from_be_bytes([b[0], b[1], b[2], b[3]]));
                }
            }
        }
    }
    say(
        &mut regs,
        "step 4: report printed on the console only; USB untouched; SoC reset now",
    );
    say(&mut regs, "lemnos stage: end");
    // Let the last characters leave the UART before the reset.
    let mut n = 0x200000u32;
    while n != 0 {
        n -= 1;
        core::hint::spin_loop();
    }
    soc_reset()
}

/// The card's identity, capacity and boot area, read through the SD host the parameter names.
/// Reads the card through `controller`. An error carries the step it came from.
fn read_card(
    controller: SdController,
    cid: &mut [u8; 16],
    blocks: &mut u64,
) -> Result<BootAreas, (&'static str, SdError)> {
    let host =
        SdhciHost::new(controller.base()).map_err(|e| ("sd error in: controller start", e))?;
    let mut card = Card::init(host).map_err(|e| ("sd error in: card start", e))?;
    *cid = card.cid();
    *blocks = card.blocks();
    report::read_boot_areas(&mut card).map_err(|e| ("sd error in: boot area read", e))
}

/// R3c: the controller's status registers ([`sdhci::SNAPSHOT`]) as hex lines. Reads only.
fn say_sd<R: RegAccess>(regs: &mut R, when: &str, base: usize) {
    let values = sdhci::snapshot(base);
    for (value, (_, name)) in values.iter().zip(sdhci::SNAPSHOT) {
        if unsafe_gave_up() {
            return;
        }
        let digits = hex8(*value);
        let digits = core::str::from_utf8(&digits).unwrap_or("????????");
        gave_up(uart::write_line_parts(
            regs,
            &[when, " ", name, " ", digits],
        ));
    }
}

fn run_probe(flags: u32) -> ! {
    let mut probe = Probe::new(Dwc2::new(), flags).with_boot(boot_ctl());
    let mut regs = Mmio;
    let clock = Ticks;
    loop {
        if let Action::Reset(_path) = probe.tick(&mut regs, &clock) {
            soc_reset();
        }
    }
}

/// The stage's medium: the SD card, or nothing when the card cannot be started. A stage with no medium
/// refuses every write and header, so nothing is stored.
enum ImageMedium {
    Sd(SdMedium<SdhciHost>),
    Absent(NoMedium),
}

impl Medium for ImageMedium {
    fn geometry(&self) -> Geometry {
        match self {
            Self::Sd(m) => m.geometry(),
            Self::Absent(m) => m.geometry(),
        }
    }

    fn read(&mut self, offset: u64, out: &mut [u8]) -> Result<(), MediumError> {
        match self {
            Self::Sd(m) => m.read(offset, out),
            Self::Absent(m) => m.read(offset, out),
        }
    }

    fn write(&mut self, offset: u64, data: &[u8]) -> Result<(), MediumError> {
        match self {
            Self::Sd(m) => m.write(offset, data),
            Self::Absent(m) => m.write(offset, data),
        }
    }

    fn erase(&mut self, offset: u64, len: u64) -> Result<(), MediumError> {
        match self {
            Self::Sd(m) => m.erase(offset, len),
            Self::Absent(m) => m.erase(offset, len),
        }
    }
}

/// The SD controller the stage boots from: the boot mode the ROM latched, and the parameter the host
/// patched (none when unpatched). Refused by name otherwise, and the medium is then absent.
fn stage_medium(sd: Option<u32>) -> ImageMedium {
    let controller = sd.and_then(|v| controller_from_parameter(v).ok());
    let mode = boot_mode(boot_ctl());
    match resolve_controller(mode, controller) {
        Ok(controller) => match SdhciHost::new(controller.base())
            .map_err(|_| ())
            .and_then(|host| Card::init(host).map_err(|_| ()))
        {
            Ok(card) => ImageMedium::Sd(SdMedium::new(card)),
            Err(()) => ImageMedium::Absent(NoMedium),
        },
        Err(_) => ImageMedium::Absent(NoMedium),
    }
}

fn run_stage() -> ! {
    let sd = read_sd_parameter();
    let medium = stage_medium(sd);
    // The arming was done by the entry; the stage's own arming proof is made again, identically.
    let mut regs = Mmio;
    let armed = match wdt::arm(&mut regs) {
        Ok(armed) => armed,
        Err(_) => soc_reset(),
    };
    let mut stage = Stage::new(medium, Dog::new(Mmio, armed));
    let mut device = match Device::start(Dwc2::new()) {
        Ok(device) => device,
        Err(_) => soc_reset(),
    };
    loop {
        // A device error is not a reason to feed or to reset: the stage only feeds after a verified
        // chunk, inside `stage`.
        let _ = device.step(&mut stage);
    }
}

/// The SD parameter from the mode block, when the host passed one.
fn read_sd_parameter() -> Option<u32> {
    // SAFETY: `lemnos_image_block` is this image's own static, read-only, and its word reads are
    // aligned and in bounds.
    let words: [u32; 5] = unsafe { core::ptr::read_volatile(&raw const lemnos_image_block) };
    read_block(&words).and_then(|(_, sd)| sd)
}
