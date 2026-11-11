//! The RP1 PIO block (the `rp1_pio` kernel driver): claiming state machines, loading
//! instructions, pin and GPIO-function setup, and DMA-backed FIFO transfers. Two routes reach
//! it with the same operations: the driver's own node (`/dev/pioN`), and the Lemnos module's
//! client of that driver on a claim of `pio-0` (`LEMNOS_IOC_PIO_*` in `kernel/lemnos.h`), which
//! a holder that dies lets go of.
//!
//! Layouts and request numbers follow the kernel's `rp1_pio_if.h` as earlier
//! ports recorded them. The `sm_config` words are the RP2040 PIO register
//! layouts (`EXECCTRL`, `SHIFTCTRL`, `PINCTRL`, `CLKDIV`). The driver forwards the
//! PIO operations to the RP1 firmware, so each call is a mailbox round trip; data
//! moves through the DMA `XFER_DATA32` path.
//!
//! Every method checks its arguments before the ioctl is issued: state machine
//! indices, instruction ranges, GPIO numbers, bit masks and DMA lengths. A
//! refused argument returns `EINVAL` without touching the device. The bounds are
//! this crate's; they are not read back from the kernel.

use crate::ioctl::{Ioctl, Pod, ioctl_ptr, iow, iowr, pod};
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError, Weak};

const PIO_IOC_MAGIC: u8 = 102;

/// Instruction slots per PIO block (`RP1_PIO_INSTRUCTION_COUNT`).
pub const INSTRUCTION_COUNT: usize = 32;
/// State machines per PIO block.
pub const SM_COUNT: u16 = 4;
/// GPIO pins the PIO can address (`RP1_PIO_GPIO_COUNT`).
pub const GPIO_COUNT: u32 = 28;
/// GPIO function select for PIO (`RP1_GPIO_FUNC_PIO`).
pub const GPIO_FUNC_PIO: u16 = 7;
/// GPIO function select for the software-controlled (SIO) mode the kernel
/// gpio driver uses (`GPIO_FUNC_SYS_RIO` in `pio_rp1.h`).
pub const GPIO_FUNC_SIO: u16 = 5;
/// The largest GPIO function select (a 5-bit field).
pub const GPIO_FUNC_MAX: u16 = 31;
/// `RP1_PIO_DIR_TO_SM` / `RP1_PIO_DIR_FROM_SM`.
pub const DIR_TO_SM: u16 = 0;
/// Data from a machine's RX FIFO to the host.
pub const DIR_FROM_SM: u16 = 1;
/// The most words one DMA transfer moves (256 KiB).
pub const MAX_XFER_WORDS: usize = 1 << 16;
/// Largest DMA bounce buffer this crate asks for, in bytes.
pub const MAX_BUF_SIZE: u32 = 4096;
/// Most DMA bounce buffers this crate asks for.
pub const MAX_BUF_COUNT: u32 = 64;

/// Bits of the state machine mask (one per machine).
const SM_MASK: u16 = (1 << SM_COUNT) - 1;
/// Bits of the GPIO mask (one per pin).
const GPIO_MASK: u32 = (1 << GPIO_COUNT) - 1;

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

pod!(
    SmConfig,
    AddProgramArgs,
    RemoveProgramArgs,
    ClaimArgs,
    SmInitArgs,
    SmSetConfigArgs,
    SmSetEnabledArgs,
    SmSetClkdivArgs,
    SmSetPinsArgs,
    SmClearFifosArgs,
    SmRestartArgs,
    GpioInitArgs,
    GpioSetFunctionArgs,
    GpioSetArgs,
    ConfigXfer32Args,
    PioOpenRaw,
    PioConfigXferRaw,
);

/// One PIO operation, two ways: the stock node's request and the module's operation number
/// (`enum lemnos_pio_op` in `kernel/lemnos.h`), with the same argument structure.
struct PioOp<T> {
    stock: Ioctl<T>,
    module: u32,
}

impl<T> Clone for PioOp<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for PioOp<T> {}

impl<T: Pod> PioOp<T> {
    const fn new(nr: u8, name: &'static str, module: u32) -> Self {
        Self {
            stock: Ioctl::new(iow::<T>(PIO_IOC_MAGIC, nr), name),
            module,
        }
    }
}

const ADD_PROGRAM: PioOp<AddProgramArgs> = PioOp::new(11, "PIO_IOC_ADD_PROGRAM", 0);
const REMOVE_PROGRAM: PioOp<RemoveProgramArgs> = PioOp::new(12, "PIO_IOC_REMOVE_PROGRAM", 1);
const SM_CLAIM: PioOp<ClaimArgs> = PioOp::new(20, "PIO_IOC_SM_CLAIM", 2);
const SM_UNCLAIM: PioOp<ClaimArgs> = PioOp::new(21, "PIO_IOC_SM_UNCLAIM", 3);
const SM_INIT: PioOp<SmInitArgs> = PioOp::new(30, "PIO_IOC_SM_INIT", 4);
const SM_SET_CONFIG: PioOp<SmSetConfigArgs> = PioOp::new(31, "PIO_IOC_SM_SET_CONFIG", 5);
const SM_CLEAR_FIFOS: PioOp<SmClearFifosArgs> = PioOp::new(33, "PIO_IOC_SM_CLEAR_FIFOS", 7);
const SM_SET_CLKDIV: PioOp<SmSetClkdivArgs> = PioOp::new(34, "PIO_IOC_SM_SET_CLKDIV", 8);
const SM_SET_PINS: PioOp<SmSetPinsArgs> = PioOp::new(35, "PIO_IOC_SM_SET_PINS", 9);
const SM_SET_PINDIRS: PioOp<SmSetPinsArgs> = PioOp::new(36, "PIO_IOC_SM_SET_PINDIRS", 10);
const SM_SET_ENABLED: PioOp<SmSetEnabledArgs> = PioOp::new(37, "PIO_IOC_SM_SET_ENABLED", 11);
const SM_RESTART: PioOp<SmRestartArgs> = PioOp::new(38, "PIO_IOC_SM_RESTART", 12);
const SM_DRAIN_TX: PioOp<SmClearFifosArgs> = PioOp::new(45, "PIO_IOC_SM_DRAIN_TX", 16);
const GPIO_INIT: PioOp<GpioInitArgs> = PioOp::new(50, "PIO_IOC_GPIO_INIT", 18);
const GPIO_SET_FUNCTION: PioOp<GpioSetFunctionArgs> =
    PioOp::new(51, "PIO_IOC_GPIO_SET_FUNCTION", 19);
const GPIO_SET_OUTOVER: PioOp<GpioSetArgs> = PioOp::new(53, "PIO_IOC_GPIO_SET_OUTOVER", 21);
const GPIO_SET_INPUT_ENABLED: PioOp<GpioSetArgs> =
    PioOp::new(56, "PIO_IOC_GPIO_SET_INPUT_ENABLED", 22);
const SM_CONFIG_XFER32: Ioctl<ConfigXfer32Args> = Ioctl::new(
    iow::<ConfigXfer32Args>(PIO_IOC_MAGIC, 3),
    "PIO_IOC_SM_CONFIG_XFER32",
);
// Carries a pointer the kernel follows, so it isn't an `Ioctl`.
const SM_XFER_DATA32: u32 = iow::<XferData32Args>(PIO_IOC_MAGIC, 2);

/// The module's `'L'` requests for PIO (`kernel/lemnos.h`).
const LEMNOS: u8 = b'L';
const LEMNOS_PIO_OPEN: Ioctl<PioOpenRaw> =
    Ioctl::new(iow::<PioOpenRaw>(LEMNOS, 0x90), "LEMNOS_IOC_PIO_OPEN");
const LEMNOS_PIO_CONFIG_XFER: Ioctl<PioConfigXferRaw> = Ioctl::new(
    iow::<PioConfigXferRaw>(LEMNOS, 0x92),
    "LEMNOS_IOC_PIO_CONFIG_XFER",
);
// Carries a pointer the kernel follows, so it isn't an `Ioctl`.
const LEMNOS_PIO_XFER: u32 = iow::<PioXferRaw>(LEMNOS, 0x93);
/// `LEMNOS_PIO_ARGS`: the bytes of arguments one `LEMNOS_IOC_PIO_CALL` carries.
const LEMNOS_PIO_ARGS: usize = 72;
/// The claim the module's PIO belongs to.
pub const MODULE_RESOURCE: &str = "pio-0";

/// `struct lemnos_pio_open`.
#[repr(C)]
#[derive(Clone, Copy)]
struct PioOpenRaw {
    claim: i32,
}

/// `struct lemnos_pio_call`, with its argument bytes seen as the operation's own structure.
#[repr(C)]
#[derive(Clone, Copy)]
struct PioCallRaw<T: Copy> {
    claim: i32,
    op: u32,
    result: i64,
    args: PioArgs<T>,
}

/// The `args` bytes of `struct lemnos_pio_call`: the operation's structure, zero after it.
#[repr(C)]
#[derive(Clone, Copy)]
union PioArgs<T: Copy> {
    args: T,
    bytes: [u8; LEMNOS_PIO_ARGS],
}

// SAFETY: integers and a union of a `Pod` structure with bytes: every bit pattern is a value.
unsafe impl<T: Pod> Pod for PioCallRaw<T> {}

/// `struct lemnos_pio_config_xfer`.
#[repr(C)]
#[derive(Clone, Copy)]
struct PioConfigXferRaw {
    claim: i32,
    sm: u32,
    dir: u32,
    buf_size: u32,
    buf_count: u32,
    reserved: u32,
}

/// `struct lemnos_pio_xfer`.
#[repr(C)]
#[derive(Clone, Copy)]
struct PioXferRaw {
    claim: i32,
    sm: u32,
    dir: u32,
    len: u32,
    data: u64,
    timeout_ms: u32,
    reserved: u32,
}

/// The error for an argument outside what this crate lets through to the device.
fn refused() -> io::Error {
    io::Error::from_raw_os_error(libc::EINVAL)
}

fn check_sm(sm: u16) -> io::Result<()> {
    if sm < SM_COUNT {
        Ok(())
    } else {
        Err(refused())
    }
}

fn check_sm_mask(mask: u16) -> io::Result<()> {
    if mask & !SM_MASK == 0 {
        Ok(())
    } else {
        Err(refused())
    }
}

fn check_gpio(gpio: u16) -> io::Result<()> {
    if u32::from(gpio) < GPIO_COUNT {
        Ok(())
    } else {
        Err(refused())
    }
}

/// `values` and `mask` name GPIO bits only, and `values` sets bits `mask` covers.
fn check_pin_bits(values: u32, mask: u32) -> io::Result<()> {
    if mask & !GPIO_MASK == 0 && values & !mask == 0 {
        Ok(())
    } else {
        Err(refused())
    }
}

/// A program at `origin` of `len` slots must lie inside the block, or `origin`
/// must be `u16::MAX` (any free place).
fn check_origin(origin: u16, len: usize) -> io::Result<()> {
    if origin == u16::MAX {
        return Ok(());
    }
    if usize::from(origin) + len <= INSTRUCTION_COUNT {
        Ok(())
    } else {
        Err(refused())
    }
}

/// The Lemnos module's PIO client: a claim on `pio-0` with a client of the kernel's PIO driver
/// bound to it. Closing the claim ends the client, which frees its machines and programs.
#[derive(Debug)]
struct ModuleClient {
    node: Arc<OwnedFd>,
    claim: OwnedFd,
}

impl ModuleClient {
    fn claim_fd(&self) -> i32 {
        self.claim.as_raw_fd()
    }

    /// Runs one operation through `LEMNOS_IOC_PIO_CALL`. The operation's own return value comes
    /// back in `result`: a negative one is its error.
    fn call<T: Pod>(&self, op: u32, args: &mut T) -> io::Result<libc::c_int> {
        const { assert!(size_of::<T>() <= LEMNOS_PIO_ARGS) };
        let request: Ioctl<PioCallRaw<T>> =
            Ioctl::new(iowr::<PioCallRaw<T>>(LEMNOS, 0x91), "LEMNOS_IOC_PIO_CALL");
        let mut call = PioCallRaw {
            claim: self.claim_fd(),
            op,
            result: 0,
            args: PioArgs {
                bytes: [0; LEMNOS_PIO_ARGS],
            },
        };
        call.args.args = *args;
        request.call(self.node.as_fd(), &mut call)?;
        // SAFETY: `args` is the field written above, and the kernel writes only an operation's
        // own structure back into it; `T` is `Pod`, so any bytes it holds are a value.
        *args = unsafe { call.args.args };
        result_of(call.result)
    }
}

/// An operation's return value as the module reports it: a negative errno, or the value.
fn result_of(result: i64) -> io::Result<libc::c_int> {
    if result < 0 {
        let errno = i32::try_from(-result).unwrap_or(libc::EIO);
        return Err(io::Error::from_raw_os_error(errno));
    }
    libc::c_int::try_from(result).map_err(|_| io::Error::from_raw_os_error(libc::EOVERFLOW))
}

/// How a [`PioDevice`] reaches the PIO block.
#[derive(Debug)]
enum Route {
    /// The stock driver's node, `/dev/pioN`.
    Node(File),
    /// The Lemnos module, shared by every bus of this process.
    Module(Arc<ModuleClient>),
}

/// An open RP1 PIO client: either the stock driver's node or the Lemnos module's client on a
/// claim of `pio-0`. The operations, and the checks before them, are the same both ways.
/// Dropping the last handle on a client ends it, and the driver then stops and releases
/// everything it claimed.
#[derive(Debug)]
pub struct PioDevice {
    route: Route,
}

impl PioDevice {
    /// Opens `path` (`/dev/pio0`). Needs read and write access to the node.
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        Ok(Self {
            route: Route::Node(file),
        })
    }

    /// The PIO block through the Lemnos module's control node `node`: claims `pio-0` and binds a
    /// client to it (`LEMNOS_IOC_PIO_OPEN`). The claim is exclusive system-wide, so a process
    /// has one: every call with the same open node while a handle from an earlier call lives
    /// shares that client. A kernel module without PIO answers `ENOTTY`; someone else holding
    /// the claim, `EBUSY`.
    pub fn through_module(node: &Arc<OwnedFd>) -> io::Result<Self> {
        type Shared = Mutex<Option<(Weak<OwnedFd>, Weak<ModuleClient>)>>;
        static SHARED: Shared = Mutex::new(None);
        let mut shared = SHARED.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((at, client)) = shared.as_ref()
            && at.upgrade().is_some_and(|at| Arc::ptr_eq(&at, node))
            && let Some(client) = client.upgrade()
        {
            return Ok(Self {
                route: Route::Module(client),
            });
        }
        let claim = crate::module::claim(node.as_fd(), MODULE_RESOURCE)?;
        let mut open = PioOpenRaw {
            claim: claim.as_raw_fd(),
        };
        LEMNOS_PIO_OPEN.call(node.as_fd(), &mut open)?;
        let client = Arc::new(ModuleClient {
            node: Arc::clone(node),
            claim,
        });
        *shared = Some((Arc::downgrade(node), Arc::downgrade(&client)));
        Ok(Self {
            route: Route::Module(client),
        })
    }

    /// Whether this client is the Lemnos module's.
    pub fn is_module(&self) -> bool {
        matches!(self.route, Route::Module(_))
    }

    /// Runs one operation on whichever route this client is.
    fn run<T: Pod>(&self, op: PioOp<T>, args: &mut T) -> io::Result<libc::c_int> {
        match &self.route {
            Route::Node(file) => op.stock.call(file.as_fd(), args),
            Route::Module(client) => client.call(op.module, args),
        }
    }

    /// Loads `instrs` into instruction memory at `origin` (or any free place
    /// with `u16::MAX`); returns the offset the program was placed at.
    /// Fails with `EBUSY`/`ENOSPC`-style errors when the space is taken.
    pub fn add_program(&self, origin: u16, instrs: &[u16]) -> io::Result<u16> {
        if instrs.is_empty() || instrs.len() > INSTRUCTION_COUNT {
            return Err(refused());
        }
        check_origin(origin, instrs.len())?;
        let mut args = AddProgramArgs {
            num_instrs: instrs.len() as u16,
            origin,
            instrs: [0; INSTRUCTION_COUNT],
        };
        args.instrs[..instrs.len()].copy_from_slice(instrs);
        // The driver returns the placed offset as the result.
        let ret = self.run(ADD_PROGRAM, &mut args)?;
        Ok(ret as u16)
    }

    /// Removes a program previously added at `origin` with `len` instructions.
    pub fn remove_program(&self, origin: u16, len: u16) -> io::Result<()> {
        if len == 0 || usize::from(len) > INSTRUCTION_COUNT {
            return Err(refused());
        }
        check_origin(origin, usize::from(len))?;
        let mut args = RemoveProgramArgs {
            num_instrs: len,
            origin,
        };
        self.run(REMOVE_PROGRAM, &mut args)?;
        Ok(())
    }

    /// Claims any free state machine and returns its number.
    pub fn claim_any_sm(&self) -> io::Result<u16> {
        // With a zero mask the driver claims one free machine and returns its index.
        let mut args = ClaimArgs { mask: 0 };
        let ret = self.run(SM_CLAIM, &mut args)?;
        Ok(ret as u16)
    }

    /// Releases the state machines in `mask` (bit n = machine n).
    pub fn unclaim_sms(&self, mask: u16) -> io::Result<()> {
        check_sm_mask(mask)?;
        let mut args = ClaimArgs { mask };
        self.run(SM_UNCLAIM, &mut args)?;
        Ok(())
    }

    /// Sets the machine's configuration and program counter.
    pub fn sm_init(&self, sm: u16, initial_pc: u16, config: SmConfig) -> io::Result<()> {
        check_sm(sm)?;
        if usize::from(initial_pc) >= INSTRUCTION_COUNT {
            return Err(refused());
        }
        let mut args = SmInitArgs {
            sm,
            initial_pc,
            config,
        };
        self.run(SM_INIT, &mut args)?;
        Ok(())
    }

    /// Replaces the machine's configuration without moving its program counter.
    pub fn sm_set_config(&self, sm: u16, config: SmConfig) -> io::Result<()> {
        check_sm(sm)?;
        let mut args = SmSetConfigArgs {
            sm,
            rsvd: 0,
            config,
        };
        self.run(SM_SET_CONFIG, &mut args)?;
        Ok(())
    }

    /// Sets the clock divider (`div_int` + `div_frac`/256).
    pub fn sm_set_clkdiv(&self, sm: u16, div_int: u16, div_frac: u8) -> io::Result<()> {
        check_sm(sm)?;
        let mut args = SmSetClkdivArgs {
            sm,
            div_int,
            div_frac,
            rsvd: 0,
        };
        self.run(SM_SET_CLKDIV, &mut args)?;
        Ok(())
    }

    /// Drives the pins in `mask` (absolute GPIO bits) to `values` through the
    /// machine's SET group.
    pub fn sm_set_pins(&self, sm: u16, values: u32, mask: u32) -> io::Result<()> {
        check_sm(sm)?;
        check_pin_bits(values, mask)?;
        let mut args = SmSetPinsArgs {
            sm,
            rsvd: 0,
            values,
            mask,
        };
        self.run(SM_SET_PINS, &mut args)?;
        Ok(())
    }

    /// Sets the direction (1 = output) of the pins in `mask` (absolute GPIO bits).
    pub fn sm_set_pindirs(&self, sm: u16, dirs: u32, mask: u32) -> io::Result<()> {
        check_sm(sm)?;
        check_pin_bits(dirs, mask)?;
        // `rp1_pio_sm_set_pindirs_args` has the layout of `SmSetPinsArgs`.
        let mut args = SmSetPinsArgs {
            sm,
            rsvd: 0,
            values: dirs,
            mask,
        };
        self.run(SM_SET_PINDIRS, &mut args)?;
        Ok(())
    }

    /// Enables (`true`) or halts (`false`) the machines in `mask`.
    pub fn sm_set_enabled(&self, mask: u16, enable: bool) -> io::Result<()> {
        check_sm_mask(mask)?;
        let mut args = SmSetEnabledArgs {
            mask,
            enable: u8::from(enable),
            rsvd: 0,
        };
        self.run(SM_SET_ENABLED, &mut args)?;
        Ok(())
    }

    /// Restarts the machines in `mask` (clears their shift and pin state).
    pub fn sm_restart(&self, mask: u16) -> io::Result<()> {
        check_sm_mask(mask)?;
        let mut args = SmRestartArgs { mask };
        self.run(SM_RESTART, &mut args)?;
        Ok(())
    }

    /// Empties both FIFOs of machine `sm`.
    pub fn sm_clear_fifos(&self, sm: u16) -> io::Result<()> {
        check_sm(sm)?;
        let mut args = SmClearFifosArgs { sm };
        self.run(SM_CLEAR_FIFOS, &mut args)?;
        Ok(())
    }

    /// Empties the TX FIFO of machine `sm`.
    pub fn sm_drain_tx(&self, sm: u16) -> io::Result<()> {
        check_sm(sm)?;
        let mut args = SmClearFifosArgs { sm };
        self.run(SM_DRAIN_TX, &mut args)?;
        Ok(())
    }

    /// Requests the DMA channels for `sm` in `dir` with `buf_count` bounce
    /// buffers of `buf_size` bytes (a multiple of 4, at most [`MAX_BUF_SIZE`]).
    /// Configuring a channel again stops anything still running on it, and on the module's
    /// route also clears a channel a timed-out transfer left refusing.
    pub fn config_xfer32(
        &self,
        sm: u16,
        dir: u16,
        buf_size: u32,
        buf_count: u32,
    ) -> io::Result<()> {
        check_sm(sm)?;
        check_dir(dir)?;
        if buf_size == 0 || buf_size > MAX_BUF_SIZE || !buf_size.is_multiple_of(4) {
            return Err(refused());
        }
        if buf_count == 0 || buf_count > MAX_BUF_COUNT {
            return Err(refused());
        }
        match &self.route {
            Route::Node(file) => {
                let mut args = ConfigXfer32Args {
                    sm,
                    dir,
                    buf_size,
                    buf_count,
                };
                SM_CONFIG_XFER32.call(file.as_fd(), &mut args)?;
            }
            Route::Module(client) => {
                let mut args = PioConfigXferRaw {
                    claim: client.claim_fd(),
                    sm: u32::from(sm),
                    dir: u32::from(dir),
                    buf_size,
                    buf_count,
                    reserved: 0,
                };
                LEMNOS_PIO_CONFIG_XFER.call(client.node.as_fd(), &mut args)?;
            }
        }
        Ok(())
    }

    /// Moves `bytes` bytes at `data` between this process and machine `sm` by DMA.
    ///
    /// # Safety
    ///
    /// `data` must be valid for `bytes` bytes of reads (`DIR_TO_SM`) or of writes
    /// (`DIR_FROM_SM`), and not otherwise accessed, for the duration of the call.
    unsafe fn xfer(&self, sm: u16, dir: u16, data: *mut u8, bytes: u32) -> io::Result<()> {
        match &self.route {
            Route::Node(file) => {
                let mut args = XferData32Args {
                    sm,
                    dir,
                    data_bytes: bytes,
                    data,
                };
                // SAFETY: SM_XFER_DATA32 moves `data_bytes` bytes at `data`, which the caller
                // guarantees for the duration of the call.
                unsafe { ioctl_ptr(file.as_fd(), SM_XFER_DATA32, &mut args) }?;
            }
            Route::Module(client) => {
                let mut args = PioXferRaw {
                    claim: client.claim_fd(),
                    sm: u32::from(sm),
                    dir: u32::from(dir),
                    len: bytes,
                    data: data as u64,
                    // The module's default: one second.
                    timeout_ms: 0,
                    reserved: 0,
                };
                // SAFETY: LEMNOS_IOC_PIO_XFER moves `len` bytes at `data`, which the caller
                // guarantees for the duration of the call; the module copies through its own
                // buffers and has finished with `data` when it returns.
                unsafe { ioctl_ptr(client.node.as_fd(), LEMNOS_PIO_XFER, &mut args) }?;
            }
        }
        Ok(())
    }

    /// Pushes `words` into machine `sm`'s TX FIFO by DMA. Blocks until the
    /// driver has handed every word to the FIFO, which needs the machine to
    /// consume them when there are more words than FIFO slots.
    pub fn xfer_to_sm(&self, sm: u16, words: &[u32]) -> io::Result<()> {
        check_sm(sm)?;
        check_words(words.len())?;
        // Checked above: at most `MAX_XFER_WORDS` words, so this fits.
        let bytes = (words.len() * 4) as u32;
        // SAFETY: `words` is live for the call, and the kernel only reads from it for
        // `DIR_TO_SM`.
        unsafe { self.xfer(sm, DIR_TO_SM, words.as_ptr() as *mut u8, bytes) }
    }

    /// Reads `words.len()` words from machine `sm`'s RX FIFO by DMA. Blocks
    /// until the machine has produced them.
    pub fn xfer_from_sm(&self, sm: u16, words: &mut [u32]) -> io::Result<()> {
        check_sm(sm)?;
        check_words(words.len())?;
        // Checked above: at most `MAX_XFER_WORDS` words, so this fits.
        let bytes = (words.len() * 4) as u32;
        // SAFETY: `words` is a live, exclusively borrowed slice of `bytes` bytes for the call.
        unsafe { self.xfer(sm, DIR_FROM_SM, words.as_mut_ptr().cast(), bytes) }
    }

    /// Hands GPIO `gpio` to the PIO block (`pio_gpio_init`: init, then function).
    pub fn gpio_init(&self, gpio: u16) -> io::Result<()> {
        check_gpio(gpio)?;
        let mut args = GpioInitArgs { gpio };
        self.run(GPIO_INIT, &mut args)?;
        Ok(())
    }

    /// Selects the function of GPIO `gpio` (for example [`GPIO_FUNC_PIO`]).
    pub fn gpio_set_function(&self, gpio: u16, func: u16) -> io::Result<()> {
        check_gpio(gpio)?;
        if func > GPIO_FUNC_MAX {
            return Err(refused());
        }
        let mut args = GpioSetFunctionArgs { gpio, func };
        self.run(GPIO_SET_FUNCTION, &mut args)?;
        Ok(())
    }

    /// Forces the output value of GPIO `gpio` (an [`override_value`]).
    pub fn gpio_set_outover(&self, gpio: u16, value: u16) -> io::Result<()> {
        check_gpio(gpio)?;
        if value > override_value::HIGH {
            return Err(refused());
        }
        let mut args = GpioSetArgs { gpio, value };
        self.run(GPIO_SET_OUTOVER, &mut args)?;
        Ok(())
    }

    /// Enables or disables the input buffer of GPIO `gpio`.
    pub fn gpio_set_input_enabled(&self, gpio: u16, enabled: bool) -> io::Result<()> {
        check_gpio(gpio)?;
        let mut args = GpioSetArgs {
            gpio,
            value: u16::from(enabled),
        };
        self.run(GPIO_SET_INPUT_ENABLED, &mut args)?;
        Ok(())
    }
}

fn check_dir(dir: u16) -> io::Result<()> {
    if dir == DIR_TO_SM || dir == DIR_FROM_SM {
        Ok(())
    } else {
        Err(refused())
    }
}

/// A DMA length: at least one word, at most [`MAX_XFER_WORDS`].
fn check_words(len: usize) -> io::Result<()> {
    if (1..=MAX_XFER_WORDS).contains(&len) {
        Ok(())
    } else {
        Err(refused())
    }
}

/// The instruction slots a program of `len` instructions at `origin` occupies,
/// as a bitmask (bit n = slot n).
pub const fn slot_mask(origin: u16, len: usize) -> u32 {
    let span = if len >= 32 {
        u32::MAX
    } else {
        (1u32 << len) - 1
    };
    span << origin
}

/// The lowest origin where `len` consecutive slots are free in `occupied`
/// (a [`slot_mask`] of the resident programs), or `None` when no run fits.
/// Tries origins from 0 upward, the order the PIO I2C loader uses.
pub fn first_fit(occupied: u32, len: usize) -> Option<u16> {
    if len == 0 || len > INSTRUCTION_COUNT {
        return None;
    }
    (0..=(INSTRUCTION_COUNT - len) as u16).find(|&origin| slot_mask(origin, len) & occupied == 0)
}

/// RP2040-layout register helpers (`pio_rp1.h`): the PINCTRL, SHIFTCTRL,
/// EXECCTRL and CLKDIV fields the PIO I2C bus uses.
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

    /// `/dev/null` takes every request as "not a terminal", so a refused
    /// argument (`EINVAL`) and an argument that reached the device (`ENOTTY`)
    /// tell apart without a board.
    fn device() -> PioDevice {
        PioDevice::open(Path::new("/dev/null")).unwrap()
    }

    fn errno(result: io::Result<impl Sized>) -> Option<i32> {
        result.err().and_then(|e| e.raw_os_error())
    }

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
        assert_eq!(SM_CONFIG_XFER32.nr(), 0x400c_6603); // _IOW(102, 3, 12 bytes)
        assert_eq!(SM_XFER_DATA32, 0x4010_6602); // _IOW(102, 2, 16 bytes)
        assert_eq!(SM_CLAIM.stock.nr(), 0x4002_6614); // _IOW(102, 20, 2 bytes)
        assert_eq!(ADD_PROGRAM.stock.nr(), 0x4044_660b); // _IOW(102, 11, 68 bytes)
    }

    #[test]
    fn the_modules_structures_are_the_headers() {
        use std::mem::offset_of;
        assert_eq!(size_of::<PioOpenRaw>(), 4);
        assert_eq!(size_of::<PioCallRaw<AddProgramArgs>>(), 88);
        assert_eq!(size_of::<PioCallRaw<ClaimArgs>>(), 88);
        assert_eq!(size_of::<PioCallRaw<SmInitArgs>>(), 88);
        assert_eq!(offset_of!(PioCallRaw<ClaimArgs>, result), 8);
        assert_eq!(offset_of!(PioCallRaw<ClaimArgs>, args), 16);
        assert_eq!(size_of::<PioConfigXferRaw>(), 24);
        assert_eq!(size_of::<PioXferRaw>(), 32);
        assert_eq!(offset_of!(PioXferRaw, data), 16);
        assert_eq!(offset_of!(PioXferRaw, timeout_ms), 24);
        // _IOW('L', 0x90, 4), _IOWR('L', 0x91, 88), _IOW('L', 0x92, 24), _IOW('L', 0x93, 32).
        assert_eq!(LEMNOS_PIO_OPEN.nr(), 0x4004_4c90);
        assert_eq!(iowr::<PioCallRaw<ClaimArgs>>(LEMNOS, 0x91), 0xc058_4c91);
        assert_eq!(LEMNOS_PIO_CONFIG_XFER.nr(), 0x4018_4c92);
        assert_eq!(LEMNOS_PIO_XFER, 0x4020_4c93);
        let header = include_str!("../../../kernel/lemnos.h");
        assert!(header.contains("#define LEMNOS_PIO_ARGS 72"));
        for (name, nr) in [
            ("LEMNOS_IOC_PIO_OPEN   _IOW('L', 0x90", 0x90),
            ("LEMNOS_IOC_PIO_CALL   _IOWR('L', 0x91", 0x91),
            ("LEMNOS_IOC_PIO_CONFIG_XFER _IOW('L', 0x92", 0x92),
            ("LEMNOS_IOC_PIO_XFER   _IOW('L', 0x93", 0x93),
        ] {
            assert!(header.contains(name), "{name} ({nr:#x})");
        }
    }

    /// Each operation's number is its place in `enum lemnos_pio_op`, read from the header itself.
    #[test]
    fn module_operation_numbers_are_the_headers() {
        let header = include_str!("../../../kernel/lemnos.h");
        let body = header
            .split("enum lemnos_pio_op {")
            .nth(1)
            .and_then(|rest| rest.split("};").next())
            .unwrap();
        let names: Vec<&str> = body
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .collect();
        let place = |name: &str| names.iter().position(|n| *n == name).unwrap() as u32;
        let ops = [
            ("LEMNOS_PIO_ADD_PROGRAM", ADD_PROGRAM.module),
            ("LEMNOS_PIO_REMOVE_PROGRAM", REMOVE_PROGRAM.module),
            ("LEMNOS_PIO_SM_CLAIM", SM_CLAIM.module),
            ("LEMNOS_PIO_SM_UNCLAIM", SM_UNCLAIM.module),
            ("LEMNOS_PIO_SM_INIT", SM_INIT.module),
            ("LEMNOS_PIO_SM_SET_CONFIG", SM_SET_CONFIG.module),
            ("LEMNOS_PIO_SM_CLEAR_FIFOS", SM_CLEAR_FIFOS.module),
            ("LEMNOS_PIO_SM_SET_CLKDIV", SM_SET_CLKDIV.module),
            ("LEMNOS_PIO_SM_SET_PINS", SM_SET_PINS.module),
            ("LEMNOS_PIO_SM_SET_PINDIRS", SM_SET_PINDIRS.module),
            ("LEMNOS_PIO_SM_SET_ENABLED", SM_SET_ENABLED.module),
            ("LEMNOS_PIO_SM_RESTART", SM_RESTART.module),
            ("LEMNOS_PIO_SM_DRAIN_TX", SM_DRAIN_TX.module),
            ("LEMNOS_PIO_GPIO_INIT", GPIO_INIT.module),
            ("LEMNOS_PIO_GPIO_SET_FUNCTION", GPIO_SET_FUNCTION.module),
            ("LEMNOS_PIO_GPIO_SET_OUTOVER", GPIO_SET_OUTOVER.module),
            (
                "LEMNOS_PIO_GPIO_SET_INPUT_ENABLED",
                GPIO_SET_INPUT_ENABLED.module,
            ),
        ];
        for (name, number) in ops {
            assert_eq!(place(name), number, "{name}");
        }
    }

    #[test]
    fn a_modules_result_is_the_value_or_its_error() {
        assert_eq!(result_of(5).unwrap(), 5);
        assert_eq!(result_of(0).unwrap(), 0);
        assert_eq!(
            result_of(-i64::from(libc::EBUSY))
                .err()
                .and_then(|e| e.raw_os_error()),
            Some(libc::EBUSY)
        );
        assert_eq!(
            result_of(i64::MAX).err().and_then(|e| e.raw_os_error()),
            Some(libc::EOVERFLOW)
        );
    }

    #[test]
    fn a_node_that_is_not_the_module_refuses_the_claim() {
        let node = Arc::new(OwnedFd::from(File::open("/dev/null").unwrap()));
        let error = PioDevice::through_module(&node).err().unwrap();
        assert_eq!(error.raw_os_error(), Some(libc::ENOTTY));
        assert!(!device().is_module());
    }

    #[test]
    fn slot_masks_place_programs_and_first_fit_finds_the_gap() {
        assert_eq!(slot_mask(0, 4), 0b1111);
        assert_eq!(slot_mask(4, 27), 0x7fff_fff0);
        assert_eq!(slot_mask(0, 32), u32::MAX);
        // A 4-slot program at 0 leaves 4 and up free: a 27-slot program lands at 4.
        assert_eq!(first_fit(slot_mask(0, 4), 27), Some(4));
        // Nothing fits in a full block, and nothing empty-length or oversized.
        assert_eq!(first_fit(u32::MAX, 1), None);
        assert_eq!(first_fit(0, 0), None);
        assert_eq!(first_fit(0, 33), None);
        assert_eq!(first_fit(0, 32), Some(0));
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

    #[test]
    fn a_state_machine_index_past_the_block_is_refused_before_the_ioctl() {
        let dev = device();
        assert_eq!(
            errno(dev.sm_init(4, 0, SmConfig::default())),
            Some(libc::EINVAL)
        );
        assert_eq!(errno(dev.sm_clear_fifos(u16::MAX)), Some(libc::EINVAL));
        assert_eq!(errno(dev.sm_set_enabled(0x10, true)), Some(libc::EINVAL));
        assert_eq!(errno(dev.unclaim_sms(0x1_0)), Some(libc::EINVAL));
        // A valid index reaches the device (`/dev/null` is not a terminal).
        assert_eq!(errno(dev.sm_clear_fifos(3)), Some(libc::ENOTTY));
    }

    #[test]
    fn a_program_outside_the_instruction_memory_is_refused() {
        let dev = device();
        assert_eq!(errno(dev.add_program(28, &[0; 5])), Some(libc::EINVAL));
        assert_eq!(errno(dev.add_program(0, &[])), Some(libc::EINVAL));
        assert_eq!(errno(dev.add_program(0, &[0; 33])), Some(libc::EINVAL));
        assert_eq!(errno(dev.add_program(4, &[0; 27])), Some(libc::ENOTTY));
        assert_eq!(
            errno(dev.add_program(u16::MAX, &[0; 4])),
            Some(libc::ENOTTY)
        );
        assert_eq!(errno(dev.remove_program(30, 4)), Some(libc::EINVAL));
        assert_eq!(errno(dev.remove_program(0, 0)), Some(libc::EINVAL));
    }

    #[test]
    fn a_gpio_or_function_outside_range_is_refused() {
        let dev = device();
        assert_eq!(errno(dev.gpio_init(28)), Some(libc::EINVAL));
        assert_eq!(errno(dev.gpio_set_function(7, 32)), Some(libc::EINVAL));
        assert_eq!(errno(dev.gpio_set_outover(7, 4)), Some(libc::EINVAL));
        assert_eq!(
            errno(dev.gpio_set_input_enabled(40, true)),
            Some(libc::EINVAL)
        );
        assert_eq!(
            errno(dev.gpio_set_function(7, GPIO_FUNC_PIO)),
            Some(libc::ENOTTY)
        );
    }

    #[test]
    fn pin_masks_stay_on_the_gpios_and_values_stay_in_the_mask() {
        let dev = device();
        assert_eq!(errno(dev.sm_set_pins(0, 0, 1 << 28)), Some(libc::EINVAL));
        assert_eq!(
            errno(dev.sm_set_pindirs(0, 1 << 8, 1 << 7)),
            Some(libc::EINVAL)
        );
        assert_eq!(errno(dev.sm_set_pindirs(0, 0, 1 << 27)), Some(libc::ENOTTY));
    }

    #[test]
    fn dma_lengths_are_bounded_before_they_reach_the_device() {
        let dev = device();
        let big = vec![0u32; MAX_XFER_WORDS + 1];
        let mut big_mut = vec![0u32; MAX_XFER_WORDS + 1];
        assert_eq!(errno(dev.xfer_to_sm(0, &[])), Some(libc::EINVAL));
        assert_eq!(errno(dev.xfer_to_sm(0, &big)), Some(libc::EINVAL));
        assert_eq!(errno(dev.xfer_from_sm(0, &mut big_mut)), Some(libc::EINVAL));
        assert_eq!(errno(dev.xfer_to_sm(0, &[1, 2])), Some(libc::ENOTTY));
        assert_eq!(
            errno(dev.config_xfer32(0, DIR_TO_SM, 6, 4)),
            Some(libc::EINVAL)
        );
        assert_eq!(
            errno(dev.config_xfer32(0, DIR_TO_SM, 0, 4)),
            Some(libc::EINVAL)
        );
        assert_eq!(
            errno(dev.config_xfer32(0, DIR_TO_SM, 64, 0)),
            Some(libc::EINVAL)
        );
        assert_eq!(errno(dev.config_xfer32(0, 2, 64, 4)), Some(libc::EINVAL));
        assert_eq!(
            errno(dev.config_xfer32(0, DIR_FROM_SM, 64, 4)),
            Some(libc::ENOTTY)
        );
    }
}
