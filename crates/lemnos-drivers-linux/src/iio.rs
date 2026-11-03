//! Buffered reads from an IIO device: the `/dev/iio:deviceN` character device
//! with its scan elements enabled, its trigger set and its buffer started.
//!
//! Each record is decoded with the scan elements' `_type` descriptors
//! (endianness, signedness, bits, storage bits, shift and repeat). The
//! timestamp element, which the kernel takes in the IRQ (or in the trigger's
//! handler), becomes the record's time on the readings' clock:
//! `CLOCK_BOOTTIME` in microseconds, as `Reading::timestamp_us`.
//!
//! The record layout is the kernel's (`Documentation/iio/iio_devbuf.rst`, and
//! `iio_compute_scan_bytes` in `drivers/iio/industrialio-buffer.c`): each
//! element starts at a multiple of its own span (its storage bytes, times the
//! repeat count rounded up to a power of two), the timestamp at a multiple of
//! its span after the data, and the record is padded to its largest element.
//!
//! Setup and teardown follow the order the kernel accepts: the clock, trigger,
//! scan elements, length and watermark are written while the buffer is idle;
//! `buffer/enable` starts it; teardown stops it first, then clears the scan
//! elements and the trigger. Nothing here is tested on a board with IIO
//! hardware; the host tests run against a fake sysfs tree, with a regular file
//! standing in for the character device.

use crate::{SysfsError, sysfs};
use lemnos_device::NO_VALUE;
use lemnos_hal::{ErrorKind, HalError};
use lemnos_linux_sys::poll::{POLLIN, open_nonblocking, poll_one};
use lemnos_linux_sys::time::boottime_us_of_monotonic_ns;
use std::fs::File;
use std::io::{self, Read};
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The clock of a buffer's IRQ timestamps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Clock {
    /// `boottime`: the readings' clock already, so stamps pass through.
    Boottime,
    /// `monotonic`, the kernel's default: converted to the boot clock per
    /// record (the offset between the two is read at each conversion).
    Monotonic,
}

/// How a buffer is set up. The defaults keep the kernel's own choices.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BufferConfig {
    /// The trigger's name (`trigger/current_trigger`). `None` leaves the
    /// device's trigger as it is.
    pub trigger: Option<String>,
    /// Capacity in records (`buffer/length`). 0 keeps the kernel's default.
    pub length: u32,
    /// Records that make the descriptor readable (`buffer/watermark`). 0
    /// keeps the kernel's default of one.
    pub watermark: u32,
    /// The character device. `None`: `/dev/<device name>`, such as
    /// `/dev/iio:device0`.
    pub node: Option<PathBuf>,
}

/// One scan element's `_type` descriptor: `[be|le]:[s|u]bits/storagebits`,
/// then `X<repeat>` when the repeat is above one, then `>>shift`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanType {
    pub big_endian: bool,
    pub signed: bool,
    /// The valid data bits.
    pub bits: u8,
    /// The bits one value occupies in the record (8, 16, 32 or 64).
    pub storage_bits: u8,
    /// The right shift applied before the valid bits are masked out.
    pub shift: u8,
    /// Values per element (1 when the descriptor has no repeat).
    pub repeat: u8,
}

impl ScanType {
    /// Parses a `_type` attribute. `None` when the text does not match the
    /// format, or the sizes do not fit (`bits` and `shift` inside the storage
    /// bits; floating-point elements are not decoded).
    pub fn parse(text: &str) -> Option<Self> {
        let (endian, rest) = text.split_once(':')?;
        let big_endian = match endian {
            "be" => true,
            "le" => false,
            _ => return None,
        };
        let (signed, rest) = match rest.strip_prefix('s') {
            Some(rest) => (true, rest),
            None => (false, rest.strip_prefix('u')?),
        };
        let (rest, shift) = match rest.split_once(">>") {
            Some((rest, shift)) => (rest, shift.parse().ok()?),
            None => (rest, 0),
        };
        let (rest, repeat) = match rest.split_once('X') {
            Some((rest, repeat)) => (rest, repeat.parse().ok()?),
            None => (rest, 1),
        };
        let (bits, storage) = rest.split_once('/')?;
        let ty = Self {
            big_endian,
            signed,
            bits: bits.parse().ok()?,
            storage_bits: storage.parse().ok()?,
            shift,
            repeat,
        };
        ty.is_valid().then_some(ty)
    }

    fn is_valid(&self) -> bool {
        matches!(self.storage_bits, 8 | 16 | 32 | 64)
            && self.bits >= 1
            && self.bits <= self.storage_bits
            && u16::from(self.shift) + u16::from(self.bits) <= u16::from(self.storage_bits)
            && self.repeat >= 1
    }

    /// Bytes of one value.
    fn storage_bytes(&self) -> usize {
        usize::from(self.storage_bits / 8)
    }

    /// Bytes an element takes in a record, the alignment unit the kernel
    /// uses: its repeats, padded to a power of two.
    pub fn span(&self) -> usize {
        self.storage_bytes() * usize::from(self.repeat).next_power_of_two()
    }

    /// The value at the start of `bytes`: the storage bytes read in the
    /// descriptor's order, shifted, masked to the valid bits and sign-extended
    /// when signed. `bytes` must hold at least one storage value.
    pub fn decode(&self, bytes: &[u8]) -> i64 {
        let n = self.storage_bytes();
        let word = if self.big_endian {
            bytes[..n]
                .iter()
                .fold(0u64, |word, &b| (word << 8) | u64::from(b))
        } else {
            bytes[..n]
                .iter()
                .rev()
                .fold(0u64, |word, &b| (word << 8) | u64::from(b))
        };
        let bits = u32::from(self.bits);
        let mask = if bits == 64 {
            u64::MAX
        } else {
            (1u64 << bits) - 1
        };
        let value = (word >> self.shift) & mask;
        if self.signed && bits < 64 && ((value >> (bits - 1)) & 1) == 1 {
            value as i64 - (1i64 << bits)
        } else {
            value as i64
        }
    }
}

/// One scan element of a record: its name (the `_en` attribute's stem), its
/// offset in the record and its descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub name: String,
    pub offset: usize,
    pub ty: ScanType,
}

/// The layout of one record: the data elements in scan-index order, then the
/// timestamp, with the record size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    pub fields: Vec<Field>,
    pub timestamp: Option<Field>,
    pub record_bytes: usize,
}

impl Layout {
    /// The layout of the enabled elements `(name, scan index, type)` and the
    /// timestamp element's type, if one is enabled. The kernel's rules: each
    /// element at a multiple of its span, the timestamp at a multiple of its
    /// span after the data, and the record padded to the largest span.
    pub fn new(mut elements: Vec<(String, u32, ScanType)>, timestamp: Option<ScanType>) -> Self {
        elements.sort_by_key(|(_, index, _)| *index);
        let mut bytes = 0;
        let mut largest = 0;
        let mut fields = Vec::with_capacity(elements.len());
        for (name, _, ty) in elements {
            let span = ty.span();
            bytes = align(bytes, span);
            fields.push(Field {
                name,
                offset: bytes,
                ty,
            });
            bytes += span;
            largest = largest.max(span);
        }
        let timestamp = timestamp.map(|ty| {
            let span = ty.span();
            bytes = align(bytes, span);
            let field = Field {
                name: "in_timestamp".to_string(),
                offset: bytes,
                ty,
            };
            bytes += span;
            largest = largest.max(span);
            field
        });
        Self {
            fields,
            timestamp,
            record_bytes: align(bytes, largest),
        }
    }
}

fn align(value: usize, unit: usize) -> usize {
    value.div_ceil(unit) * unit
}

/// A channel of the binding that a buffer serves.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct BoundChannel {
    /// The scan element's name (`in_accel_x`).
    pub(crate) attribute: &'static str,
    /// Index into `DeviceInfo::channels`.
    pub(crate) channel: usize,
    /// The power of ten from the kernel's unit to the canonical unit.
    pub(crate) shift: i32,
    /// `(raw + offset) × scale` gives the kernel's unit.
    pub(crate) offset: f64,
    pub(crate) scale: f64,
}

/// One decoded value of a record: where it is, what it is and how to convert.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Tap {
    offset: usize,
    ty: ScanType,
    /// The output position and conversion; `None` for an element that is
    /// enabled for another consumer and is only skipped over.
    out: Option<Conversion>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Conversion {
    position: usize,
    offset: f64,
    scale: f64,
    shift: i32,
}

/// A buffered IIO device. Dropping it stops the buffer and restores the
/// scan elements, the trigger and the clock it changed.
#[derive(Debug)]
pub struct IioBuffer {
    // Declared before `session`, so the descriptor closes before the
    // buffer is stopped.
    node: File,
    node_path: PathBuf,
    session: Session,
    taps: Vec<Tap>,
    timestamp: Option<Tap>,
    channels: Vec<usize>,
    clock: Clock,
    record_bytes: usize,
    raw: Vec<u8>,
}

impl IioBuffer {
    /// Sets up the buffer of the IIO device at `dir` (a class directory such
    /// as `bus/iio/devices/iio:device0`) for `bound` channels, and opens its
    /// character device. Fails if the buffer is already running (another
    /// consumer), or the device has no timestamp element.
    pub(crate) fn open(
        dir: &Path,
        config: &BufferConfig,
        bound: &[BoundChannel],
    ) -> Result<Self, SysfsError> {
        let names: Vec<&str> = bound.iter().map(|b| b.attribute).collect();
        let (session, clock) = Session::start(dir, config, &names)?;
        let (layout, enabled) = session.layout()?;
        for name in &names {
            if !enabled.iter().any(|e| e == name) {
                return Err(SysfsError::new(
                    ErrorKind::Failed,
                    dir,
                    format!("{name} is not enabled after the setup"),
                ));
            }
        }
        let node_path = match &config.node {
            Some(path) => path.clone(),
            None => PathBuf::from("/dev").join(
                dir.file_name()
                    .ok_or_else(|| SysfsError::new(ErrorKind::Configuration, dir, "no name"))?,
            ),
        };
        let node =
            open_nonblocking(&node_path).map_err(|e| SysfsError::io(&node_path, "open", &e))?;
        let capacity = sysfs::read_parsed_optional::<usize>(&dir.join("buffer/length"))?
            .unwrap_or(config.length.max(1) as usize)
            .max(1);
        let (taps, timestamp, channels) = plan(&layout, bound, dir)?;
        let record_bytes = layout.record_bytes;
        Ok(Self {
            node,
            node_path,
            session,
            taps,
            timestamp,
            channels,
            clock,
            record_bytes,
            raw: vec![0; capacity * record_bytes],
        })
    }

    /// The channel index (into `DeviceInfo::channels`) of each value a record
    /// yields, in the order [`IioBuffer::read`] writes them.
    pub fn channels(&self) -> &[usize] {
        &self.channels
    }

    /// The bytes of one record in the character device.
    pub fn record_bytes(&self) -> usize {
        self.record_bytes
    }

    /// The clock the timestamps are on before conversion.
    pub fn clock(&self) -> Clock {
        self.clock
    }

    /// Waits until the descriptor is readable (the watermark is reached, or
    /// `timeout` passes; `None` waits forever), then reads up to
    /// `stamps.len()` records. Each record writes [`IioBuffer::channels`]
    /// values in canonical units to `values` and its IRQ time, in
    /// microseconds on `CLOCK_BOOTTIME`, to `stamps`. After a timeout the
    /// samples already buffered are returned, below the watermark.
    ///
    /// Returns the number of records read (0 when none was buffered).
    pub fn read(
        &mut self,
        timeout: Option<Duration>,
        values: &mut [i32],
        stamps: &mut [u64],
    ) -> Result<usize, SysfsError> {
        let width = self.channels.len();
        let room = values.len().checked_div(width).unwrap_or(0);
        let records = stamps
            .len()
            .min(room)
            .min(self.raw.len() / self.record_bytes);
        if records == 0 {
            return Ok(0);
        }
        poll_one(self.node.as_fd(), POLLIN, timeout)
            .map_err(|e| SysfsError::io(&self.node_path, "poll", &e))?;
        let want = records * self.record_bytes;
        let got = match self.node.read(&mut self.raw[..want]) {
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => 0,
            Err(e) => return Err(SysfsError::io(&self.node_path, "read", &e)),
        };
        if got % self.record_bytes != 0 {
            return Err(SysfsError::new(
                ErrorKind::Failed,
                &self.node_path,
                format!(
                    "{got} bytes is not a whole number of {}-byte records",
                    self.record_bytes
                ),
            ));
        }
        let count = got / self.record_bytes;
        for (r, stamp) in stamps.iter_mut().enumerate().take(count) {
            let record = &self.raw[r * self.record_bytes..(r + 1) * self.record_bytes];
            for tap in &self.taps {
                let Some(conv) = tap.out else { continue };
                let raw = tap.ty.decode(&record[tap.offset..]);
                values[r * width + conv.position] = convert(raw as f64, conv);
            }
            *stamp = self.stamp_of(record);
        }
        Ok(count)
    }

    /// The record's IRQ time on the readings' clock, microseconds.
    fn stamp_of(&self, record: &[u8]) -> u64 {
        let Some(ts) = self.timestamp else { return 0 };
        let ns = ts.ty.decode(&record[ts.offset..]).max(0) as u64;
        match self.clock {
            Clock::Boottime => ns / 1_000,
            Clock::Monotonic => boottime_us_of_monotonic_ns(ns),
        }
    }

    /// Stops the buffer and restores what setup changed, reporting the first
    /// error. Dropping the buffer does the same, but ignores errors.
    pub fn close(self) -> Result<(), SysfsError> {
        let IioBuffer {
            node, mut session, ..
        } = self;
        drop(node);
        session.teardown()
    }
}

/// The taps, the timestamp tap and the output channels of a buffer.
type Plan = (Vec<Tap>, Option<Tap>, Vec<usize>);

/// Builds the taps for a layout: one per value of each element, with a
/// conversion where the element is one of `bound`, and the timestamp apart.
fn plan(layout: &Layout, bound: &[BoundChannel], dir: &Path) -> Result<Plan, SysfsError> {
    let mut taps = Vec::new();
    let mut channels = Vec::new();
    for field in &layout.fields {
        let wanted = bound.iter().find(|b| b.attribute == field.name);
        let storage = field.ty.storage_bytes();
        // A repeated element maps its channel to the first value only; the
        // further values are decoded past and dropped.
        let position = wanted.map(|b| {
            channels.push(b.channel);
            channels.len() - 1
        });
        for k in 0..usize::from(field.ty.repeat) {
            let out = match (k, wanted, position) {
                (0, Some(b), Some(position)) => Some(Conversion {
                    position,
                    offset: b.offset,
                    scale: b.scale,
                    shift: b.shift,
                }),
                _ => None,
            };
            taps.push(Tap {
                offset: field.offset + k * storage,
                ty: field.ty,
                out,
            });
        }
    }
    for b in bound {
        if !layout.fields.iter().any(|f| f.name == b.attribute) {
            return Err(SysfsError::new(
                ErrorKind::Failed,
                dir,
                format!("{} is not an enabled scan element", b.attribute),
            ));
        }
    }
    let timestamp = layout.timestamp.as_ref().map(|field| Tap {
        offset: field.offset,
        ty: field.ty,
        out: None,
    });
    Ok((taps, timestamp, channels))
}

/// The canonical value of a raw one: `(raw + offset) × scale` in the kernel's
/// unit, moved by `shift` powers of ten. The same rule as the snapshot read.
fn convert(raw: f64, conv: Conversion) -> i32 {
    canonical((raw + conv.offset) * conv.scale, conv.shift)
}

/// `value` in the canonical unit: `value × 10^shift`, rounded, clamped to the
/// `i32` range below `NO_VALUE`. `NO_VALUE` for a value that is not finite.
pub(crate) fn canonical(value: f64, shift: i32) -> i32 {
    let scaled = (value * 10f64.powi(shift)).round();
    if scaled.is_finite() {
        scaled.clamp(f64::from(NO_VALUE) + 1.0, f64::from(i32::MAX)) as i32
    } else {
        NO_VALUE
    }
}

/// The part of a buffer's setup that touches sysfs. Dropping it tears down
/// whatever it changed, so a failed setup leaves the device as it found it.
#[derive(Debug)]
struct Session {
    dir: PathBuf,
    /// Scan elements this session enabled.
    enabled: Vec<String>,
    /// Whether this session started the buffer.
    running: bool,
    /// The trigger before this session set one (`""` for none).
    trigger: Option<String>,
    /// The clock before this session set `boottime`.
    clock: Option<String>,
}

impl Session {
    /// Sets up the buffer: clock, trigger, scan elements, length and
    /// watermark, then `buffer/enable`. Returns the session and the clock.
    fn start(
        dir: &Path,
        config: &BufferConfig,
        names: &[&str],
    ) -> Result<(Self, Clock), SysfsError> {
        let mut session = Self {
            dir: dir.to_path_buf(),
            enabled: Vec::new(),
            running: false,
            trigger: None,
            clock: None,
        };
        let running = sysfs::read_optional(&dir.join("buffer/enable"))?;
        if running.as_deref().is_some_and(|v| v != "0") {
            return Err(SysfsError::new(
                ErrorKind::Busy,
                dir,
                "the buffer is already enabled (another consumer)",
            ));
        }
        let clock = session.set_clock()?;
        if let Some(name) = &config.trigger {
            let path = dir.join("trigger/current_trigger");
            let before = sysfs::read(&path)?;
            sysfs::write(&path, name)?;
            session.trigger = Some(before);
        }
        let timestamp = dir.join("scan_elements/in_timestamp_en");
        if sysfs::read_optional(&timestamp)?.is_none() {
            return Err(SysfsError::new(
                ErrorKind::Unsupported,
                &timestamp,
                "no timestamp element: the IRQ time is not available",
            ));
        }
        for name in names.iter().copied().chain(["in_timestamp"]) {
            sysfs::write(&dir.join(format!("scan_elements/{name}_en")), 1)?;
            session.enabled.push(name.to_string());
        }
        if config.length > 0 {
            sysfs::write(&dir.join("buffer/length"), config.length)?;
        }
        if config.watermark > 0 {
            sysfs::write(&dir.join("buffer/watermark"), config.watermark)?;
        }
        sysfs::write(&dir.join("buffer/enable"), 1)?;
        session.running = true;
        Ok((session, clock))
    }

    /// Puts the device's timestamps on `boottime` when it allows it. A
    /// device that only has `monotonic` keeps it, and its stamps are converted
    /// per record. Any other clock cannot be put on the readings' clock.
    fn set_clock(&mut self) -> Result<Clock, SysfsError> {
        let path = self.dir.join("current_timestamp_clock");
        let Some(before) = sysfs::read_optional(&path)? else {
            // Kernels without the attribute stamp on monotonic.
            return Ok(Clock::Monotonic);
        };
        if before == "boottime" {
            return Ok(Clock::Boottime);
        }
        match sysfs::write(&path, "boottime") {
            Ok(()) => {
                self.clock = Some(before);
                Ok(Clock::Boottime)
            }
            // A kernel without `boottime` stamps on monotonic: convert.
            Err(e) if e.kind() == ErrorKind::InvalidInput && before == "monotonic" => {
                Ok(Clock::Monotonic)
            }
            Err(e) if e.kind() == ErrorKind::InvalidInput => Err(SysfsError::new(
                ErrorKind::Unsupported,
                &path,
                format!("the timestamp clock {before} is not the readings' clock"),
            )),
            Err(e) => Err(e),
        }
    }

    /// The record layout the kernel reports: every enabled scan element
    /// (another consumer's too, since they take space in the record), and the
    /// enabled names.
    fn layout(&self) -> Result<(Layout, Vec<String>), SysfsError> {
        let scan = self.dir.join("scan_elements");
        let mut elements = Vec::new();
        let mut enabled = Vec::new();
        let mut timestamp = None;
        for entry in sysfs::entries(&scan)? {
            let Some(file) = entry.file_name().and_then(|f| f.to_str()) else {
                continue;
            };
            let Some(name) = file.strip_suffix("_en") else {
                continue;
            };
            if sysfs::read(&entry)? != "1" {
                continue;
            }
            let index: u32 = sysfs::read_parsed(&scan.join(format!("{name}_index")))?;
            let type_path = scan.join(format!("{name}_type"));
            let text = sysfs::read(&type_path)?;
            let ty = ScanType::parse(&text).ok_or_else(|| {
                SysfsError::new(
                    ErrorKind::Failed,
                    &type_path,
                    format!("unsupported scan type {text:?}"),
                )
            })?;
            enabled.push(name.to_string());
            if name == "in_timestamp" {
                timestamp = Some(ty);
            } else {
                elements.push((name.to_string(), index, ty));
            }
        }
        let timestamp = timestamp.ok_or_else(|| {
            SysfsError::new(
                ErrorKind::Unsupported,
                &scan,
                "no enabled timestamp element",
            )
        })?;
        Ok((Layout::new(elements, Some(timestamp)), enabled))
    }

    /// Stops the buffer, then clears the scan elements, the trigger and the
    /// clock, in that order (the kernel refuses the last three while the
    /// buffer runs). Reports the first error; later steps still run.
    fn teardown(&mut self) -> Result<(), SysfsError> {
        let mut first = Ok(());
        let mut keep = |result: Result<(), SysfsError>| {
            if first.is_ok() {
                first = result;
            }
        };
        if std::mem::take(&mut self.running) {
            keep(sysfs::write(&self.dir.join("buffer/enable"), 0));
        }
        for name in std::mem::take(&mut self.enabled) {
            keep(sysfs::write(
                &self.dir.join(format!("scan_elements/{name}_en")),
                0,
            ));
        }
        if let Some(before) = self.trigger.take() {
            let value = if before.is_empty() {
                "\n".to_string()
            } else {
                before
            };
            keep(sysfs::write(
                &self.dir.join("trigger/current_trigger"),
                value,
            ));
        }
        if let Some(before) = self.clock.take() {
            keep(sysfs::write(
                &self.dir.join("current_timestamp_clock"),
                before,
            ));
        }
        first
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.teardown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ty(text: &str) -> ScanType {
        ScanType::parse(text).unwrap_or_else(|| panic!("{text} parses"))
    }

    #[test]
    fn parses_the_kernel_descriptors() {
        // The kernel's own example: 12 valid bits in two bytes, shifted by 4.
        assert_eq!(
            ty("le:s12/16>>4"),
            ScanType {
                big_endian: false,
                signed: true,
                bits: 12,
                storage_bits: 16,
                shift: 4,
                repeat: 1,
            }
        );
        // The timestamp, and a repeat (the repeat comes before the shift).
        assert_eq!(ty("le:s64/64>>0").storage_bits, 64);
        let repeated = ty("be:u16/16X4>>0");
        assert_eq!((repeated.repeat, repeated.big_endian), (4, true));
        assert_eq!(repeated.span(), 2 * 4);
        // Repeat 3 pads to 4 for alignment.
        assert_eq!(ty("le:u8/8X3").span(), 4);
        // No shift is given: zero.
        assert_eq!(ty("be:u32/32").shift, 0);
    }

    #[test]
    fn rejects_what_it_cannot_decode() {
        assert_eq!(ScanType::parse("le:f32/32>>0"), None, "floats");
        assert_eq!(ScanType::parse("middle:s16/16"), None, "endianness");
        assert_eq!(ScanType::parse("le:s16/24"), None, "storage size");
        assert_eq!(ScanType::parse("le:s16/16>>4"), None, "shift and bits");
        assert_eq!(ScanType::parse("le:s12/8"), None, "bits above storage");
        assert_eq!(ScanType::parse("le:s12"), None, "no storage size");
    }

    #[test]
    fn decodes_endianness_sign_shift_and_mask() {
        // Two's complement -5 in 12 bits, stored above a 4-bit shift in 16 bits.
        let raw: u16 = (0xFFBu16) << 4;
        assert_eq!(ty("le:s12/16>>4").decode(&raw.to_le_bytes()), -5);
        assert_eq!(ty("be:s12/16>>4").decode(&raw.to_be_bytes()), -5);
        // Unsigned, with the bits above the valid ones ignored.
        assert_eq!(ty("le:u4/8>>0").decode(&[0xF3]), 3);
        assert_eq!(
            ty("be:u32/32").decode(&0xDEAD_BEEFu32.to_be_bytes()),
            0xDEAD_BEEF
        );
        assert_eq!(ty("le:s32/32").decode(&(-7i32).to_le_bytes()), -7);
        assert_eq!(
            ty("le:s64/64").decode(&1_234_567_890_123i64.to_le_bytes()),
            1_234_567_890_123
        );
    }

    #[test]
    fn layout_follows_the_kernels_doc_example() {
        // channel0 be:u16, channel1 and channel2 be:u32, channel3 be:u64.
        let elements = |names: &[(&str, u32, &str)]| -> Vec<(String, u32, ScanType)> {
            names
                .iter()
                .map(|(n, i, t)| ((*n).to_string(), *i, ty(t)))
                .collect()
        };
        let all = Layout::new(
            elements(&[
                ("c0", 0, "be:u16/16>>0"),
                ("c1", 1, "be:u32/32>>0"),
                ("c2", 2, "be:u32/32>>0"),
                ("c3", 3, "be:u64/64>>0"),
            ]),
            None,
        );
        let offsets: Vec<usize> = all.fields.iter().map(|f| f.offset).collect();
        assert_eq!(offsets, [0, 4, 8, 16]);
        assert_eq!(all.record_bytes, 24);

        // Only channel0 and channel3: channel3 moves to byte 8.
        let some = Layout::new(
            elements(&[("c0", 0, "be:u16/16>>0"), ("c3", 3, "be:u64/64>>0")]),
            None,
        );
        assert_eq!(some.fields[1].offset, 8);
        assert_eq!(some.record_bytes, 16);
    }

    #[test]
    fn timestamp_goes_last_on_its_own_alignment() {
        // A 12-bit sample (2 bytes) then the 8-byte timestamp: the stamp
        // starts at byte 8, and the record is 16 bytes.
        let layout = Layout::new(
            vec![("in_accel_x".to_string(), 0, ty("le:s12/16>>4"))],
            Some(ty("le:s64/64>>0")),
        );
        let stamp = layout.timestamp.expect("timestamp");
        assert_eq!(stamp.offset, 8);
        assert_eq!(layout.record_bytes, 16);
        assert_eq!(layout.fields[0].offset, 0);
    }

    #[test]
    fn canonical_values_round_and_keep_no_value() {
        assert_eq!(canonical(1.2345, 3), 1_235);
        assert_eq!(canonical(-0.0006, 3), -1);
        assert_eq!(canonical(f64::NAN, 0), NO_VALUE);
        assert_eq!(canonical(f64::INFINITY, 0), NO_VALUE);
        assert_eq!(canonical(1e30, 0), i32::MAX);
    }
}
