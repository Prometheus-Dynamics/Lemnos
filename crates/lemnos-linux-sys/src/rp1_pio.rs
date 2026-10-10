//! The Raspberry Pi RP1 PIO character device (`/dev/pioN`, the `rp1_pio`
//! kernel driver): claiming state machines, loading instructions, pin and
//! GPIO-function setup, and DMA-backed FIFO transfers.
//!
//! Layouts and request numbers come from the kernel's
//! `include/uapi/misc/rp1_pio_if.h` (raspberrypi/linux `rpi-7.0.y`). The
//! `sm_config` words are the RP2040 PIO register layouts (`EXECCTRL`,
//! `SHIFTCTRL`, `PINCTRL`, `CLKDIV`), as in the kernel's `include/linux/pio_rp1.h`.
//! The driver forwards the PIO operations to the RP1 firmware, so each call is a
//! mailbox round trip; data moves through the DMA `XFER_DATA32` path.

use crate::ioctl::{ioctl_ptr, iow};
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsFd, BorrowedFd};
use std::path::Path;

const PIO_IOC_MAGIC: u8 = 102;

/// Instruction slots per PIO block (`RP1_PIO_INSTRUCTION_COUNT`).
pub const INSTRUCTION_COUNT: usize = 32;
/// State machines per PIO block (`RP1_PIO_SM_COUNT`).
pub const SM_COUNT: u16 = 4;
/// GPIO pins the PIO can address (`RP1_PIO_GPIO_COUNT`).
pub const GPIO_COUNT: u32 = 28;
/// GPIO function select for PIO (`RP1_GPIO_FUNC_PIO`).
pub const GPIO_FUNC_PIO: u16 = 7;
/// GPIO function select for the software-controlled (SIO) mode the kernel
/// gpio driver uses (`GPIO_FUNC_SYS_RIO` in `pio_rp1.h`).
pub const GPIO_FUNC_SIO: u16 = 5;
/// `RP1_PIO_ORIGIN_ANY`: let the driver pick the instruction offset.
pub const ORIGIN_ANY: u16 = u16::MAX;
/// `RP1_PIO_DIR_TO_SM` / `RP1_PIO_DIR_FROM_SM`.
pub const DIR_TO_SM: u16 = 0;
/// Data from a machine's RX FIFO to the host.
pub const DIR_FROM_SM: u16 = 1;

/// GPIO override values (`GPIO_OVERRIDE_*`, RP2040 `GPIOx_CTRL.OUTOVER` etc.).
pub mod override_value {
    /// The peripheral drives the output.
    pub const NORMAL: u16 = 0;
    /// The peripheral's output is inverted.
    pub const INVERT: u16 = 1;
    /// The output is forced low.
    pub const LOW: u16 = 2;
    /// The output is forced high.
    pub const HIGH: u16 = 3;
}

/// `rp1_pio_sm_config`: the four RP2040-layout register words of a state machine.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SmConfig {
    /// `CLKDIV`: integer part in 31:16, fraction in 15:8.
    pub clkdiv: u32,
    /// `EXECCTRL`.
    pub execctrl: u32,
    /// `SHIFTCTRL`.
    pub shiftctrl: u32,
    /// `PINCTRL`.
    pub pinctrl: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct AddProgramArgs {
    num_instrs: u16,
    origin: u16,
    instrs: [u16; INSTRUCTION_COUNT],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RemoveProgramArgs {
    num_instrs: u16,
    origin: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ClaimArgs {
    mask: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SmInitArgs {
    sm: u16,
    initial_pc: u16,
    config: SmConfig,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SmSetConfigArgs {
    sm: u16,
    rsvd: u16,
    config: SmConfig,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SmSetEnabledArgs {
    mask: u16,
    enable: u8,
    rsvd: u8,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SmSetClkdivArgs {
    sm: u16,
    div_int: u16,
    div_frac: u8,
    rsvd: u8,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SmSetPinsArgs {
    sm: u16,
    rsvd: u16,
    values: u32,
    mask: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SmClearFifosArgs {
    sm: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SmRestartArgs {
    mask: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct GpioInitArgs {
    gpio: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct GpioSetFunctionArgs {
    gpio: u16,
    func: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct GpioSetArgs {
    gpio: u16,
    value: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ConfigXfer32Args {
    sm: u16,
    dir: u16,
    buf_size: u32,
    buf_count: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct XferData32Args {
    sm: u16,
    dir: u16,
    data_bytes: u32,
    data: *mut u8,
}

const ADD_PROGRAM: u32 = iow::<AddProgramArgs>(PIO_IOC_MAGIC, 11);
const REMOVE_PROGRAM: u32 = iow::<RemoveProgramArgs>(PIO_IOC_MAGIC, 12);
const SM_CLAIM: u32 = iow::<ClaimArgs>(PIO_IOC_MAGIC, 20);
const SM_UNCLAIM: u32 = iow::<ClaimArgs>(PIO_IOC_MAGIC, 21);
const SM_INIT: u32 = iow::<SmInitArgs>(PIO_IOC_MAGIC, 30);
const SM_SET_CONFIG: u32 = iow::<SmSetConfigArgs>(PIO_IOC_MAGIC, 31);
const SM_CLEAR_FIFOS: u32 = iow::<SmClearFifosArgs>(PIO_IOC_MAGIC, 33);
const SM_SET_CLKDIV: u32 = iow::<SmSetClkdivArgs>(PIO_IOC_MAGIC, 34);
const SM_SET_PINS: u32 = iow::<SmSetPinsArgs>(PIO_IOC_MAGIC, 35);
const SM_SET_PINDIRS: u32 = iow::<SmSetPinsArgs>(PIO_IOC_MAGIC, 36);
const SM_SET_ENABLED: u32 = iow::<SmSetEnabledArgs>(PIO_IOC_MAGIC, 37);
const SM_RESTART: u32 = iow::<SmRestartArgs>(PIO_IOC_MAGIC, 38);
const SM_DRAIN_TX: u32 = iow::<SmClearFifosArgs>(PIO_IOC_MAGIC, 45);
const SM_CONFIG_XFER32: u32 = iow::<ConfigXfer32Args>(PIO_IOC_MAGIC, 3);
const SM_XFER_DATA32: u32 = iow::<XferData32Args>(PIO_IOC_MAGIC, 2);
const GPIO_INIT: u32 = iow::<GpioInitArgs>(PIO_IOC_MAGIC, 50);
const GPIO_SET_FUNCTION: u32 = iow::<GpioSetFunctionArgs>(PIO_IOC_MAGIC, 51);
const GPIO_SET_OUTOVER: u32 = iow::<GpioSetArgs>(PIO_IOC_MAGIC, 53);
const GPIO_SET_INPUT_ENABLED: u32 = iow::<GpioSetArgs>(PIO_IOC_MAGIC, 56);

/// An open RP1 PIO client (one file descriptor). Dropping it closes the
/// descriptor, and the driver then stops and releases everything it claimed.
#[derive(Debug)]
pub struct PioDevice {
    file: File,
}

impl PioDevice {
    /// Opens `path` (`/dev/pio0`). Needs read and write access to the node.
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        Ok(Self { file })
    }

    fn fd(&self) -> BorrowedFd<'_> {
        self.file.as_fd()
    }

    /// Loads `instrs` into instruction memory at `origin` (or any free place
    /// with [`ORIGIN_ANY`]); returns the offset the program was placed at.
    /// Fails with `EBUSY`/`ENOSPC`-style errors when the space is taken.
    pub fn add_program(&self, origin: u16, instrs: &[u16]) -> io::Result<u16> {
        if instrs.is_empty() || instrs.len() > INSTRUCTION_COUNT {
            return Err(io::Error::from_raw_os_error(libc::EINVAL));
        }
        let mut args = AddProgramArgs {
            num_instrs: instrs.len() as u16,
            origin,
            instrs: [0; INSTRUCTION_COUNT],
        };
        args.instrs[..instrs.len()].copy_from_slice(instrs);
        // SAFETY: ADD_PROGRAM reads one `rp1_pio_add_program_args` (`args`) and
        // returns the placed offset as the ioctl result.
        let ret = unsafe { ioctl_ptr(self.fd(), ADD_PROGRAM, &mut args) }?;
        Ok(ret as u16)
    }

    /// Removes a program previously added at `origin` with `len` instructions.
    pub fn remove_program(&self, origin: u16, len: u16) -> io::Result<()> {
        let mut args = RemoveProgramArgs {
            num_instrs: len,
            origin,
        };
        // SAFETY: REMOVE_PROGRAM reads one `rp1_pio_remove_program_args`.
        unsafe { ioctl_ptr(self.fd(), REMOVE_PROGRAM, &mut args) }?;
        Ok(())
    }

    /// Claims any free state machine and returns its number.
    pub fn claim_any_sm(&self) -> io::Result<u16> {
        let mut args = ClaimArgs { mask: 0 };
        // SAFETY: SM_CLAIM reads one `rp1_pio_sm_claim_args`; with a zero mask
        // the driver claims one free machine and returns its index.
        let ret = unsafe { ioctl_ptr(self.fd(), SM_CLAIM, &mut args) }?;
        Ok(ret as u16)
    }

    /// Releases the state machines in `mask` (bit n = machine n).
    pub fn unclaim_sms(&self, mask: u16) -> io::Result<()> {
        let mut args = ClaimArgs { mask };
        // SAFETY: SM_UNCLAIM reads one `rp1_pio_sm_claim_args`.
        unsafe { ioctl_ptr(self.fd(), SM_UNCLAIM, &mut args) }?;
        Ok(())
    }

    /// Sets the machine's configuration and program counter.
    pub fn sm_init(&self, sm: u16, initial_pc: u16, config: SmConfig) -> io::Result<()> {
        let mut args = SmInitArgs {
            sm,
            initial_pc,
            config,
        };
        // SAFETY: SM_INIT reads one `rp1_pio_sm_init_args`.
        unsafe { ioctl_ptr(self.fd(), SM_INIT, &mut args) }?;
        Ok(())
    }

    /// Replaces the machine's configuration without moving its program counter.
    pub fn sm_set_config(&self, sm: u16, config: SmConfig) -> io::Result<()> {
        let mut args = SmSetConfigArgs {
            sm,
            rsvd: 0,
            config,
        };
        // SAFETY: SM_SET_CONFIG reads one `rp1_pio_sm_set_config_args`.
        unsafe { ioctl_ptr(self.fd(), SM_SET_CONFIG, &mut args) }?;
        Ok(())
    }

    /// Sets the clock divider (`div_int` + `div_frac`/256).
    pub fn sm_set_clkdiv(&self, sm: u16, div_int: u16, div_frac: u8) -> io::Result<()> {
        let mut args = SmSetClkdivArgs {
            sm,
            div_int,
            div_frac,
            rsvd: 0,
        };
        // SAFETY: SM_SET_CLKDIV reads one `rp1_pio_sm_set_clkdiv_args`.
        unsafe { ioctl_ptr(self.fd(), SM_SET_CLKDIV, &mut args) }?;
        Ok(())
    }

    /// Drives the pins in `mask` (absolute GPIO bits) to `values` through the
    /// machine's SET group.
    pub fn sm_set_pins(&self, sm: u16, values: u32, mask: u32) -> io::Result<()> {
        let mut args = SmSetPinsArgs {
            sm,
            rsvd: 0,
            values,
            mask,
        };
        // SAFETY: SM_SET_PINS reads one `rp1_pio_sm_set_pins_args`.
        unsafe { ioctl_ptr(self.fd(), SM_SET_PINS, &mut args) }?;
        Ok(())
    }

    /// Sets the direction (1 = output) of the pins in `mask` (absolute GPIO bits).
    pub fn sm_set_pindirs(&self, sm: u16, dirs: u32, mask: u32) -> io::Result<()> {
        let mut args = SmSetPinsArgs {
            sm,
            rsvd: 0,
            values: dirs,
            mask,
        };
        // SAFETY: SM_SET_PINDIRS reads one `rp1_pio_sm_set_pindirs_args`, whose
        // layout matches `SmSetPinsArgs`.
        unsafe { ioctl_ptr(self.fd(), SM_SET_PINDIRS, &mut args) }?;
        Ok(())
    }

    /// Enables (`true`) or halts (`false`) the machines in `mask`.
    pub fn sm_set_enabled(&self, mask: u16, enable: bool) -> io::Result<()> {
        let mut args = SmSetEnabledArgs {
            mask,
            enable: u8::from(enable),
            rsvd: 0,
        };
        // SAFETY: SM_SET_ENABLED reads one `rp1_pio_sm_set_enabled_args`.
        unsafe { ioctl_ptr(self.fd(), SM_SET_ENABLED, &mut args) }?;
        Ok(())
    }

    /// Restarts the machines in `mask` (clears their shift and pin state).
    pub fn sm_restart(&self, mask: u16) -> io::Result<()> {
        let mut args = SmRestartArgs { mask };
        // SAFETY: SM_RESTART reads one `rp1_pio_sm_restart_args`.
        unsafe { ioctl_ptr(self.fd(), SM_RESTART, &mut args) }?;
        Ok(())
    }

    /// Empties both FIFOs of machine `sm`.
    pub fn sm_clear_fifos(&self, sm: u16) -> io::Result<()> {
        let mut args = SmClearFifosArgs { sm };
        // SAFETY: SM_CLEAR_FIFOS reads one `rp1_pio_sm_clear_fifos_args`.
        unsafe { ioctl_ptr(self.fd(), SM_CLEAR_FIFOS, &mut args) }?;
        Ok(())
    }

    /// Empties the TX FIFO of machine `sm`.
    pub fn sm_drain_tx(&self, sm: u16) -> io::Result<()> {
        let mut args = SmClearFifosArgs { sm };
        // SAFETY: SM_DRAIN_TX reads one `rp1_pio_sm_clear_fifos_args`.
        unsafe { ioctl_ptr(self.fd(), SM_DRAIN_TX, &mut args) }?;
        Ok(())
    }

    /// Requests the DMA channels for `sm` in `dir` with `buf_count` bounce
    /// buffers of `buf_size` bytes (a multiple of 4).
    pub fn config_xfer32(
        &self,
        sm: u16,
        dir: u16,
        buf_size: u32,
        buf_count: u32,
    ) -> io::Result<()> {
        let mut args = ConfigXfer32Args {
            sm,
            dir,
            buf_size,
            buf_count,
        };
        // SAFETY: SM_CONFIG_XFER32 reads one `rp1_pio_sm_config_xfer32_args`.
        unsafe { ioctl_ptr(self.fd(), SM_CONFIG_XFER32, &mut args) }?;
        Ok(())
    }

    /// Pushes `words` into machine `sm`'s TX FIFO by DMA. Blocks until the
    /// driver has handed every word to the FIFO, which needs the machine to
    /// consume them when there are more words than FIFO slots.
    pub fn xfer_to_sm(&self, sm: u16, words: &[u32]) -> io::Result<()> {
        let mut args = XferData32Args {
            sm,
            dir: DIR_TO_SM,
            data_bytes: (words.len() * 4) as u32,
            // The kernel only reads from this buffer for `DIR_TO_SM`.
            data: words.as_ptr() as *mut u8,
        };
        // SAFETY: SM_XFER_DATA32 reads `data_bytes` bytes from `data`, which is
        // the live slice `words` for the duration of the call.
        unsafe { ioctl_ptr(self.fd(), SM_XFER_DATA32, &mut args) }?;
        Ok(())
    }

    /// Reads `words.len()` words from machine `sm`'s RX FIFO by DMA. Blocks
    /// until the machine has produced them.
    pub fn xfer_from_sm(&self, sm: u16, words: &mut [u32]) -> io::Result<()> {
        let mut args = XferData32Args {
            sm,
            dir: DIR_FROM_SM,
            data_bytes: (words.len() * 4) as u32,
            data: words.as_mut_ptr().cast(),
        };
        // SAFETY: SM_XFER_DATA32 writes `data_bytes` bytes into `data`, which is
        // the live, exclusively borrowed slice `words` for the call.
        unsafe { ioctl_ptr(self.fd(), SM_XFER_DATA32, &mut args) }?;
        Ok(())
    }

    /// Hands GPIO `gpio` to the PIO block (`pio_gpio_init`: init, then function).
    pub fn gpio_init(&self, gpio: u16) -> io::Result<()> {
        let mut args = GpioInitArgs { gpio };
        // SAFETY: GPIO_INIT reads one `rp1_gpio_init_args`.
        unsafe { ioctl_ptr(self.fd(), GPIO_INIT, &mut args) }?;
        Ok(())
    }

    /// Selects the function of GPIO `gpio` (for example [`GPIO_FUNC_PIO`]).
    pub fn gpio_set_function(&self, gpio: u16, func: u16) -> io::Result<()> {
        let mut args = GpioSetFunctionArgs { gpio, func };
        // SAFETY: GPIO_SET_FUNCTION reads one `rp1_gpio_set_function_args`.
        unsafe { ioctl_ptr(self.fd(), GPIO_SET_FUNCTION, &mut args) }?;
        Ok(())
    }

    /// Forces the output value of GPIO `gpio` (an [`override_value`]).
    pub fn gpio_set_outover(&self, gpio: u16, value: u16) -> io::Result<()> {
        let mut args = GpioSetArgs { gpio, value };
        // SAFETY: GPIO_SET_OUTOVER reads one `rp1_gpio_set_args`.
        unsafe { ioctl_ptr(self.fd(), GPIO_SET_OUTOVER, &mut args) }?;
        Ok(())
    }

    /// Enables or disables the input buffer of GPIO `gpio`.
    pub fn gpio_set_input_enabled(&self, gpio: u16, enabled: bool) -> io::Result<()> {
        let mut args = GpioSetArgs {
            gpio,
            value: u16::from(enabled),
        };
        // SAFETY: GPIO_SET_INPUT_ENABLED reads one `rp1_gpio_set_args`.
        unsafe { ioctl_ptr(self.fd(), GPIO_SET_INPUT_ENABLED, &mut args) }?;
        Ok(())
    }
}

/// RP2040-layout register helpers (`pio_rp1.h`): the PINCTRL, SHIFTCTRL,
/// EXECCTRL and CLKDIV fields used by Lemnos.
pub mod regs {
    /// `PINCTRL.OUT_BASE` / `OUT_COUNT`.
    pub const fn pinctrl_out(base: u32, count: u32) -> u32 {
        (base & 0x1f) | ((count & 0x3f) << 20)
    }
    /// `PINCTRL.SET_BASE` / `SET_COUNT`.
    pub const fn pinctrl_set(base: u32, count: u32) -> u32 {
        ((base & 0x1f) << 5) | ((count & 0x7) << 26)
    }
    /// `PINCTRL.IN_BASE`.
    pub const fn pinctrl_in(base: u32) -> u32 {
        (base & 0x1f) << 15
    }
    /// `PINCTRL.SIDESET_BASE` / `SIDESET_COUNT` (bit count, not counting the enable bit).
    pub const fn pinctrl_sideset(base: u32, count: u32) -> u32 {
        ((base & 0x1f) << 10) | ((count & 0x7) << 29)
    }
    /// `EXECCTRL.SIDE_EN` (optional side-set) and `SIDE_PINDIR`.
    pub const SIDE_EN: u32 = 1 << 30;
    /// `EXECCTRL.SIDE_PINDIR`: side-set drives pin directions, not values.
    pub const SIDE_PINDIR: u32 = 1 << 29;
    /// `EXECCTRL.JMP_PIN`.
    pub const fn execctrl_jmp_pin(pin: u32) -> u32 {
        (pin & 0x1f) << 24
    }
    /// `EXECCTRL.WRAP_TOP` / `WRAP_BOTTOM`.
    pub const fn execctrl_wrap(bottom: u32, top: u32) -> u32 {
        ((top & 0x1f) << 12) | ((bottom & 0x1f) << 7)
    }
    /// `SHIFTCTRL` for the input side: shift direction (true = right), autopush, threshold (32 = 0).
    pub const fn shiftctrl_in(shift_right: bool, autopush: bool, threshold: u32) -> u32 {
        ((shift_right as u32) << 18) | ((autopush as u32) << 16) | ((threshold & 0x1f) << 20)
    }
    /// `SHIFTCTRL` for the output side: shift direction, autopull, threshold.
    /// The threshold is the number of output bits before the OSR counts as empty
    /// (`jmp !osre`), 32 encoded as 0.
    pub const fn shiftctrl_out(shift_right: bool, autopull: bool, threshold: u32) -> u32 {
        ((shift_right as u32) << 19) | ((autopull as u32) << 17) | ((threshold & 0x1f) << 25)
    }
    /// `CLKDIV` from integer and fractional (1/256) parts.
    pub const fn clkdiv(int: u32, frac: u32) -> u32 {
        ((int & 0xffff) << 16) | ((frac & 0xff) << 8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn struct_sizes_match_the_kernel_layout() {
        // Sizes of the `rp1_pio_if.h` structures on LP64 targets.
        assert_eq!(size_of::<SmConfig>(), 16);
        assert_eq!(size_of::<AddProgramArgs>(), 68);
        assert_eq!(size_of::<RemoveProgramArgs>(), 4);
        assert_eq!(size_of::<ClaimArgs>(), 2);
        assert_eq!(size_of::<SmInitArgs>(), 20);
        assert_eq!(size_of::<SmSetConfigArgs>(), 20);
        assert_eq!(size_of::<SmSetEnabledArgs>(), 4);
        assert_eq!(size_of::<SmSetClkdivArgs>(), 6);
        assert_eq!(size_of::<SmSetPinsArgs>(), 12);
        assert_eq!(size_of::<ConfigXfer32Args>(), 12);
        assert_eq!(size_of::<XferData32Args>(), 16);
        assert_eq!(size_of::<GpioSetArgs>(), 4);
        assert_eq!(size_of::<GpioSetFunctionArgs>(), 4);
    }

    #[test]
    fn request_numbers_match_the_kernel_header() {
        // _IOW(102, nr, struct) on aarch64 / x86_64 for the structures above.
        assert_eq!(SM_CONFIG_XFER32, 0x400c_6603); // _IOW(102, 3, 12 bytes)
        assert_eq!(SM_XFER_DATA32, 0x4010_6602); // _IOW(102, 2, 16 bytes)
        assert_eq!(SM_CLAIM, 0x4002_6614); // _IOW(102, 20, 2 bytes)
        assert_eq!(ADD_PROGRAM, 0x4044_660b); // _IOW(102, 11, 68 bytes)
    }

    #[test]
    fn register_fields_follow_the_rp2040_layout() {
        // OUT base 8, count 1 (SDA direction): bits 4:0 and 25:20.
        assert_eq!(regs::pinctrl_out(8, 1), 8 | (1 << 20));
        // SIDESET base 7, one bit: bits 14:10 and 31:29.
        assert_eq!(regs::pinctrl_sideset(7, 1), (7 << 10) | (1 << 29));
        // Output threshold 13 sits in bits 29:25.
        assert_eq!(regs::shiftctrl_out(false, false, 13), 13 << 25);
        assert_eq!(regs::clkdiv(20, 213), (20 << 16) | (213 << 8));
    }
}
