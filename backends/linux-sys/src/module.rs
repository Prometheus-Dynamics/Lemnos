//! The Lemnos kernel module's control node, `/dev/lemnos`: claims, and the buses, lines and
//! channels that belong to them. The interface is `kernel/lemnos.h`.

use std::io;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd};

use crate::i2c::Message;
use crate::ioctl::{Ioctl, from_bytes, ioctl_ptr, iow, iowr, pod, zeroed};
use crate::spi::Transfer;

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

#[cfg(test)]
mod tests {
    use super::*;

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
    }

    #[test]
    fn requests_are_the_kernels() {
        assert_eq!(CLAIM.nr(), 0xc048_4cce);
        assert_eq!(I2C_TRANSFER, 0x4018_4ca8);
        assert_eq!(SPI_TRANSFER, 0x4020_4caa);
        assert_eq!(PWM_GET.nr(), 0xc020_4cba);
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
