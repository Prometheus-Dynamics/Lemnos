//! The GPIO character device, uAPI v2 (`/dev/gpiochipN`, Linux 5.10+).
//!
//! The structures carry no pointers, so they are public and plain; the ioctls
//! are wrapped in safe functions. Building requests is `lemnos-linux`'s job.
//! Ported from Styx (`styx-kernel/src/bus/gpio.rs`).

use crate::ioctl::{ioctl_ptr, ior, iowr};
use std::io;
use std::os::fd::{BorrowedFd, FromRawFd, OwnedFd};

/// `GPIO_MAX_NAME_SIZE`.
pub const MAX_NAME_SIZE: usize = 32;
/// Most lines one request can hold (`GPIO_V2_LINES_MAX`).
pub const MAX_LINES: usize = 64;
/// Most configuration attributes (`GPIO_V2_LINE_NUM_ATTRS_MAX`).
pub const NUM_ATTRS_MAX: usize = 10;

/// `GPIO_V2_LINE_FLAG_*`.
pub mod flag {
    /// Requested by someone (line info only).
    pub const USED: u64 = 1 << 0;
    /// Active-low.
    pub const ACTIVE_LOW: u64 = 1 << 1;
    /// Input.
    pub const INPUT: u64 = 1 << 2;
    /// Output.
    pub const OUTPUT: u64 = 1 << 3;
    /// Rising-edge events.
    pub const EDGE_RISING: u64 = 1 << 4;
    /// Falling-edge events.
    pub const EDGE_FALLING: u64 = 1 << 5;
    /// Open drain.
    pub const OPEN_DRAIN: u64 = 1 << 6;
    /// Open source.
    pub const OPEN_SOURCE: u64 = 1 << 7;
    /// Pull-up.
    pub const BIAS_PULL_UP: u64 = 1 << 8;
    /// Pull-down.
    pub const BIAS_PULL_DOWN: u64 = 1 << 9;
    /// Bias disabled.
    pub const BIAS_DISABLED: u64 = 1 << 10;
    /// Event timestamps from `CLOCK_REALTIME`.
    pub const EVENT_CLOCK_REALTIME: u64 = 1 << 11;
}

/// `GPIO_V2_LINE_ATTR_ID_*`.
pub mod attr {
    /// The attribute holds flags.
    pub const FLAGS: u32 = 1;
    /// The attribute holds output values.
    pub const OUTPUT_VALUES: u32 = 2;
    /// The attribute holds a debounce period in microseconds.
    pub const DEBOUNCE: u32 = 3;
}

/// `GPIO_V2_LINE_EVENT_RISING_EDGE`.
pub const EVENT_RISING_EDGE: u32 = 1;
/// `GPIO_V2_LINE_EVENT_FALLING_EDGE`.
pub const EVENT_FALLING_EDGE: u32 = 2;

/// `struct gpiochip_info`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ChipInfo {
    /// Kernel name (`gpiochipN`), NUL-terminated.
    pub name: [u8; MAX_NAME_SIZE],
    /// Label (e.g. `pinctrl-rp1`), NUL-terminated.
    pub label: [u8; MAX_NAME_SIZE],
    /// Number of lines.
    pub lines: u32,
}

/// `struct gpio_v2_line_attribute` (the union is 8 bytes: flags / values /
/// debounce period).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LineAttribute {
    /// [`attr`] id.
    pub id: u32,
    /// Reserved.
    pub padding: u32,
    /// Flags, values bitmap, or debounce microseconds (low 32 bits).
    pub value: u64,
}

/// `struct gpio_v2_line_config_attribute`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LineConfigAttribute {
    /// The attribute.
    pub attr: LineAttribute,
    /// Lines (by request index) it applies to.
    pub mask: u64,
}

/// `struct gpio_v2_line_config`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LineConfig {
    /// Flags of lines no attribute overrides.
    pub flags: u64,
    /// Attributes used.
    pub num_attrs: u32,
    /// Reserved.
    pub padding: [u32; 5],
    /// Per-line overrides.
    pub attrs: [LineConfigAttribute; NUM_ATTRS_MAX],
}

/// `struct gpio_v2_line_request`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineRequest {
    /// Chip offsets of the requested lines.
    pub offsets: [u32; MAX_LINES],
    /// Consumer label, NUL-terminated.
    pub consumer: [u8; MAX_NAME_SIZE],
    /// Configuration.
    pub config: LineConfig,
    /// Lines used in `offsets`.
    pub num_lines: u32,
    /// Event queue size hint (0: kernel default).
    pub event_buffer_size: u32,
    /// Reserved.
    pub padding: [u32; 5],
    /// Set by the kernel: the request's descriptor.
    pub fd: i32,
}

impl Default for LineRequest {
    fn default() -> Self {
        Self {
            offsets: [0; MAX_LINES],
            consumer: [0; MAX_NAME_SIZE],
            config: LineConfig::default(),
            num_lines: 0,
            event_buffer_size: 0,
            padding: [0; 5],
            fd: -1,
        }
    }
}

/// `struct gpio_v2_line_values`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LineValues {
    /// Values by request index.
    pub bits: u64,
    /// Which indexes to read or set.
    pub mask: u64,
}

/// `struct gpio_v2_line_info`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct LineInfo {
    /// Line name, NUL-terminated.
    pub name: [u8; MAX_NAME_SIZE],
    /// Consumer label, NUL-terminated.
    pub consumer: [u8; MAX_NAME_SIZE],
    /// Offset on the chip.
    pub offset: u32,
    /// Attributes used.
    pub num_attrs: u32,
    /// [`flag`] bits.
    pub flags: u64,
    /// Attributes (e.g. the debounce period).
    pub attrs: [LineAttribute; NUM_ATTRS_MAX],
    /// Reserved.
    pub padding: [u32; 4],
}

impl LineInfo {
    fn zeroed(offset: u32) -> Self {
        Self {
            name: [0; MAX_NAME_SIZE],
            consumer: [0; MAX_NAME_SIZE],
            offset,
            num_attrs: 0,
            flags: 0,
            attrs: [LineAttribute::default(); NUM_ATTRS_MAX],
            padding: [0; 4],
        }
    }
}

/// `struct gpio_v2_line_event`, as read from a line request descriptor.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LineEvent {
    /// Nanoseconds on `CLOCK_MONOTONIC` (or the requested clock).
    pub timestamp_ns: u64,
    /// [`EVENT_RISING_EDGE`] or [`EVENT_FALLING_EDGE`].
    pub id: u32,
    /// Chip offset of the line.
    pub offset: u32,
    /// Sequence number across the request.
    pub seqno: u32,
    /// Sequence number on this line.
    pub line_seqno: u32,
    /// Reserved.
    pub padding: [u32; 6],
}

/// Bytes of one [`LineEvent`] on the wire.
pub const LINE_EVENT_SIZE: usize = size_of::<LineEvent>();

impl LineEvent {
    /// Decodes one event read from a request descriptor (native endianness).
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < LINE_EVENT_SIZE {
            return None;
        }
        let u32_at =
            |i: usize| u32::from_ne_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]);
        let mut ts = [0u8; 8];
        ts.copy_from_slice(&bytes[..8]);
        Some(Self {
            timestamp_ns: u64::from_ne_bytes(ts),
            id: u32_at(8),
            offset: u32_at(12),
            seqno: u32_at(16),
            line_seqno: u32_at(20),
            padding: [0; 6],
        })
    }
}

const GET_CHIPINFO: u32 = ior::<ChipInfo>(0xb4, 0x01);
const GET_LINEINFO: u32 = iowr::<LineInfo>(0xb4, 0x05);
const GET_LINE: u32 = iowr::<LineRequest>(0xb4, 0x07);
const LINE_SET_CONFIG: u32 = iowr::<LineConfig>(0xb4, 0x0d);
const LINE_GET_VALUES: u32 = iowr::<LineValues>(0xb4, 0x0e);
const LINE_SET_VALUES: u32 = iowr::<LineValues>(0xb4, 0x0f);

/// Chip information (`GPIO_GET_CHIPINFO_IOCTL`) from a chip descriptor.
pub fn chip_info(chip: BorrowedFd<'_>) -> io::Result<ChipInfo> {
    let mut info = ChipInfo {
        name: [0; MAX_NAME_SIZE],
        label: [0; MAX_NAME_SIZE],
        lines: 0,
    };
    // SAFETY: GPIO_GET_CHIPINFO_IOCTL fills one `gpiochip_info`, which `info`
    // is (a live local of the encoded type).
    unsafe { ioctl_ptr(chip, GET_CHIPINFO, &mut info) }?;
    Ok(info)
}

/// Information about the line at `offset` (`GPIO_V2_GET_LINEINFO_IOCTL`).
pub fn line_info(chip: BorrowedFd<'_>, offset: u32) -> io::Result<LineInfo> {
    let mut info = LineInfo::zeroed(offset);
    // SAFETY: GPIO_V2_GET_LINEINFO_IOCTL reads the offset from and fills one
    // `gpio_v2_line_info`, which `info` is.
    unsafe { ioctl_ptr(chip, GET_LINEINFO, &mut info) }?;
    Ok(info)
}

/// Requests lines (`GPIO_V2_GET_LINE_IOCTL`); returns the request's
/// descriptor, which releases the lines when closed. Fails with `EBUSY` if a
/// line is held.
pub fn request_lines(chip: BorrowedFd<'_>, request: &LineRequest) -> io::Result<OwnedFd> {
    let mut req = *request;
    req.fd = -1;
    // SAFETY: GPIO_V2_GET_LINE_IOCTL reads and fills one
    // `gpio_v2_line_request`, which `req` is (a live local copy).
    unsafe { ioctl_ptr(chip, GET_LINE, &mut req) }?;
    if req.fd < 0 {
        return Err(io::Error::other("GPIO line request returned no descriptor"));
    }
    // SAFETY: the kernel returned a new descriptor that nothing else owns.
    Ok(unsafe { OwnedFd::from_raw_fd(req.fd) })
}

/// Changes the configuration of requested lines
/// (`GPIO_V2_LINE_SET_CONFIG_IOCTL`) without releasing them.
pub fn set_config(lines: BorrowedFd<'_>, config: &LineConfig) -> io::Result<()> {
    let mut config = *config;
    // SAFETY: GPIO_V2_LINE_SET_CONFIG_IOCTL reads one `gpio_v2_line_config`,
    // which `config` is (a live local copy).
    unsafe { ioctl_ptr(lines, LINE_SET_CONFIG, &mut config) }?;
    Ok(())
}

/// Reads the values of the lines in `mask` (`GPIO_V2_LINE_GET_VALUES_IOCTL`).
pub fn get_values(lines: BorrowedFd<'_>, mask: u64) -> io::Result<u64> {
    let mut values = LineValues { bits: 0, mask };
    // SAFETY: GPIO_V2_LINE_GET_VALUES_IOCTL reads and fills one
    // `gpio_v2_line_values`, which `values` is.
    unsafe { ioctl_ptr(lines, LINE_GET_VALUES, &mut values) }?;
    Ok(values.bits & mask)
}

/// Sets the values of the lines in `values.mask`
/// (`GPIO_V2_LINE_SET_VALUES_IOCTL`).
pub fn set_values(lines: BorrowedFd<'_>, values: LineValues) -> io::Result<()> {
    let mut values = values;
    // SAFETY: GPIO_V2_LINE_SET_VALUES_IOCTL reads one `gpio_v2_line_values`,
    // which `values` is (a live local copy).
    unsafe { ioctl_ptr(lines, LINE_SET_VALUES, &mut values) }?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::offset_of;
    use std::os::fd::AsFd;

    #[test]
    fn struct_layouts_match_the_kernel() {
        assert_eq!(size_of::<ChipInfo>(), 68);
        assert_eq!(size_of::<LineAttribute>(), 16);
        assert_eq!(size_of::<LineConfigAttribute>(), 24);
        assert_eq!(size_of::<LineConfig>(), 272);
        assert_eq!(size_of::<LineRequest>(), 592);
        assert_eq!(offset_of!(LineRequest, fd), 588);
        assert_eq!(size_of::<LineValues>(), 16);
        assert_eq!(size_of::<LineInfo>(), 256);
        assert_eq!(size_of::<LineEvent>(), 48);
        assert_eq!(GET_LINE, 0xc250_b407);
        assert_eq!(LINE_SET_VALUES, 0xc010_b40f);
        assert_eq!(GET_LINEINFO, 0xc100_b405);
        assert_eq!(LINE_SET_CONFIG, 0xc110_b40d);
    }

    #[test]
    fn decodes_events() {
        let mut raw = [0u8; LINE_EVENT_SIZE];
        raw[..8].copy_from_slice(&123_456u64.to_ne_bytes());
        raw[8..12].copy_from_slice(&EVENT_FALLING_EDGE.to_ne_bytes());
        raw[12..16].copy_from_slice(&17u32.to_ne_bytes());
        raw[16..20].copy_from_slice(&5u32.to_ne_bytes());
        raw[20..24].copy_from_slice(&2u32.to_ne_bytes());
        let ev = LineEvent::from_bytes(&raw).unwrap();
        assert_eq!(
            (ev.timestamp_ns, ev.id, ev.offset, ev.seqno, ev.line_seqno),
            (123_456, EVENT_FALLING_EDGE, 17, 5, 2)
        );
        assert!(LineEvent::from_bytes(&raw[..40]).is_none());
    }

    /// Reads chip and line info from every chip present, read-only.
    #[test]
    fn reads_present_chips() {
        let Ok(dir) = std::fs::read_dir("/dev") else {
            return;
        };
        for entry in dir.flatten() {
            if !entry.file_name().to_string_lossy().starts_with("gpiochip") {
                continue;
            }
            let Ok(file) = std::fs::File::open(entry.path()) else {
                continue;
            };
            let info = chip_info(file.as_fd()).unwrap();
            if info.lines > 0 {
                assert_eq!(line_info(file.as_fd(), 0).unwrap().offset, 0);
            }
        }
    }
}
