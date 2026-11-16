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
use lemnos_k230_usbboot_stage::pads::{self, PadGuard};
use lemnos_k230_usbboot_stage::report::{self, BootAreas, CardState, NoCard};
use lemnos_k230_usbboot_stage::sd::SdController;
use lemnos_k230_usbboot_stage::uart;
use lemnos_k230_usbboot_stage::wdt::RegAccess;
use lemnos_k230_usbboot_stage::medium::{Geometry, Medium, MediumError, NoMedium};
use lemnos_k230_usbboot_stage::probe::{Action, Clock, Probe};
use lemnos_k230_usbboot_stage::sd::{
    Card, SdMedium, boot_mode, controller_from_parameter, resolve_controller,
};
use lemnos_k230_usbboot_stage::stage::Stage;
use lemnos_k230_usbboot_stage::sysctl::{SOC_GLB_RST, SOC_RESET_VALUE};
use lemnos_k230_usbboot_stage::usb_dev::Device;
use lemnos_k230_usbboot_stage::wdt::{self, Dog};

use crate::dwc2::Dwc2;
use crate::sdhci::SdhciHost;
use crate::Mmio;

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
        "la t6, lemnos_image_block",
        "lw s0, 4(t6)",
        "lw t1, 8(t6)",
        "xor t1, s0, t1",
        "li t2, -1",
        "bne t1, t2, 90f",
        "andi t1, s0, 4",
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
        // Readback of wdt0: CR enabled, CCVR moves within 16 reads.
        "andi t1, s0, 1",
        "beqz t1, 30f",
        "li t0, 0x91106000",
        "lw t2, 0x00(t0)",
        "andi t2, t2, 1",
        "beqz t2, 90f",
        "lw t2, 0x08(t0)",
        "li t3, 16",
        "41:",
        "lw t4, 0x08(t0)",
        "bne t4, t2, 42f",
        "addi t3, t3, -1",
        "bnez t3, 41b",
        "j 90f",
        "42:",
        "30:",
        // Readback of wdt1, the same.
        "andi t1, s0, 2",
        "beqz t1, 50f",
        "li t0, 0x91106800",
        "lw t2, 0x00(t0)",
        "andi t2, t2, 1",
        "beqz t2, 90f",
        "lw t2, 0x08(t0)",
        "li t3, 16",
        "61:",
        "lw t4, 0x08(t0)",
        "bne t4, t2, 62f",
        "addi t3, t3, -1",
        "bnez t3, 61b",
        "j 90f",
        "62:",
        "50:",
        "mv a0, s0",
        "la sp, lemnos_stack_top",
        "call lemnos_image_run",
        // The reset: a failed check, the SoC mode, or a return from the run (which does not happen).
        "90:",
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

/// Writes a line on the first UART. Nothing else is written to the UART block, and the pads are not touched.
fn say<R: RegAccess>(regs: &mut R, line: &str) {
    let _ = uart::write_line(regs, line);
}

/// Writes a 32-bit value as eight hex digits on the UART (no formatting machinery in the image).
fn say_hex<R: RegAccess>(regs: &mut R, label: &str, value: u32) {
    let digits = hex8(value);
    let digits = core::str::from_utf8(&digits).unwrap_or("????????");
    let _ = uart::write_line_parts(regs, &[label, " ", digits]);
}

fn hex8(value: u32) -> [u8; 8] {
    let mut out = [b'0'; 8];
    for (i, digit) in out.iter_mut().enumerate() {
        let nibble = (value >> (28 - 4 * i)) & 0xf;
        *digit = b"0123456789abcdef"[nibble as usize];
    }
    out
}

/// The report (stage L): read everything, write nothing, answer the records. The order is the reviewer's:
/// the banner, the boot mode, the pad dump, the gate, then the SD card only if the gate passed and a controller
/// was given, then the records over USB until the watchdog returns the board.
fn run_report() -> ! {
    let mut regs = PadGuard::new(Mmio);
    uart::configure_if_unset(&mut regs);
    say(&mut regs, "lemnos stage: report (no writes; watchdog armed before this line)");
    let ctl = boot_ctl();
    say_hex(&mut regs, "step 1: boot mode soc_boot_ctl", ctl);
    let pad_words = pads::dump(&mut regs);
    say(&mut regs, "step 2: pad dump read, IO0 to IO63 (no pad written)");
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
            say(&mut regs, "SD not reachable without pad setup: an SD pad is not on the SD function as the ROM left it");
            let _ = pad;
            (None, NoCard::PadGate)
        }
        Ok(()) => match read_sd_parameter().and_then(|v| controller_from_parameter(v).ok()) {
            None => {
                say(&mut regs, "step 3: no SD controller given; the card is not read");
                (None, NoCard::NotGiven)
            }
            Some(controller) => {
                say(&mut regs, "step 3: card read (CID, CSD, boot area)");
                match read_card(controller, &mut cid, &mut blocks) {
                    Ok(areas) => (Some(areas), NoCard::Failed),
                    Err(()) => {
                        say(&mut regs, "step 3: the card did not answer or a read failed");
                        (None, NoCard::Failed)
                    }
                }
            }
        },
    };
    let state = match &areas {
        Some(areas) => CardState::Read(areas),
        None => CardState::Absent(reason),
    };
    let records = report::build(ctl, &pad_words, &cid, blocks, state);
    say(&mut regs, "step 4: report built; answering over USB until the watchdog returns the board");
    report::serve(Dwc2::new(), &records)
}

/// The card's identity, capacity and boot area, read through the SD host the parameter names.
fn read_card(controller: SdController, cid: &mut [u8; 16], blocks: &mut u64) -> Result<BootAreas, ()> {
    let host = SdhciHost::new(controller.base()).map_err(|_| ())?;
    let mut card = Card::init(host).map_err(|_| ())?;
    *cid = card.cid();
    *blocks = card.blocks();
    report::read_boot_areas(&mut card).map_err(|_| ())
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

