//! A GPIO line driven by an RP1 PIO state machine that runs a program the driver supplies. A board
//! file names it `pio-out:gpio=N`; the driver's [`PioJob`](lemnos_hal::pio::PioJob) is what the
//! machine does with that pin.
//!
//! The machine is one state machine of the block, claimed for the machine's life and its program
//! removed when it ends. Through the Lemnos module the claim and its client are the module's, as
//! for `pio-i2c`; without it the stock PIO node is used. The stock node must exist either way.
//!
//! The program is placed at the first origin that fits in instruction memory, and its jumps are
//! moved there. The clock divider comes from the block's clock, 200 MHz on the RP1.

use std::io;
use std::path::Path;
use std::time::Duration;

use lemnos_core::Status;
use lemnos_hal::ErrorKind;
use lemnos_hal::pio::{PioJob, PioMachine, PioOperation};
use lemnos_linux_sys::rp1_pio::{self, INSTRUCTION_COUNT, PioDevice, SmConfig, regs};

use crate::kind_of;
use crate::module::{self, Node};
use crate::open_status;
use crate::pio_i2c::device::{self, pins_free};

/// The RP1 PIO clock (`clock_get_hz(clk_sys)` in `pio_rp1.h`), in hertz.
const PIO_CLOCK_HZ: f64 = 200_000_000.0;
/// Bounce buffers for the DMA into the machine's FIFO: bytes each, and how many.
const DMA_BUF_SIZE: u32 = 64;
const DMA_BUF_COUNT: u32 = 4;
/// The FUNCSEL of a pin that a PIO block drives.
const GPIO_FUNC_SIO: u16 = rp1_pio::GPIO_FUNC_SIO;

const PREFIX: &str = "pio-out:";

/// The GPIO a `pio-out:gpio=N` name means: `Some(Ok(n))`, `Some(Err)` for a malformed name, and
/// `None` for a name of another kind.
fn parse(need: &str) -> Option<Result<u8, ()>> {
    let rest = need.strip_prefix(PREFIX)?;
    let gpio = rest
        .strip_prefix("gpio=")
        .ok_or(())
        .and_then(|n| n.parse::<u8>().map_err(|_| ()));
    let gpio = gpio.and_then(|n| {
        if u32::from(n) < rp1_pio::GPIO_COUNT {
            Ok(n)
        } else {
            Err(())
        }
    });
    Some(gpio)
}

/// Whether a board-file name is a PIO output and `dev` has the block's node.
pub(crate) fn provided(dev: &Path, need: &str) -> bool {
    matches!(parse(need), Some(Ok(_))) && dev.join(device::DEVNODE).exists()
}

/// The PIO clock divider for `hz`, as the integer and 1/256 parts of `CLKDIV`.
fn clkdiv_for(hz: u32) -> Result<(u16, u8), String> {
    if hz == 0 {
        return Err("PIO clock must be above zero".into());
    }
    let div = PIO_CLOCK_HZ / f64::from(hz);
    let int = div.floor();
    if !(1.0..=65_535.0).contains(&int) {
        return Err(format!("PIO clock {hz} Hz is out of range"));
    }
    let frac = ((div - int) * 256.0).floor().clamp(0.0, 255.0) as u8;
    Ok((int as u16, frac))
}

/// `program` with every JMP target moved by `base`. A JMP is the instruction whose top three bits
/// are zero; its target is the low five bits.
fn relocate(program: &[u16], base: u16) -> Vec<u16> {
    program
        .iter()
        .map(|&word| {
            if word >> 13 == 0 {
                (word & !0x1f) | ((word & 0x1f) + base)
            } else {
                word
            }
        })
        .collect()
}

/// A validated job: the program's length and the wrap window, in range.
fn check(job: &PioJob<'_>) -> Result<(), ErrorKind> {
    let len = job.program.len();
    let valid = (1..=INSTRUCTION_COUNT).contains(&len)
        && usize::from(job.wrap_bottom) < len
        && usize::from(job.wrap_top) < len
        && job.wrap_bottom <= job.wrap_top
        && (1..=32).contains(&job.out_bits)
        && clkdiv_for(job.clock_hz).is_ok();
    if valid {
        Ok(())
    } else {
        Err(ErrorKind::InvalidInput)
    }
}

/// A loaded program: its machine, origin and length.
#[derive(Clone, Copy)]
struct Loaded {
    sm: u16,
    base: u16,
    len: u16,
}

/// A GPIO line driven by one PIO state machine.
pub(crate) struct PioOut {
    device: PioDevice,
    pin: u8,
    loaded: Option<Loaded>,
}

impl PioOut {
    /// Stops the machine, returns the pin to the kernel, and removes the program and the claim.
    fn unload(&mut self) {
        if let Some(loaded) = self.loaded.take() {
            self.unload_loaded(loaded);
        }
    }

    fn start(&self, job: &PioJob<'_>) -> io::Result<Loaded> {
        let (int, frac) = clkdiv_for(job.clock_hz).map_err(|_| refused())?;
        pins_free(&[self.pin])?;
        let sm = self.device.claim_any_sm()?;
        let len = job.program.len();
        let mut base = None;
        for origin in 0..=(INSTRUCTION_COUNT - len) as u16 {
            match self
                .device
                .add_program(origin, &relocate(job.program, origin))
            {
                Ok(placed) if placed == origin => {
                    base = Some(origin);
                    break;
                }
                Ok(placed) => {
                    let _ = self.device.remove_program(placed, len as u16);
                }
                Err(_) => {}
            }
        }
        let Some(base) = base else {
            let _ = self.device.unclaim_sms(1 << sm);
            return Err(io::Error::from_raw_os_error(libc::ENOSPC));
        };
        let loaded = Loaded {
            sm,
            base,
            len: len as u16,
        };
        match self.configure(loaded, job, int, frac) {
            Ok(()) => Ok(loaded),
            Err(error) => {
                self.unload_loaded(loaded);
                Err(error)
            }
        }
    }

    /// Sets up the pin, the DMA and the machine, and starts it.
    fn configure(&self, loaded: Loaded, job: &PioJob<'_>, int: u16, frac: u8) -> io::Result<()> {
        let pin = u16::from(self.pin);
        let (bottom, top) = (
            u32::from(loaded.base) + u32::from(job.wrap_bottom),
            u32::from(loaded.base) + u32::from(job.wrap_top),
        );
        let config = SmConfig {
            clkdiv: regs::clkdiv(u32::from(int), u32::from(frac)),
            // Side-set drives the pin's level, not its direction; the wrap window is moved to
            // the program's place.
            execctrl: regs::execctrl_wrap(bottom, top),
            // Output words are shifted out most significant bit first, pulled as needed.
            shiftctrl: regs::shiftctrl_in(false, false, 32)
                | regs::shiftctrl_out(false, true, u32::from(job.out_bits)),
            // One side-set bit, not optional: every instruction drives the pin.
            pinctrl: regs::pinctrl_sideset(u32::from(self.pin), 1),
        };
        self.device.gpio_init(pin)?;
        self.device.gpio_set_function(pin, rp1_pio::GPIO_FUNC_PIO)?;
        self.device
            .config_xfer32(loaded.sm, rp1_pio::DIR_TO_SM, DMA_BUF_SIZE, DMA_BUF_COUNT)?;
        self.device.sm_init(loaded.sm, loaded.base, config)?;
        self.device
            .sm_set_pindirs(loaded.sm, 1 << self.pin, 1 << self.pin)?;
        self.device.sm_set_enabled(1 << loaded.sm, true)
    }

    fn unload_loaded(&self, loaded: Loaded) {
        let _ = self.device.sm_set_enabled(1 << loaded.sm, false);
        let _ = self.device.sm_set_pindirs(loaded.sm, 0, 1 << self.pin);
        let _ = self
            .device
            .gpio_set_function(u16::from(self.pin), GPIO_FUNC_SIO);
        let _ = self.device.remove_program(loaded.base, loaded.len);
        let _ = self.device.unclaim_sms(1 << loaded.sm);
    }
}

impl Drop for PioOut {
    fn drop(&mut self) {
        self.unload();
    }
}

impl PioMachine for PioOut {
    fn load(&mut self, job: &PioJob<'_>) -> Result<(), ErrorKind> {
        check(job)?;
        self.unload();
        let loaded = self.start(job).map_err(|e| kind_of(&e))?;
        self.loaded = Some(loaded);
        Ok(())
    }

    fn transaction(&mut self, operations: &mut [PioOperation<'_>]) -> Result<(), ErrorKind> {
        let Some(loaded) = self.loaded else {
            return Err(ErrorKind::InvalidInput);
        };
        for operation in operations.iter() {
            match operation {
                PioOperation::Write(words) => {
                    if words.is_empty() {
                        continue;
                    }
                    self.device
                        .xfer_to_sm(loaded.sm, words)
                        .map_err(|e| kind_of(&e))?;
                }
                PioOperation::DelayNs(ns) => {
                    std::thread::sleep(Duration::from_nanos(u64::from(*ns)));
                }
            }
        }
        Ok(())
    }
}

/// The machine a board-file name `pio-out:gpio=N` means: through `module` when it is open and
/// serves PIO, otherwise on the stock node under `dev`.
pub(crate) fn open(dev: &Path, need: &str, module: Option<&Node>) -> Result<PioOut, Status> {
    let pin = match parse(need) {
        None => return Err(Status::NOT_FOUND),
        Some(Err(())) => return Err(Status::INVALID_ARGUMENT),
        Some(Ok(pin)) => pin,
    };
    let through_module = match module {
        Some(node) => match PioDevice::through_module(node.shared()) {
            Ok(device) => Some(device),
            Err(error) if module::not_served(&error) => None,
            Err(error) => return Err(open_status(&error)),
        },
        None => None,
    };
    let device = match through_module {
        Some(device) => device,
        None => PioDevice::open(&dev.join(device::DEVNODE)).map_err(|e| open_status(&e))?,
    };
    Ok(PioOut {
        device,
        pin,
        loaded: None,
    })
}

fn refused() -> io::Error {
    io::Error::from_raw_os_error(libc::EINVAL)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pio_output_name_parses_and_needs_the_block_node() {
        assert_eq!(parse("pio-out:gpio=13"), Some(Ok(13)));
        assert_eq!(parse("pio-out:gpio=28"), Some(Err(())));
        assert_eq!(parse("pio-out:13"), Some(Err(())));
        assert_eq!(parse("pio-i2c:sda=8,scl=7"), None);
    }

    #[test]
    fn a_missing_node_is_no_device_and_a_bad_pin_is_refused() {
        let dir = std::env::temp_dir().join(format!("pio-out-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!provided(&dir, "pio-out:gpio=13"));
        std::fs::write(dir.join(device::DEVNODE), b"").unwrap();
        assert!(provided(&dir, "pio-out:gpio=13"));
        assert_eq!(
            open(&dir, "pio-out:gpio=99", None).err(),
            Some(Status::INVALID_ARGUMENT)
        );
        assert_eq!(open(&dir, "i2c-1", None).err(), Some(Status::NOT_FOUND));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn jumps_move_with_the_program_and_nothing_else_does() {
        // jmp 3 (cond not-x, side 1 delay 1) and jmp 0 (always), then an out and a nop.
        let program = [0x1123, 0x1400, 0x6221, 0xa442];
        let moved = relocate(&program, 5);
        assert_eq!(moved, vec![0x1128, 0x1405, 0x6221, 0xa442]);
    }

    #[test]
    fn the_clock_divider_is_200_mhz_over_the_clock() {
        assert_eq!(clkdiv_for(8_000_000), Ok((25, 0)));
        assert_eq!(clkdiv_for(0), Err("PIO clock must be above zero".into()));
    }

    #[test]
    fn a_job_with_a_wrap_outside_its_program_is_refused() {
        let program = [0x1400u16];
        let job = PioJob {
            program: &program,
            wrap_bottom: 0,
            wrap_top: 1,
            clock_hz: 8_000_000,
            out_bits: 24,
        };
        assert_eq!(check(&job), Err(ErrorKind::InvalidInput));
    }
}
