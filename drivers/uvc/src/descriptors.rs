//! The USB configuration and UVC class descriptors, read from the device and trusted for nothing.
//!
//! A descriptor whose own length cannot be right ends the walk: nothing after it can be found. One
//! that is framed properly but says something impossible is skipped and counted, and the rest of
//! the device is still usable. The camera is usable if the walk found a control interface, a
//! streaming interface, at least one mode and at least one isochronous alternate setting.
//!
//! Modes are the flat list `camera.frames/1` serves: one entry per format, frame size and rate.

/// The most of a configuration read. A device with more is read this far and counted.
pub const MAX_CONFIG_BYTES: usize = 16 * 1024;
/// The most modes kept, which also keeps every index a `u16`.
pub const MAX_MODES: usize = 256;
/// The most alternate settings kept.
pub const MAX_ALTS: usize = 32;
/// The largest frame believed. A device can claim 4 GiB in a descriptor.
pub const MAX_FRAME_BYTES: u32 = 64 * 1024 * 1024;

const DT_CONFIG: u8 = 0x02;
const DT_INTERFACE: u8 = 0x04;
const DT_ENDPOINT: u8 = 0x05;
const DT_SS_COMPANION: u8 = 0x30;
const CS_INTERFACE: u8 = 0x24;

const CC_VIDEO: u8 = 0x0e;
const SC_VIDEOCONTROL: u8 = 0x01;
const SC_VIDEOSTREAMING: u8 = 0x02;

const VC_HEADER: u8 = 0x01;
const VS_INPUT_HEADER: u8 = 0x01;
const VS_FORMAT_UNCOMPRESSED: u8 = 0x04;
const VS_FRAME_UNCOMPRESSED: u8 = 0x05;
const VS_FORMAT_MJPEG: u8 = 0x06;
const VS_FRAME_MJPEG: u8 = 0x07;
const VS_FORMAT_FRAME_BASED: u8 = 0x10;
const VS_FORMAT_STREAM_BASED: u8 = 0x12;

const CONFIG_MIN: usize = 9;
const INTERFACE_MIN: usize = 9;
const ENDPOINT_MIN: usize = 7;
const COMPANION_MIN: usize = 6;
const VC_HEADER_MIN: usize = 12;
const VS_HEADER_MIN: usize = 13;
const FORMAT_UNCOMPRESSED_MIN: usize = 27;
const FORMAT_MJPEG_MIN: usize = 11;
const FRAME_MIN: usize = 26;
const CONTINUOUS_MIN: usize = FRAME_MIN + 12;

/// The tail every standard UVC format GUID shares after its four-character code.
const GUID_SUFFIX: [u8; 12] = [
    0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71,
];
const FOURCC_MJPEG: u32 = u32::from_le_bytes(*b"MJPG");

/// 100 ns units in one second, times a thousand.
const FPS_MILLI_SCALE: u64 = 10_000_000_000;

const ISO: u8 = 0x01;
const BULK: u8 = 0x02;
const TRANSFER_MASK: u8 = 0x03;
const DIR_IN: u8 = 0x80;

pub const BAD_CONFIG: &str = "the configuration descriptor is malformed: retrying";
pub const NOT_UVC: &str = "the device has no video control and streaming interfaces: retrying";
pub const NO_MODES: &str = "the device describes no usable video format: retrying";
pub const NO_ISO: &str = "the device has no isochronous video endpoint: retrying";

/// One format, size and rate of the camera.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mode {
    pub fourcc: u32,
    pub width: u32,
    pub height: u32,
    pub fps_milli: u32,
    pub max_frame_bytes: u32,
    /// `bFormatIndex`, as probe and commit name it.
    pub format_index: u8,
    /// `bFrameIndex`, as probe and commit name it.
    pub frame_index: u8,
    /// 100 ns units.
    pub interval: u32,
    /// Bytes of a complete frame, for an uncompressed format.
    pub expected_bytes: Option<u32>,
}

/// An isochronous alternate setting of the streaming interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Alt {
    pub alternate: u8,
    pub endpoint: u8,
    /// Bytes per (micro)frame, which is what one isochronous packet can carry.
    pub bytes: u32,
}

/// What the descriptors say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Camera {
    /// `bcdUVC`.
    pub version: u16,
    /// The video control interface. A host's own video driver binds this one and the streaming
    /// interface, so both are taken from it.
    pub control_interface: u8,
    pub stream_interface: u8,
    pub modes: Vec<Mode>,
    /// Smallest first.
    pub alts: Vec<Alt>,
    /// Descriptors that made no sense, each counted once.
    pub malformed: u32,
    /// Well-formed things this driver doesn't serve, or that didn't fit.
    pub unsupported: u32,
}

impl Camera {
    /// The smallest alternate setting that carries `payload` bytes per (micro)frame. A device can
    /// ask for more than any setting has, so the largest it has is the answer then.
    pub fn alt_for(&self, payload: u32) -> Option<Alt> {
        self.alts
            .iter()
            .find(|alt| alt.bytes >= payload)
            .or(self.alts.last())
            .copied()
    }

    /// The most bytes one packet can carry on any setting.
    pub fn largest_packet(&self) -> u32 {
        self.alts.iter().map(|alt| alt.bytes).max().unwrap_or(0)
    }
}

fn u16_at(d: &[u8], at: usize) -> Option<u16> {
    let bytes = d.get(at..at.checked_add(2)?)?;
    Some(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn u32_at(d: &[u8], at: usize) -> Option<u32> {
    let bytes = d.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    Other,
    Control,
    Streaming { alternate: u8 },
}

#[derive(Debug, Clone, Copy)]
struct Format {
    index: u8,
    frames: u8,
    fourcc: u32,
    /// Set for an uncompressed format.
    bits_per_pixel: Option<u8>,
    seen_frames: IndexSet,
}

#[derive(Debug, Clone, Copy)]
enum Current {
    /// No format descriptor has been seen, so a frame descriptor here is an orphan.
    Nothing,
    /// A format this driver doesn't serve: its frames are not orphans.
    Unsupported,
    Format(Format),
}

struct Parser {
    section: Section,
    version: Option<u16>,
    control_interface: Option<u8>,
    stream_interface: Option<u8>,
    header_seen: bool,
    endpoint: Option<u8>,
    declared_formats: u8,
    seen_formats: IndexSet,
    current: Current,
    modes: Vec<Mode>,
    alts: Vec<Alt>,
    last_alt: Option<usize>,
    malformed: u32,
    unsupported: u32,
}

/// Parses a configuration, of which `bytes` is what the device returned.
pub fn parse(bytes: &[u8]) -> Result<Camera, &'static str> {
    let (Some(&length), Some(&kind), Some(total)) = (bytes.first(), bytes.get(1), u16_at(bytes, 2))
    else {
        return Err(BAD_CONFIG);
    };
    let (length, total) = (usize::from(length), usize::from(total));
    if bytes.len() < CONFIG_MIN || length < CONFIG_MIN || kind != DT_CONFIG || total < length {
        return Err(BAD_CONFIG);
    }
    let mut parser = Parser::new();
    if total > bytes.len() {
        // More is promised than arrived, or than this reads. Use what is here.
        parser.malformed += 1;
    }
    parser.walk(&bytes[..total.min(bytes.len())]);
    parser.finish()
}

impl Parser {
    fn new() -> Self {
        Self {
            section: Section::Other,
            version: None,
            control_interface: None,
            stream_interface: None,
            header_seen: false,
            endpoint: None,
            declared_formats: 0,
            seen_formats: IndexSet::default(),
            current: Current::Nothing,
            modes: Vec::new(),
            alts: Vec::new(),
            last_alt: None,
            malformed: 0,
            unsupported: 0,
        }
    }

    fn walk(&mut self, bytes: &[u8]) {
        let mut rest = bytes;
        while let Some(&length) = rest.first() {
            let length = usize::from(length);
            let Some(descriptor) = rest.get(..length).filter(|_| length >= 2) else {
                self.malformed += 1;
                return;
            };
            rest = &rest[length..];
            self.descriptor(descriptor, rest.len());
        }
    }

    /// `after` is how many bytes of the configuration follow this descriptor.
    fn descriptor(&mut self, d: &[u8], after: usize) {
        match d[1] {
            DT_INTERFACE => self.interface(d),
            DT_ENDPOINT => self.endpoint(d),
            DT_SS_COMPANION => self.companion(d),
            CS_INTERFACE => self.class_specific(d, after),
            _ => {}
        }
    }

    fn interface(&mut self, d: &[u8]) {
        self.last_alt = None;
        if d.len() < INTERFACE_MIN {
            self.malformed += 1;
            self.section = Section::Other;
            return;
        }
        let (number, alternate, class, subclass) = (d[2], d[3], d[5], d[6]);
        self.section = match (class, subclass) {
            (CC_VIDEO, SC_VIDEOCONTROL) if self.control_interface.is_none() => {
                self.control_interface = Some(number);
                Section::Control
            }
            (CC_VIDEO, SC_VIDEOSTREAMING) => match self.stream_interface {
                None => {
                    self.stream_interface = Some(number);
                    Section::Streaming { alternate }
                }
                Some(interface) if interface == number => Section::Streaming { alternate },
                Some(_) => {
                    if alternate == 0 {
                        self.unsupported += 1;
                    }
                    Section::Other
                }
            },
            _ => Section::Other,
        };
    }

    fn class_specific(&mut self, d: &[u8], after: usize) {
        let Some(&subtype) = d.get(2) else {
            self.malformed += 1;
            return;
        };
        match (self.section, subtype) {
            (Section::Control, VC_HEADER) => self.control_header(d, after),
            (Section::Streaming { alternate: 0 }, _) => self.streaming(d, subtype, after),
            _ => {}
        }
    }

    /// Counts a class header whose `wTotalLength` can't be true. The walk goes by each
    /// descriptor's own length, so this never decides what gets read.
    fn check_total(&mut self, declared: Option<u16>, d: &[u8], after: usize) {
        let fits = declared.is_some_and(|total| {
            let total = usize::from(total);
            total >= d.len() && total - d.len() <= after
        });
        if !fits {
            self.malformed += 1;
        }
    }

    fn control_header(&mut self, d: &[u8], after: usize) {
        if d.len() < VC_HEADER_MIN || self.version.is_some() {
            self.malformed += 1;
            return;
        }
        self.check_total(u16_at(d, 5), d, after);
        self.version = u16_at(d, 3);
    }

    fn streaming(&mut self, d: &[u8], subtype: u8, after: usize) {
        match subtype {
            VS_INPUT_HEADER => self.input_header(d, after),
            VS_FORMAT_UNCOMPRESSED => self.format_uncompressed(d),
            VS_FORMAT_MJPEG => self.format_mjpeg(d),
            VS_FRAME_UNCOMPRESSED | VS_FRAME_MJPEG => self.frame(d),
            VS_FORMAT_FRAME_BASED | VS_FORMAT_STREAM_BASED => {
                self.unsupported += 1;
                self.current = Current::Unsupported;
            }
            _ => {}
        }
    }

    fn input_header(&mut self, d: &[u8], after: usize) {
        if d.len() < VS_HEADER_MIN || self.header_seen {
            self.malformed += 1;
            return;
        }
        self.header_seen = true;
        self.check_total(u16_at(d, 4), d, after);
        self.declared_formats = d[3];
        if d[6] & DIR_IN == 0 {
            self.malformed += 1;
        } else {
            self.endpoint = Some(d[6]);
        }
    }

    fn begin_format(&mut self, d: &[u8], fourcc: u32, bits_per_pixel: Option<u8>) {
        let (index, frames) = (d[3], d[4]);
        if index == 0 || index > self.declared_formats || self.seen_formats.test_and_set(index) {
            self.malformed += 1;
            self.current = Current::Nothing;
            return;
        }
        self.current = Current::Format(Format {
            index,
            frames,
            fourcc,
            bits_per_pixel,
            seen_frames: IndexSet::default(),
        });
    }

    fn format_uncompressed(&mut self, d: &[u8]) {
        if d.len() < FORMAT_UNCOMPRESSED_MIN {
            self.malformed += 1;
            self.current = Current::Nothing;
            return;
        }
        if d[9..21] != GUID_SUFFIX {
            self.unsupported += 1;
            self.current = Current::Unsupported;
            return;
        }
        let fourcc = u32::from_le_bytes([d[5], d[6], d[7], d[8]]);
        self.begin_format(d, fourcc, Some(d[21]));
    }

    fn format_mjpeg(&mut self, d: &[u8]) {
        if d.len() < FORMAT_MJPEG_MIN {
            self.malformed += 1;
            self.current = Current::Nothing;
            return;
        }
        self.begin_format(d, FOURCC_MJPEG, None);
    }

    fn frame(&mut self, d: &[u8]) {
        let format = match &mut self.current {
            Current::Format(format) => format,
            Current::Unsupported => return,
            Current::Nothing => {
                self.malformed += 1;
                return;
            }
        };
        if d.len() < FRAME_MIN {
            self.malformed += 1;
            return;
        }
        let index = d[3];
        if index == 0 || index > format.frames || format.seen_frames.test_and_set(index) {
            self.malformed += 1;
            return;
        }
        let format = *format;
        let (Some(width), Some(height), Some(buffer), Some(default)) =
            (u16_at(d, 5), u16_at(d, 7), u32_at(d, 17), u32_at(d, 21))
        else {
            self.malformed += 1;
            return;
        };
        let Some(mut mode) = frame_mode(&format, index, width, height, buffer) else {
            self.malformed += 1;
            return;
        };
        match d[25] {
            0 => self.continuous(d, default, &mut mode),
            count => self.discrete(d, usize::from(count), &mut mode),
        }
    }

    fn continuous(&mut self, d: &[u8], default: u32, mode: &mut Mode) {
        let (Some(min), Some(max)) = (u32_at(d, FRAME_MIN), u32_at(d, FRAME_MIN + 4)) else {
            self.malformed += 1;
            return;
        };
        if d.len() < CONTINUOUS_MIN || min == 0 || max < min || default < min || default > max {
            self.malformed += 1;
            return;
        }
        self.add_mode(mode, min);
        if default != min {
            self.add_mode(mode, default);
        }
        if max != min && max != default {
            self.add_mode(mode, max);
        }
    }

    fn discrete(&mut self, d: &[u8], count: usize, mode: &mut Mode) {
        // At most 255 entries of 4 bytes past a descriptor no longer than 255 bytes.
        if d.len() < FRAME_MIN + count * 4 {
            self.malformed += 1;
            return;
        }
        for entry in 0..count {
            match u32_at(d, FRAME_MIN + entry * 4) {
                Some(interval) if interval != 0 => self.add_mode(mode, interval),
                _ => self.malformed += 1,
            }
        }
    }

    fn add_mode(&mut self, mode: &mut Mode, interval: u32) {
        if self.modes.len() >= MAX_MODES {
            self.unsupported += 1;
            return;
        }
        mode.interval = interval;
        mode.fps_milli = u32::try_from(FPS_MILLI_SCALE / u64::from(interval)).unwrap_or(u32::MAX);
        self.modes.push(*mode);
    }

    fn endpoint(&mut self, d: &[u8]) {
        let Section::Streaming { alternate } = self.section else {
            return;
        };
        if d.len() < ENDPOINT_MIN {
            self.malformed += 1;
            return;
        }
        let (address, attributes) = (d[2], d[3]);
        if alternate == 0 || attributes & TRANSFER_MASK == BULK {
            return;
        }
        if attributes & TRANSFER_MASK != ISO || address & DIR_IN == 0 {
            return;
        }
        if self.endpoint != Some(address) {
            self.malformed += 1;
            return;
        }
        let Some(max_packet) = u16_at(d, 4) else {
            self.malformed += 1;
            return;
        };
        let size = u32::from(max_packet & 0x07ff);
        let extra = u32::from((max_packet >> 11) & 0x03);
        if extra == 3 || size == 0 {
            self.malformed += 1;
            return;
        }
        if self.alts.len() >= MAX_ALTS {
            self.unsupported += 1;
            return;
        }
        if self.alts.iter().any(|alt| alt.alternate == alternate) {
            self.malformed += 1;
            return;
        }
        self.last_alt = Some(self.alts.len());
        self.alts.push(Alt {
            alternate,
            endpoint: address,
            bytes: size * (1 + extra),
        });
    }

    /// A SuperSpeed endpoint's real rate is here, and the endpoint's own bits mean something else.
    fn companion(&mut self, d: &[u8]) {
        let Some(index) = self.last_alt.take() else {
            return;
        };
        match u16_at(d, 4).filter(|_| d.len() >= COMPANION_MIN) {
            Some(bytes) if bytes != 0 => self.alts[index].bytes = u32::from(bytes),
            _ => self.malformed += 1,
        }
    }

    fn finish(mut self) -> Result<Camera, &'static str> {
        let (Some(version), Some(control_interface), Some(stream_interface)) =
            (self.version, self.control_interface, self.stream_interface)
        else {
            return Err(NOT_UVC);
        };
        if self.modes.is_empty() {
            return Err(NO_MODES);
        }
        if self.alts.is_empty() {
            return Err(NO_ISO);
        }
        self.alts.sort_by_key(|alt| alt.bytes);
        Ok(Camera {
            version,
            control_interface,
            stream_interface,
            modes: self.modes,
            alts: self.alts,
            malformed: self.malformed,
            unsupported: self.unsupported,
        })
    }
}

/// The mode a frame descriptor describes, before any rate, or `None` if it can't be true.
fn frame_mode(format: &Format, index: u8, width: u16, height: u16, buffer: u32) -> Option<Mode> {
    if width == 0 || height == 0 {
        return None;
    }
    let (expected_bytes, max_frame_bytes) = match format.bits_per_pixel {
        Some(bits) => {
            let bits = u64::from(bits).checked_mul(u64::from(width))?;
            let bytes = bits.checked_mul(u64::from(height))? / 8;
            let bytes = u32::try_from(bytes).ok().filter(|bytes| *bytes != 0)?;
            (Some(bytes), bytes)
        }
        None => (None, buffer),
    };
    if max_frame_bytes == 0 || max_frame_bytes > MAX_FRAME_BYTES {
        return None;
    }
    Some(Mode {
        fourcc: format.fourcc,
        width: u32::from(width),
        height: u32::from(height),
        fps_milli: 0,
        max_frame_bytes,
        format_index: format.index,
        frame_index: index,
        interval: 0,
        expected_bytes,
    })
}

/// A set of `u8` values, so a duplicate index is found without allocating.
#[derive(Debug, Clone, Copy, Default)]
struct IndexSet([u64; 4]);

impl IndexSet {
    /// Whether `value` was already in the set. It is in the set afterwards.
    fn test_and_set(&mut self, value: u8) -> bool {
        let (word, bit) = (usize::from(value >> 6), 1u64 << (value & 0x3f));
        let seen = self.0[word] & bit != 0;
        self.0[word] |= bit;
        seen
    }
}
