//! The Lemnos kernel module's control node, `/dev/lemnos`: claims, and the buses, lines and
//! channels that belong to them, and a bound block's DMA memory, register regions, interrupts,
//! clocks and resets. The interface is `kernel/lemnos.h`.

use std::fs::OpenOptions;
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError, Weak};

use crate::i2c::Message;
use crate::ioctl::{Ioctl, from_bytes, ioctl_ptr, iow, iowr, pod, zeroed};
use crate::spi::Transfer;

/// Opens the control node at `path` for everything in this process to share.
///
/// The module lets one file be open on the node at a time, system-wide, and what a process does
/// through it (its claims, its blocks' streams, its events) belongs to that file. So a process
/// opens the node once: the first call opens it, non-blocking and close-on-exec, and every
/// later call for the same path gets that same open file, until the last holder drops it and
/// the file closes. A daemon whose drivers reach the module on their own (a block driver making
/// its stream) and through the board (claims) thus holds one file, not two the module refuses.
pub fn open_shared(path: &Path) -> io::Result<Arc<OwnedFd>> {
    static OPEN: Mutex<Option<(PathBuf, Weak<OwnedFd>)>> = Mutex::new(None);
    let mut open = OPEN.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((at, fd)) = open.as_ref()
        && at == path
        && let Some(fd) = fd.upgrade()
    {
        return Ok(fd);
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)?;
    let fd = Arc::new(OwnedFd::from(file));
    *open = Some((path.to_path_buf(), Arc::downgrade(&fd)));
    Ok(fd)
}

/// `LEMNOS_BUS_MAX_SEGMENTS`.
pub const MAX_SEGMENTS: usize = 16;
const NAME_MAX: usize = 64;
const PROVIDER_MAX: usize = NAME_MAX - 16;
const TYPE: u8 = b'L';

/// `LEMNOS_GPIO_*`, the flags of [`gpio_config`].
pub mod gpio {
    /// Drive the line.
    pub const OUTPUT: u32 = 1 << 0;
    /// Logical values are inverted.
    pub const ACTIVE_LOW: u32 = 1 << 1;
    /// Internal pull-up.
    pub const PULL_UP: u32 = 1 << 2;
    /// Internal pull-down.
    pub const PULL_DOWN: u32 = 1 << 3;
    /// No internal bias.
    pub const BIAS_OFF: u32 = 1 << 4;
    /// Drive low only.
    pub const OPEN_DRAIN: u32 = 1 << 5;
    /// Drive high only.
    pub const OPEN_SOURCE: u32 = 1 << 6;
    /// Report logical rises.
    pub const RISING: u32 = 1 << 7;
    /// Report logical falls.
    pub const FALLING: u32 = 1 << 8;
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Claim {
    resource: [u8; NAME_MAX],
    flags: u32,
    fd: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct I2cSegment {
    buf: u64,
    len: u32,
    flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct I2cTransfer {
    claim: i32,
    bus: u32,
    addr: u16,
    flags: u16,
    count: u32,
    segments: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct I2cSmbus {
    claim: i32,
    bus: u32,
    addr: u16,
    flags: u16,
    read: u8,
    command: u8,
    protocol: u16,
    data: [u8; 34],
    reserved: [u8; 6],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct SpiSegment {
    tx: u64,
    rx: u64,
    len: u32,
    speed_hz: u32,
    delay_us: u16,
    bits: u8,
    cs_change: u8,
    reserved: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SpiTransfer {
    claim: i32,
    bus: u32,
    cs: u32,
    mode: u32,
    count: u32,
    reserved: u32,
    segments: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SpiInfo {
    bus: u32,
    cs: u32,
    mode: u32,
    reserved: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct GpioRequest {
    claim: i32,
    chip: u32,
    offset: u32,
    reserved: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct GpioConfig {
    claim: i32,
    flags: u32,
    value: u32,
    debounce_us: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct GpioValue {
    claim: i32,
    value: u32,
}

/// `struct lemnos_gpio_event`: what reading a line's claim gives.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpioEvent {
    /// `ktime_get_ns()` at the edge.
    pub ns: u64,
    /// Counts every edge, so a gap is a loss.
    pub seq: u32,
    /// 1 for a logical rise.
    pub rising: u32,
}

/// Bytes of one [`GpioEvent`].
pub const GPIO_EVENT_SIZE: usize = size_of::<GpioEvent>();

impl GpioEvent {
    /// The event `bytes` hold.
    pub fn from_bytes(bytes: &[u8; GPIO_EVENT_SIZE]) -> Self {
        from_bytes(bytes)
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct PwmRequest {
    claim: i32,
    channel: u32,
    provider: [u8; PROVIDER_MAX],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct PwmStateRaw {
    claim: i32,
    enabled: u32,
    inverted: u32,
    reserved: u32,
    period_ns: u64,
    duty_ns: u64,
}

/// A PWM channel's settings, as the module holds them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PwmState {
    /// The output runs.
    pub enabled: bool,
    /// The polarity is inverted.
    pub inverted: bool,
    /// Period, nanoseconds.
    pub period_ns: u64,
    /// Time active per period, nanoseconds.
    pub duty_ns: u64,
}

/// `struct lemnos_next`.
#[repr(C)]
#[derive(Clone, Copy)]
struct Next {
    after: u32,
    block: u32,
    name: [u8; NAME_MAX],
}

/// `struct lemnos_info`.
#[repr(C)]
#[derive(Clone, Copy)]
struct InfoRaw {
    block: u32,
    abi: u32,
    flags: u32,
    regions: u32,
    irqs: u32,
    dma_bits: u32,
    reserved: [u32; 2],
}

/// `struct lemnos_alloc`.
#[repr(C)]
#[derive(Clone, Copy)]
struct AllocRaw {
    block: u32,
    flags: u32,
    size: u64,
    addr: u64,
    fd: i32,
    reserved: u32,
}

/// `struct lemnos_import`.
#[repr(C)]
#[derive(Clone, Copy)]
struct ImportRaw {
    block: u32,
    fd: i32,
    addr: u64,
    size: u64,
    handle: u32,
    reserved: u32,
}

/// `struct lemnos_release`.
#[repr(C)]
#[derive(Clone, Copy)]
struct ReleaseRaw {
    block: u32,
    handle: u32,
}

/// `struct lemnos_region`.
#[repr(C)]
#[derive(Clone, Copy)]
struct RegionRaw {
    block: u32,
    index: u32,
    start: u64,
    size: u64,
    offset: u64,
}

/// `struct lemnos_irq`.
#[repr(C)]
#[derive(Clone, Copy)]
struct IrqRaw {
    block: u32,
    index: u32,
}

/// `struct lemnos_power`.
#[repr(C)]
#[derive(Clone, Copy)]
struct PowerRaw {
    block: u32,
    kind: u32,
    op: u32,
    handle: u32,
    name: [u8; POWER_NAME],
    value: u64,
    value2: u64,
}

/// `struct lemnos_event`: what reading the control node gives.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Event {
    /// The block it is about.
    pub block: u32,
    /// For an interrupt, which of the block's.
    pub index: u32,
    /// For an interrupt, how many times it has fired since the block was bound; for the other
    /// kinds, their own sequence number.
    pub count: u32,
    /// `EVENT_*`.
    pub kind: u32,
    /// `ktime_get_ns()` when it happened.
    pub ns: u64,
    /// What the kind carries.
    pub value: i64,
}

/// `LEMNOS_EVENT_IRQ`.
pub const EVENT_IRQ: u32 = 0;
/// `LEMNOS_EVENT_BLOCK_BOUND`.
pub const EVENT_BLOCK_BOUND: u32 = 1;
/// `LEMNOS_EVENT_BLOCK_UNBOUND`.
pub const EVENT_BLOCK_UNBOUND: u32 = 2;

/// `LEMNOS_INFO_MMIO_MAPPABLE`: the block's registers may be mapped.
pub const INFO_MMIO_MAPPABLE: u32 = 1 << 0;
/// `LEMNOS_INFO_MMIO_UNCONFINED`: they may be mapped only because the module was loaded with the
/// block in `unconfined=`: no IOMMU confines its DMA, so its driver alone keeps it in bounds.
pub const INFO_MMIO_UNCONFINED: u32 = 1 << 1;

/// `LEMNOS_POWER_NAME`: room for a clock's or reset's name and its NUL.
pub const POWER_NAME: usize = 32;

/// `LEMNOS_POWER_*`: what a [`power`] request is about and what it does.
pub mod power {
    /// A clock.
    pub const CLOCK: u32 = 0;
    /// A supply.
    pub const SUPPLY: u32 = 1;
    /// A reset line.
    pub const RESET: u32 = 2;
    /// A clock or supply on; a reset deasserted.
    pub const ENABLE: u32 = 0;
    /// A clock or supply off; a reset asserted.
    pub const DISABLE: u32 = 1;
    /// The value is 1 for on, or for a reset asserted.
    pub const STATUS: u32 = 2;
    /// The value is hertz, or microvolts.
    pub const GET: u32 = 3;
    /// Sets hertz, or minimum and maximum microvolts; the value is what it settled on.
    pub const SET: u32 = 4;
    /// A reset asserted and deasserted.
    pub const PULSE: u32 = 5;
}

/// One register region of a block (`LEMNOS_IOC_REGION`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region {
    /// Its physical address.
    pub start: u64,
    /// Bytes.
    pub size: u64,
    /// What to give `mmap` on the control node to map it.
    pub offset: u64,
}

/// `LEMNOS_ABI`: the interface these structures are.
pub const ABI: u32 = 1;

/// What the module says about a bound block (`LEMNOS_IOC_INFO`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockInfo {
    /// `LEMNOS_ABI` of the module.
    pub abi: u32,
    /// `LEMNOS_INFO_*`.
    pub flags: u32,
    /// Register regions.
    pub regions: u32,
    /// Interrupts forwarded.
    pub irqs: u32,
    /// Every device address of the block is below `2^dma_bits`.
    pub dma_bits: u32,
}

/// DMA memory the module allocated for a block (`LEMNOS_IOC_ALLOC`).
#[derive(Debug)]
pub struct BlockMemory {
    /// The memory as a dma-buf: `mmap` it for the CPU. The module frees the memory when the last
    /// reference to it goes, and the block can't be unbound before then.
    pub fd: OwnedFd,
    /// The address the module mapped it at for the block, from its own allocation.
    pub addr: u64,
    /// Bytes, the size asked rounded up to pages.
    pub size: u64,
}

/// A dma-buf mapped on a block (`LEMNOS_IOC_IMPORT`), until [`release`] or the node closes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Imported {
    /// Its device address.
    pub addr: u64,
    /// Bytes mapped.
    pub size: u64,
    /// For [`release`]; never zero.
    pub handle: u32,
}

pod!(
    Claim,
    I2cSmbus,
    SpiInfo,
    GpioRequest,
    GpioConfig,
    GpioValue,
    GpioEvent,
    PwmRequest,
    PwmStateRaw,
    Next,
    InfoRaw,
    AllocRaw,
    ImportRaw,
    ReleaseRaw,
    RegionRaw,
    IrqRaw,
    PowerRaw,
    Event,
);

const CLAIM: Ioctl<Claim> = Ioctl::new(iowr::<Claim>(TYPE, 0xce), "LEMNOS_IOC_CLAIM");
const I2C_SMBUS: Ioctl<I2cSmbus> = Ioctl::new(iowr::<I2cSmbus>(TYPE, 0xa9), "LEMNOS_IOC_I2C_SMBUS");
const SPI_INFO: Ioctl<SpiInfo> = Ioctl::new(iowr::<SpiInfo>(TYPE, 0xab), "LEMNOS_IOC_SPI_INFO");
const GPIO_REQUEST: Ioctl<GpioRequest> =
    Ioctl::new(iow::<GpioRequest>(TYPE, 0x98), "LEMNOS_IOC_GPIO_REQUEST");
const GPIO_CONFIG: Ioctl<GpioConfig> =
    Ioctl::new(iow::<GpioConfig>(TYPE, 0x99), "LEMNOS_IOC_GPIO_CONFIG");
const GPIO_GET: Ioctl<GpioValue> = Ioctl::new(iowr::<GpioValue>(TYPE, 0x9a), "LEMNOS_IOC_GPIO_GET");
const GPIO_SET: Ioctl<GpioValue> = Ioctl::new(iow::<GpioValue>(TYPE, 0x9b), "LEMNOS_IOC_GPIO_SET");
const PWM_REQUEST: Ioctl<PwmRequest> =
    Ioctl::new(iow::<PwmRequest>(TYPE, 0xb8), "LEMNOS_IOC_PWM_REQUEST");
const PWM_APPLY: Ioctl<PwmStateRaw> =
    Ioctl::new(iow::<PwmStateRaw>(TYPE, 0xb9), "LEMNOS_IOC_PWM_APPLY");
const PWM_GET: Ioctl<PwmStateRaw> =
    Ioctl::new(iowr::<PwmStateRaw>(TYPE, 0xba), "LEMNOS_IOC_PWM_GET");

const NEXT: Ioctl<Next> = Ioctl::new(iowr::<Next>(TYPE, 0xcf), "LEMNOS_IOC_NEXT");
const INFO: Ioctl<InfoRaw> = Ioctl::new(iowr::<InfoRaw>(TYPE, 0xd0), "LEMNOS_IOC_INFO");
const ALLOC: Ioctl<AllocRaw> = Ioctl::new(iowr::<AllocRaw>(TYPE, 0xd2), "LEMNOS_IOC_ALLOC");
const IMPORT: Ioctl<ImportRaw> = Ioctl::new(iowr::<ImportRaw>(TYPE, 0xd3), "LEMNOS_IOC_IMPORT");
const RELEASE: Ioctl<ReleaseRaw> = Ioctl::new(iow::<ReleaseRaw>(TYPE, 0xd4), "LEMNOS_IOC_RELEASE");
const REGION: Ioctl<RegionRaw> = Ioctl::new(iowr::<RegionRaw>(TYPE, 0xd1), "LEMNOS_IOC_REGION");
const IRQ_ENABLE: Ioctl<IrqRaw> = Ioctl::new(iow::<IrqRaw>(TYPE, 0xd5), "LEMNOS_IOC_IRQ_ENABLE");
const POWER: Ioctl<PowerRaw> = Ioctl::new(iowr::<PowerRaw>(TYPE, 0xd6), "LEMNOS_IOC_POWER");
const TEST_IRQ: Ioctl<IrqRaw> = Ioctl::new(iow::<IrqRaw>(TYPE, 0xdf), "LEMNOS_IOC_TEST_IRQ");

// These carry pointers the kernel follows, so they aren't `Ioctl`s.
const I2C_TRANSFER: u32 = iow::<I2cTransfer>(TYPE, 0xa8);
const SPI_TRANSFER: u32 = iow::<SpiTransfer>(TYPE, 0xaa);

const SMBUS_BYTE: u16 = 1;
const SMBUS_BYTE_DATA: u16 = 2;
const SMBUS_I2C_BLOCK_DATA: u16 = 8;
const SMBUS_BLOCK_MAX: usize = 32;

const I2C_READ: u32 = 1 << 0;

fn invalid() -> io::Error {
    io::ErrorKind::InvalidInput.into()
}

/// Takes `resource`, exclusively, until the returned file closes.
pub fn claim(node: BorrowedFd<'_>, resource: &str) -> io::Result<OwnedFd> {
    let name = resource.as_bytes();
    if name.is_empty() || name.len() >= NAME_MAX || name.contains(&0) {
        return Err(invalid());
    }
    let mut arg: Claim = zeroed();
    arg.resource[..name.len()].copy_from_slice(name);
    CLAIM.call(node, &mut arg)?;
    // SAFETY: the kernel just made `arg.fd` for this process, and nothing else owns it.
    Ok(unsafe { OwnedFd::from_raw_fd(arg.fd) })
}

/// One I2C target, held by a claim on it.
#[derive(Clone, Copy)]
pub struct I2cTarget<'a> {
    /// The control node.
    pub node: BorrowedFd<'a>,
    /// A claim on this target.
    pub claim: BorrowedFd<'a>,
    /// The adapter number.
    pub bus: u32,
    /// The 7-bit address.
    pub address: u16,
}

impl I2cTarget<'_> {
    /// Runs `messages` as one transfer: repeated starts between them, one stop at the end.
    pub fn transfer(&self, messages: &mut [Message<'_>]) -> io::Result<()> {
        if messages.len() > MAX_SEGMENTS {
            return Err(invalid());
        }
        let mut segments = [I2cSegment::default(); MAX_SEGMENTS];
        for (slot, message) in segments.iter_mut().zip(messages.iter_mut()) {
            *slot = match message {
                Message::Write(data) => I2cSegment {
                    buf: data.as_ptr() as u64,
                    len: u32::try_from(data.len()).map_err(|_| invalid())?,
                    flags: 0,
                },
                Message::Read(buffer) => I2cSegment {
                    buf: buffer.as_mut_ptr() as u64,
                    len: u32::try_from(buffer.len()).map_err(|_| invalid())?,
                    flags: I2C_READ,
                },
            };
        }
        let mut arg = I2cTransfer {
            claim: self.claim.as_raw_fd(),
            bus: self.bus,
            addr: self.address,
            flags: 0,
            count: messages.len() as u32,
            segments: segments.as_mut_ptr() as u64,
        };
        // SAFETY: `arg` points at `segments`, `count` live entries whose buffers come from
        // slices `messages` borrows (read buffers exclusively), each `len` bytes long, and all
        // of them outlive the call.
        unsafe { ioctl_ptr(self.node, I2C_TRANSFER, &raw mut arg) }?;
        Ok(())
    }

    fn smbus(&self, read: bool, command: u8, protocol: u16, data: &mut [u8; 34]) -> io::Result<()> {
        let mut arg: I2cSmbus = zeroed();
        arg.claim = self.claim.as_raw_fd();
        arg.bus = self.bus;
        arg.addr = self.address;
        arg.read = u8::from(read);
        arg.command = command;
        arg.protocol = protocol;
        arg.data = *data;
        I2C_SMBUS.call(self.node, &mut arg)?;
        *data = arg.data;
        Ok(())
    }

    /// SMBus "send byte".
    pub fn smbus_write_byte(&self, value: u8) -> io::Result<()> {
        self.smbus(false, value, SMBUS_BYTE, &mut [0; 34])
    }

    /// SMBus "write byte data".
    pub fn smbus_write_byte_data(&self, register: u8, value: u8) -> io::Result<()> {
        let mut data = [0; 34];
        data[0] = value;
        self.smbus(false, register, SMBUS_BYTE_DATA, &mut data)
    }

    /// SMBus "read byte data".
    pub fn smbus_read_byte_data(&self, register: u8) -> io::Result<u8> {
        let mut data = [0; 34];
        self.smbus(true, register, SMBUS_BYTE_DATA, &mut data)?;
        Ok(data[0])
    }

    /// SMBus "write I2C block": up to 32 bytes.
    pub fn smbus_write_block(&self, register: u8, values: &[u8]) -> io::Result<()> {
        if values.len() > SMBUS_BLOCK_MAX {
            return Err(invalid());
        }
        let mut data = [0; 34];
        data[0] = values.len() as u8;
        data[1..=values.len()].copy_from_slice(values);
        self.smbus(false, register, SMBUS_I2C_BLOCK_DATA, &mut data)
    }

    /// SMBus "read I2C block" into `out`, up to 32 bytes. Returns the bytes read.
    pub fn smbus_read_block(&self, register: u8, out: &mut [u8]) -> io::Result<usize> {
        if out.len() > SMBUS_BLOCK_MAX {
            return Err(invalid());
        }
        let mut data = [0; 34];
        data[0] = out.len() as u8;
        self.smbus(true, register, SMBUS_I2C_BLOCK_DATA, &mut data)?;
        let n = usize::from(data[0]).min(out.len());
        out[..n].copy_from_slice(&data[1..=n]);
        Ok(n)
    }
}

/// The mode bits of a SPI device bound to the module, `ENODEV` if none is.
pub fn spi_mode(node: BorrowedFd<'_>, bus: u32, cs: u32) -> io::Result<u32> {
    let mut arg = SpiInfo {
        bus,
        cs,
        mode: 0,
        reserved: 0,
    };
    SPI_INFO.call(node, &mut arg)?;
    Ok(arg.mode)
}

/// Runs `transfers` as one message on a SPI device, in `mode`.
pub fn spi_transfer(
    node: BorrowedFd<'_>,
    claim: BorrowedFd<'_>,
    (bus, cs): (u32, u32),
    mode: u32,
    transfers: &mut [Transfer<'_>],
) -> io::Result<()> {
    if transfers.is_empty() {
        return Ok(());
    }
    if transfers.len() > MAX_SEGMENTS {
        return Err(invalid());
    }
    let mut segments = [SpiSegment::default(); MAX_SEGMENTS];
    for (slot, transfer) in segments.iter_mut().zip(transfers.iter_mut()) {
        let raw = transfer.raw()?;
        *slot = SpiSegment {
            tx: raw.tx_buf,
            rx: raw.rx_buf,
            len: raw.len,
            speed_hz: raw.speed_hz,
            delay_us: raw.delay_usecs,
            bits: raw.bits_per_word,
            cs_change: raw.cs_change,
            reserved: 0,
        };
    }
    let mut arg = SpiTransfer {
        claim: claim.as_raw_fd(),
        bus,
        cs,
        mode,
        count: transfers.len() as u32,
        reserved: 0,
        segments: segments.as_mut_ptr() as u64,
    };
    // SAFETY: `arg` points at `segments`, `count` live entries whose buffers come from slices
    // `transfers` borrows (read buffers exclusively), each at least `len` bytes long, and all
    // of them outlive the call.
    unsafe { ioctl_ptr(node, SPI_TRANSFER, &raw mut arg) }?;
    Ok(())
}

/// Binds line `offset` of gpiochip `chip` to its claim, leaving the line as it is.
pub fn gpio_request(
    node: BorrowedFd<'_>,
    claim: BorrowedFd<'_>,
    chip: u32,
    offset: u32,
) -> io::Result<()> {
    let mut arg = GpioRequest {
        claim: claim.as_raw_fd(),
        chip,
        offset,
        reserved: 0,
    };
    GPIO_REQUEST.call(node, &mut arg)?;
    Ok(())
}

/// Configures a line. `flags` are [`gpio`]'s, `value` is an output's logical start.
pub fn gpio_config(
    node: BorrowedFd<'_>,
    claim: BorrowedFd<'_>,
    flags: u32,
    value: bool,
    debounce_us: u32,
) -> io::Result<()> {
    let mut arg = GpioConfig {
        claim: claim.as_raw_fd(),
        flags,
        value: u32::from(value),
        debounce_us,
    };
    GPIO_CONFIG.call(node, &mut arg)?;
    Ok(())
}

/// A line's logical value.
pub fn gpio_get(node: BorrowedFd<'_>, claim: BorrowedFd<'_>) -> io::Result<bool> {
    let mut arg = GpioValue {
        claim: claim.as_raw_fd(),
        value: 0,
    };
    GPIO_GET.call(node, &mut arg)?;
    Ok(arg.value != 0)
}

/// Sets an output's logical value.
pub fn gpio_set(node: BorrowedFd<'_>, claim: BorrowedFd<'_>, value: bool) -> io::Result<()> {
    let mut arg = GpioValue {
        claim: claim.as_raw_fd(),
        value: u32::from(value),
    };
    GPIO_SET.call(node, &mut arg)?;
    Ok(())
}

/// Binds a PWM channel to its claim. `provider` is the PWM chip's parent device name.
pub fn pwm_request(
    node: BorrowedFd<'_>,
    claim: BorrowedFd<'_>,
    provider: &str,
    channel: u32,
) -> io::Result<()> {
    let name = provider.as_bytes();
    if name.is_empty() || name.len() >= PROVIDER_MAX || name.contains(&0) {
        return Err(invalid());
    }
    let mut arg: PwmRequest = zeroed();
    arg.claim = claim.as_raw_fd();
    arg.channel = channel;
    arg.provider[..name.len()].copy_from_slice(name);
    PWM_REQUEST.call(node, &mut arg)?;
    Ok(())
}

/// Applies a PWM channel's settings in one step.
pub fn pwm_apply(node: BorrowedFd<'_>, claim: BorrowedFd<'_>, state: &PwmState) -> io::Result<()> {
    let mut arg = PwmStateRaw {
        claim: claim.as_raw_fd(),
        enabled: u32::from(state.enabled),
        inverted: u32::from(state.inverted),
        reserved: 0,
        period_ns: state.period_ns,
        duty_ns: state.duty_ns,
    };
    PWM_APPLY.call(node, &mut arg)?;
    Ok(())
}

/// A PWM channel's current settings.
pub fn pwm_get(node: BorrowedFd<'_>, claim: BorrowedFd<'_>) -> io::Result<PwmState> {
    let mut arg: PwmStateRaw = zeroed();
    arg.claim = claim.as_raw_fd();
    PWM_GET.call(node, &mut arg)?;
    Ok(PwmState {
        enabled: arg.enabled != 0,
        inverted: arg.inverted != 0,
        period_ns: arg.period_ns,
        duty_ns: arg.duty_ns,
    })
}

/// The number of the bound block whose device name is `name`, walking `LEMNOS_IOC_NEXT`. `None`
/// when no block bound to the module has that name. Allocates nothing.
pub fn find_block(node: BorrowedFd<'_>, name: &str) -> io::Result<Option<u32>> {
    let wanted = name.as_bytes();
    if wanted.is_empty() || wanted.len() >= NAME_MAX || wanted.contains(&0) {
        return Err(invalid());
    }
    let mut after = 0;
    loop {
        let mut arg: Next = zeroed();
        arg.after = after;
        match NEXT.call(node, &mut arg) {
            Ok(_) => {}
            Err(e) if e.raw_os_error() == Some(libc::ENOENT) => return Ok(None),
            Err(e) => return Err(e),
        }
        let len = arg.name.iter().position(|&b| b == 0).unwrap_or(NAME_MAX);
        if &arg.name[..len] == wanted {
            return Ok(Some(arg.block));
        }
        // Numbers only grow; one that doesn't is a module this code doesn't know.
        if arg.block <= after {
            return Err(io::ErrorKind::InvalidData.into());
        }
        after = arg.block;
    }
}

/// What the module says about block `block`. `ENODEV` once it is unbound.
pub fn block_info(node: BorrowedFd<'_>, block: u32) -> io::Result<BlockInfo> {
    let mut arg: InfoRaw = zeroed();
    arg.block = block;
    INFO.call(node, &mut arg)?;
    Ok(BlockInfo {
        abi: arg.abi,
        flags: arg.flags,
        regions: arg.regions,
        irqs: arg.irqs,
        dma_bits: arg.dma_bits,
    })
}

/// Allocates `size` bytes of DMA memory for block `block`, as a dma-buf with the device address
/// the module mapped it at. `ENODEV` once the block is unbound.
pub fn alloc(node: BorrowedFd<'_>, block: u32, size: u64) -> io::Result<BlockMemory> {
    let mut arg: AllocRaw = zeroed();
    arg.block = block;
    arg.size = size;
    ALLOC.call(node, &mut arg)?;
    // SAFETY: the kernel just made `arg.fd` for this process, and nothing else owns it.
    let fd = unsafe { OwnedFd::from_raw_fd(arg.fd) };
    Ok(BlockMemory {
        fd,
        addr: arg.addr,
        size: arg.size,
    })
}

/// Maps the dma-buf `dmabuf` on block `block`, contiguous in its address space, until
/// [`release`] or the node closes.
pub fn import(node: BorrowedFd<'_>, block: u32, dmabuf: BorrowedFd<'_>) -> io::Result<Imported> {
    let mut arg: ImportRaw = zeroed();
    arg.block = block;
    arg.fd = dmabuf.as_raw_fd();
    IMPORT.call(node, &mut arg)?;
    Ok(Imported {
        addr: arg.addr,
        size: arg.size,
        handle: arg.handle,
    })
}

/// Ends an [`import`].
pub fn release(node: BorrowedFd<'_>, block: u32, handle: u32) -> io::Result<()> {
    let mut arg = ReleaseRaw { block, handle };
    RELEASE.call(node, &mut arg)?;
    Ok(())
}

/// Register region `index` of block `block`: where it is and the `mmap` offset that maps it.
/// `EINVAL` past the block's regions, `ENODEV` once it is unbound.
pub fn region(node: BorrowedFd<'_>, block: u32, index: u32) -> io::Result<Region> {
    let mut arg: RegionRaw = zeroed();
    arg.block = block;
    arg.index = index;
    REGION.call(node, &mut arg)?;
    Ok(Region {
        start: arg.start,
        size: arg.size,
        offset: arg.offset,
    })
}

/// Unmasks interrupt `index` of block `block`, after its cause was cleared. It masks itself again
/// when it fires.
pub fn irq_enable(node: BorrowedFd<'_>, block: u32, index: u32) -> io::Result<()> {
    let mut arg = IrqRaw { block, index };
    IRQ_ENABLE.call(node, &mut arg)?;
    Ok(())
}

/// Sets interrupt `index` of block `block` pending at its controller. Test only: the module
/// refuses it (`EPERM`) unless it was loaded with `test_irq_inject`.
pub fn test_irq(node: BorrowedFd<'_>, block: u32, index: u32) -> io::Result<()> {
    let mut arg = IrqRaw { block, index };
    TEST_IRQ.call(node, &mut arg)?;
    Ok(())
}

/// One `LEMNOS_IOC_POWER` request: `op` ([`power`]) on the clock, supply or reset of kind `kind`
/// named `name` (empty for a block's only clock or reset), with `value` and `value2` in. Returns
/// the value the module answers.
pub fn power(
    node: BorrowedFd<'_>,
    block: u32,
    (kind, op): (u32, u32),
    name: &str,
    (value, value2): (u64, u64),
) -> io::Result<u64> {
    let bytes = name.as_bytes();
    if bytes.len() >= POWER_NAME || bytes.contains(&0) {
        return Err(invalid());
    }
    let mut arg: PowerRaw = zeroed();
    arg.block = block;
    arg.kind = kind;
    arg.op = op;
    arg.name[..bytes.len()].copy_from_slice(bytes);
    arg.value = value;
    arg.value2 = value2;
    POWER.call(node, &mut arg)?;
    Ok(arg.value)
}

/// Reads what events are waiting on the control node into `out`, at most `out.len()`, and returns
/// how many. On a node opened non-blocking, `WouldBlock` when there are none. Allocates nothing.
pub fn read_events(node: BorrowedFd<'_>, out: &mut [Event]) -> io::Result<usize> {
    if out.is_empty() {
        return Err(invalid());
    }
    let bytes = size_of_val(out);
    loop {
        // SAFETY: `out` is `bytes` writable bytes for the call, and `Event` is plain data every
        // bit pattern of which is a value, so whatever the kernel writes there is one.
        let n = unsafe { libc::read(node.as_raw_fd(), out.as_mut_ptr().cast(), bytes) };
        if n >= 0 {
            // The module writes whole events only.
            return Ok(n as usize / size_of::<Event>());
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_process_opens_the_node_once_and_shares_it() {
        let dir = std::env::temp_dir().join(format!("lemnos-shared-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("node");
        std::fs::write(&path, b"").unwrap();
        let first = open_shared(&path).unwrap();
        let second = open_shared(&path).unwrap();
        assert!(Arc::ptr_eq(&first, &second), "one open file, not two");
        drop(first);
        drop(second);
        // Every holder gone: the file closed, and the next call opens it again.
        let again = open_shared(&path).unwrap();
        assert_eq!(Arc::strong_count(&again), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn layouts_are_the_kernels() {
        assert_eq!(size_of::<Claim>(), 72);
        assert_eq!(size_of::<I2cSegment>(), 16);
        assert_eq!(size_of::<I2cTransfer>(), 24);
        assert_eq!(size_of::<I2cSmbus>(), 56);
        assert_eq!(size_of::<SpiSegment>(), 32);
        assert_eq!(size_of::<SpiTransfer>(), 32);
        assert_eq!(size_of::<SpiInfo>(), 16);
        assert_eq!(size_of::<GpioRequest>(), 16);
        assert_eq!(size_of::<GpioConfig>(), 16);
        assert_eq!(size_of::<GpioValue>(), 8);
        assert_eq!(GPIO_EVENT_SIZE, 16);
        assert_eq!(size_of::<PwmRequest>(), 56);
        assert_eq!(size_of::<PwmStateRaw>(), 32);
        assert_eq!(size_of::<Next>(), 72);
        assert_eq!(size_of::<InfoRaw>(), 32);
        assert_eq!(size_of::<AllocRaw>(), 32);
        assert_eq!(size_of::<ImportRaw>(), 32);
        assert_eq!(size_of::<ReleaseRaw>(), 8);
        assert_eq!(size_of::<RegionRaw>(), 32);
        assert_eq!(size_of::<IrqRaw>(), 8);
        assert_eq!(size_of::<PowerRaw>(), 64);
        assert_eq!(size_of::<Event>(), 32);
    }

    #[test]
    fn block_fields_are_where_the_kernel_puts_them() {
        use std::mem::offset_of;
        assert_eq!(offset_of!(Next, block), 4);
        assert_eq!(offset_of!(Next, name), 8);
        assert_eq!(offset_of!(InfoRaw, abi), 4);
        assert_eq!(offset_of!(InfoRaw, dma_bits), 20);
        assert_eq!(offset_of!(AllocRaw, flags), 4);
        assert_eq!(offset_of!(AllocRaw, size), 8);
        assert_eq!(offset_of!(AllocRaw, addr), 16);
        assert_eq!(offset_of!(AllocRaw, fd), 24);
        assert_eq!(offset_of!(ImportRaw, fd), 4);
        assert_eq!(offset_of!(ImportRaw, addr), 8);
        assert_eq!(offset_of!(ImportRaw, size), 16);
        assert_eq!(offset_of!(ImportRaw, handle), 24);
        assert_eq!(offset_of!(ReleaseRaw, handle), 4);
        assert_eq!(offset_of!(RegionRaw, index), 4);
        assert_eq!(offset_of!(RegionRaw, start), 8);
        assert_eq!(offset_of!(RegionRaw, size), 16);
        assert_eq!(offset_of!(RegionRaw, offset), 24);
        assert_eq!(offset_of!(IrqRaw, index), 4);
        assert_eq!(offset_of!(PowerRaw, kind), 4);
        assert_eq!(offset_of!(PowerRaw, op), 8);
        assert_eq!(offset_of!(PowerRaw, handle), 12);
        assert_eq!(offset_of!(PowerRaw, name), 16);
        assert_eq!(offset_of!(PowerRaw, value), 48);
        assert_eq!(offset_of!(PowerRaw, value2), 56);
        assert_eq!(offset_of!(Event, index), 4);
        assert_eq!(offset_of!(Event, count), 8);
        assert_eq!(offset_of!(Event, kind), 12);
        assert_eq!(offset_of!(Event, ns), 16);
        assert_eq!(offset_of!(Event, value), 24);
    }

    /// The block requests as `kernel/lemnos.h` defines them, read from the header itself, so a
    /// change there that this file doesn't follow fails here.
    #[test]
    fn block_requests_are_the_headers() {
        let header = include_str!("../../../kernel/lemnos.h");
        let defined = |name: &str, text: &str| {
            header.lines().any(|line| {
                let mut words = line.split_whitespace();
                words.next() == Some("#define")
                    && words.next() == Some(name)
                    && words.collect::<Vec<_>>().join(" ") == text
            })
        };
        assert!(defined("LEMNOS_ABI", "1"));
        assert!(defined("LEMNOS_NAME_MAX", "64"));
        assert!(defined(
            "LEMNOS_IOC_NEXT",
            "_IOWR('L', 0xcf, struct lemnos_next)"
        ));
        assert!(defined(
            "LEMNOS_IOC_INFO",
            "_IOWR('L', 0xd0, struct lemnos_info)"
        ));
        assert!(defined(
            "LEMNOS_IOC_ALLOC",
            "_IOWR('L', 0xd2, struct lemnos_alloc)"
        ));
        assert!(defined(
            "LEMNOS_IOC_IMPORT",
            "_IOWR('L', 0xd3, struct lemnos_import)"
        ));
        assert!(defined(
            "LEMNOS_IOC_RELEASE",
            "_IOW('L', 0xd4, struct lemnos_release)"
        ));
        // Each structure's fields, in order, as the header lists them.
        let fields = |name: &str| -> Vec<String> {
            let start = header
                .find(&format!("struct {name} {{"))
                .unwrap_or_else(|| panic!("struct {name}"));
            let body = &header[start..start + header[start..].find("};").unwrap()];
            body.lines()
                .skip(1)
                .filter_map(|line| {
                    let decl = line.split(';').next()?.trim();
                    (!decl.is_empty() && !decl.starts_with("/*")).then(|| decl.to_string())
                })
                .collect()
        };
        assert_eq!(
            fields("lemnos_next"),
            ["__u32 after", "__u32 block", "char name[LEMNOS_NAME_MAX]"]
        );
        assert_eq!(
            fields("lemnos_info"),
            [
                "__u32 block",
                "__u32 abi",
                "__u32 flags",
                "__u32 regions",
                "__u32 irqs",
                "__u32 dma_bits",
                "__u32 reserved[2]"
            ]
        );
        assert_eq!(
            fields("lemnos_alloc"),
            [
                "__u32 block",
                "__u32 flags",
                "__u64 size",
                "__u64 addr",
                "__s32 fd",
                "__u32 reserved"
            ]
        );
        assert_eq!(
            fields("lemnos_import"),
            [
                "__u32 block",
                "__s32 fd",
                "__u64 addr",
                "__u64 size",
                "__u32 handle",
                "__u32 reserved"
            ]
        );
        assert_eq!(fields("lemnos_release"), ["__u32 block", "__u32 handle"]);
        assert!(defined("LEMNOS_INFO_MMIO_MAPPABLE", "(1u << 0)"));
        assert!(defined("LEMNOS_INFO_MMIO_UNCONFINED", "(1u << 1)"));
        assert!(defined("LEMNOS_POWER_NAME", "32"));
        assert!(defined(
            "LEMNOS_IOC_REGION",
            "_IOWR('L', 0xd1, struct lemnos_region)"
        ));
        assert!(defined(
            "LEMNOS_IOC_IRQ_ENABLE",
            "_IOW('L', 0xd5, struct lemnos_irq)"
        ));
        assert!(defined(
            "LEMNOS_IOC_POWER",
            "_IOWR('L', 0xd6, struct lemnos_power)"
        ));
        assert!(defined(
            "LEMNOS_IOC_TEST_IRQ",
            "_IOW('L', 0xdf, struct lemnos_irq)"
        ));
        for (name, value) in [
            ("LEMNOS_EVENT_IRQ", EVENT_IRQ),
            ("LEMNOS_EVENT_BLOCK_BOUND", EVENT_BLOCK_BOUND),
            ("LEMNOS_EVENT_BLOCK_UNBOUND", EVENT_BLOCK_UNBOUND),
            ("LEMNOS_POWER_CLOCK", power::CLOCK),
            ("LEMNOS_POWER_SUPPLY", power::SUPPLY),
            ("LEMNOS_POWER_RESET", power::RESET),
            ("LEMNOS_POWER_ENABLE", power::ENABLE),
            ("LEMNOS_POWER_DISABLE", power::DISABLE),
            ("LEMNOS_POWER_STATUS", power::STATUS),
            ("LEMNOS_POWER_GET", power::GET),
            ("LEMNOS_POWER_SET", power::SET),
            ("LEMNOS_POWER_PULSE", power::PULSE),
        ] {
            // The header follows a value with a tab and a comment on some of these.
            let found = header.lines().any(|line| {
                let mut words = line.split_whitespace();
                words.next() == Some("#define")
                    && words.next() == Some(name)
                    && words.next() == Some(value.to_string().as_str())
            });
            assert!(found, "{name} = {value}");
        }
        assert_eq!(
            fields("lemnos_region"),
            [
                "__u32 block",
                "__u32 index",
                "__u64 start",
                "__u64 size",
                "__u64 offset"
            ]
        );
        assert_eq!(fields("lemnos_irq"), ["__u32 block", "__u32 index"]);
        assert_eq!(
            fields("lemnos_event"),
            [
                "__u32 block",
                "__u32 index",
                "__u32 count",
                "__u32 kind",
                "__u64 ns",
                "__s64 value"
            ]
        );
        assert_eq!(
            fields("lemnos_power"),
            [
                "__u32 block",
                "__u32 kind",
                "__u32 op",
                "__u32 handle",
                "char name[LEMNOS_POWER_NAME]",
                "__u64 value",
                "__u64 value2"
            ]
        );
    }

    #[test]
    fn block_names_that_cant_be_one_are_refused_before_the_kernel() {
        let file = std::fs::File::open("/dev/null").unwrap();
        let fd = std::os::fd::AsFd::as_fd(&file);
        for name in ["", "a\0b", &"x".repeat(64)] {
            let error = find_block(fd, name).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{name:?}");
        }
    }

    #[test]
    fn requests_are_the_kernels() {
        assert_eq!(CLAIM.nr(), 0xc048_4cce);
        assert_eq!(I2C_TRANSFER, 0x4018_4ca8);
        assert_eq!(SPI_TRANSFER, 0x4020_4caa);
        assert_eq!(PWM_GET.nr(), 0xc020_4cba);
        assert_eq!(NEXT.nr(), 0xc048_4ccf);
        assert_eq!(INFO.nr(), 0xc020_4cd0);
        assert_eq!(ALLOC.nr(), 0xc020_4cd2);
        assert_eq!(IMPORT.nr(), 0xc020_4cd3);
        assert_eq!(RELEASE.nr(), 0x4008_4cd4);
        assert_eq!(REGION.nr(), 0xc020_4cd1);
        assert_eq!(IRQ_ENABLE.nr(), 0x4008_4cd5);
        assert_eq!(POWER.nr(), 0xc040_4cd6);
        assert_eq!(TEST_IRQ.nr(), 0x4008_4cdf);
    }

    #[test]
    fn power_names_that_cant_be_one_are_refused_before_the_kernel() {
        let file = std::fs::File::open("/dev/null").unwrap();
        let fd = std::os::fd::AsFd::as_fd(&file);
        for name in ["a\0b", &"x".repeat(POWER_NAME)] {
            let error = power(fd, 1, (power::CLOCK, power::STATUS), name, (0, 0)).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{name:?}");
        }
        assert_eq!(
            read_events(fd, &mut []).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn an_edge_decodes() {
        let mut bytes = [0; GPIO_EVENT_SIZE];
        bytes[0] = 9;
        bytes[8] = 2;
        bytes[12] = 1;
        assert_eq!(
            GpioEvent::from_bytes(&bytes),
            GpioEvent {
                ns: 9,
                seq: 2,
                rising: 1
            }
        );
    }
}
