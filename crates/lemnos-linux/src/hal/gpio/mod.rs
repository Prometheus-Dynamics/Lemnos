use super::{IoError, c_string, invalid_input};
use embedded_hal::digital::{ErrorType, InputPin, OutputPin, StatefulOutputPin};
use lemnos_linux_sys::gpio::{self as sys, attr, flag};
use lemnos_linux_sys::poll;
use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::os::fd::{AsFd, BorrowedFd};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Line direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum LineDirection {
    /// Input.
    #[default]
    Input,
    /// Output.
    Output,
    /// Leave the direction (and the driven value) as it is; only
    /// `active_low` may be set. For taking over a line without glitching it.
    AsIs,
}

/// Internal bias of an input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum LineBias {
    /// Leave as configured (by the device tree or a previous user).
    #[default]
    AsIs,
    /// Pull-up.
    PullUp,
    /// Pull-down.
    PullDown,
    /// No bias.
    Disabled,
}

/// Output drive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum LineDrive {
    /// Push-pull.
    #[default]
    PushPull,
    /// Open drain.
    OpenDrain,
    /// Open source.
    OpenSource,
}

/// Which edges of an input produce events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum LineEdge {
    /// None.
    #[default]
    None,
    /// Rising edges.
    Rising,
    /// Falling edges.
    Falling,
    /// Both.
    Both,
}

/// How to request (or reconfigure) one line. Values are logical: with
/// `active_low`, logical high drives the pin low.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct LineSettings {
    /// Direction.
    pub direction: LineDirection,
    /// Invert the logical value.
    pub active_low: bool,
    /// Bias (inputs; outputs on hardware that supports it).
    pub bias: LineBias,
    /// Drive (outputs).
    pub drive: LineDrive,
    /// Edge events (inputs).
    pub edge: LineEdge,
    /// Debounce period in microseconds (inputs).
    pub debounce_us: Option<u32>,
    /// Logical value an output starts at.
    pub output_value: bool,
}

impl LineSettings {
    /// An input.
    pub fn input() -> Self {
        Self::default()
    }

    /// A push-pull output starting at logical `value`.
    pub fn output(value: bool) -> Self {
        Self {
            direction: LineDirection::Output,
            output_value: value,
            ..Self::default()
        }
    }

    /// Marks the line active-low.
    pub fn active_low(mut self) -> Self {
        self.active_low = true;
        self
    }

    /// Sets the bias.
    pub fn with_bias(mut self, bias: LineBias) -> Self {
        self.bias = bias;
        self
    }

    /// Sets the drive.
    pub fn with_drive(mut self, drive: LineDrive) -> Self {
        self.drive = drive;
        self
    }

    /// Sets the edges that produce events.
    pub fn with_edge(mut self, edge: LineEdge) -> Self {
        self.edge = edge;
        self
    }

    /// Sets the debounce period.
    pub fn with_debounce_us(mut self, debounce_us: u32) -> Self {
        self.debounce_us = Some(debounce_us);
        self
    }

    /// The uAPI v2 flags.
    pub fn flags(&self) -> io::Result<u64> {
        let mut flags = match self.direction {
            LineDirection::Input => flag::INPUT,
            LineDirection::Output => flag::OUTPUT,
            LineDirection::AsIs => 0,
        };
        if self.active_low {
            flags |= flag::ACTIVE_LOW;
        }
        flags |= match self.bias {
            LineBias::AsIs => 0,
            LineBias::PullUp => flag::BIAS_PULL_UP,
            LineBias::PullDown => flag::BIAS_PULL_DOWN,
            LineBias::Disabled => flag::BIAS_DISABLED,
        };
        match self.direction {
            LineDirection::AsIs => {
                if self.bias != LineBias::AsIs
                    || self.drive != LineDrive::PushPull
                    || self.edge != LineEdge::None
                    || self.debounce_us.is_some()
                {
                    return Err(invalid_input(
                        "bias, drive, edges and debounce need a direction",
                    ));
                }
            }
            LineDirection::Output => {
                if self.edge != LineEdge::None || self.debounce_us.is_some() {
                    return Err(invalid_input("edge events and debounce need an input line"));
                }
                flags |= match self.drive {
                    LineDrive::PushPull => 0,
                    LineDrive::OpenDrain => flag::OPEN_DRAIN,
                    LineDrive::OpenSource => flag::OPEN_SOURCE,
                };
            }
            LineDirection::Input => {
                if self.drive != LineDrive::PushPull {
                    return Err(invalid_input("open drain/source needs an output line"));
                }
                flags |= match self.edge {
                    LineEdge::None => 0,
                    LineEdge::Rising => flag::EDGE_RISING,
                    LineEdge::Falling => flag::EDGE_FALLING,
                    LineEdge::Both => flag::EDGE_RISING | flag::EDGE_FALLING,
                };
            }
        }
        Ok(flags)
    }
}

/// Builds the configuration for `settings` (by request index): the first
/// line's flags as the default, an attribute per other distinct flag set, one
/// per distinct debounce period, and the outputs' initial values.
pub(crate) fn build_config(settings: &[LineSettings]) -> io::Result<sys::LineConfig> {
    if settings.is_empty() || settings.len() > sys::MAX_LINES {
        return Err(invalid_input(format!(
            "{} lines (1..={})",
            settings.len(),
            sys::MAX_LINES
        )));
    }
    let mut config = sys::LineConfig {
        flags: settings[0].flags()?,
        ..sys::LineConfig::default()
    };
    let mut attrs: Vec<sys::LineConfigAttribute> = Vec::new();
    let mut add = |id: u32, value: u64, index: usize| match attrs
        .iter_mut()
        .find(|a| a.attr.id == id && a.attr.value == value)
    {
        Some(a) => a.mask |= 1 << index,
        None => attrs.push(sys::LineConfigAttribute {
            attr: sys::LineAttribute {
                id,
                padding: 0,
                value,
            },
            mask: 1 << index,
        }),
    };
    let mut values = 0u64;
    let mut outputs = 0u64;
    for (i, line) in settings.iter().enumerate() {
        let flags = line.flags()?;
        if flags != config.flags {
            add(attr::FLAGS, flags, i);
        }
        if let Some(us) = line.debounce_us {
            add(attr::DEBOUNCE, u64::from(us), i);
        }
        if line.direction == LineDirection::Output {
            outputs |= 1 << i;
            values |= u64::from(line.output_value) << i;
        }
    }
    if outputs != 0 {
        attrs.push(sys::LineConfigAttribute {
            attr: sys::LineAttribute {
                id: attr::OUTPUT_VALUES,
                padding: 0,
                value: values,
            },
            mask: outputs,
        });
    }
    if attrs.len() > sys::NUM_ATTRS_MAX {
        return Err(invalid_input("too many distinct line configurations"));
    }
    config.num_attrs = attrs.len() as u32;
    config.attrs[..attrs.len()].copy_from_slice(&attrs);
    Ok(config)
}

/// Builds a line request for `lines` (chip offset, settings).
pub(crate) fn build_request(
    consumer: &str,
    lines: &[(u32, LineSettings)],
) -> io::Result<sys::LineRequest> {
    let settings: Vec<LineSettings> = lines.iter().map(|(_, s)| *s).collect();
    let mut req = sys::LineRequest {
        config: build_config(&settings)?,
        num_lines: lines.len() as u32,
        ..sys::LineRequest::default()
    };
    for (i, (offset, _)) in lines.iter().enumerate() {
        req.offsets[i] = *offset;
    }
    let name = consumer.as_bytes();
    let n = name.len().min(sys::MAX_NAME_SIZE - 1);
    req.consumer[..n].copy_from_slice(&name[..n]);
    Ok(req)
}

/// What the kernel reports about a chip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpioChipInfo {
    /// Kernel name (`gpiochipN`).
    pub name: String,
    /// Label (e.g. `pinctrl-rp1`).
    pub label: String,
    /// Number of lines.
    pub lines: u32,
}

/// What the kernel reports about one line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpioLineInfo {
    /// Offset on the chip.
    pub offset: u32,
    /// Line name (device tree `gpio-line-names`), empty if unnamed.
    pub name: String,
    /// Who holds it, empty if free.
    pub consumer: String,
    /// Whether anyone (kernel or userspace) holds it.
    pub used: bool,
    /// The current settings, as far as the kernel reports them
    /// (`output_value` is not reported and stays `false`).
    pub settings: LineSettings,
}

impl GpioLineInfo {
    fn from_raw(raw: &sys::LineInfo) -> Self {
        let f = raw.flags;
        let has = |bit: u64| f & bit != 0;
        let debounce_us = raw.attrs[..(raw.num_attrs as usize).min(sys::NUM_ATTRS_MAX)]
            .iter()
            .find(|a| a.id == attr::DEBOUNCE)
            .map(|a| a.value as u32);
        let settings = LineSettings {
            direction: if has(flag::OUTPUT) {
                LineDirection::Output
            } else {
                LineDirection::Input
            },
            active_low: has(flag::ACTIVE_LOW),
            bias: if has(flag::BIAS_PULL_UP) {
                LineBias::PullUp
            } else if has(flag::BIAS_PULL_DOWN) {
                LineBias::PullDown
            } else if has(flag::BIAS_DISABLED) {
                LineBias::Disabled
            } else {
                LineBias::AsIs
            },
            drive: if has(flag::OPEN_DRAIN) {
                LineDrive::OpenDrain
            } else if has(flag::OPEN_SOURCE) {
                LineDrive::OpenSource
            } else {
                LineDrive::PushPull
            },
            edge: match (has(flag::EDGE_RISING), has(flag::EDGE_FALLING)) {
                (true, true) => LineEdge::Both,
                (true, false) => LineEdge::Rising,
                (false, true) => LineEdge::Falling,
                (false, false) => LineEdge::None,
            },
            debounce_us,
            output_value: false,
        };
        Self {
            offset: raw.offset,
            name: c_string(&raw.name),
            consumer: c_string(&raw.consumer),
            used: has(flag::USED),
            settings,
        }
    }
}

/// An open GPIO chip (`/dev/gpiochipN`), GPIO uAPI v2. Ported from Styx
/// (`styx-kernel`'s `GpioChip`) and extended with inputs, bias, debounce and
/// edge events.
#[derive(Debug)]
pub struct GpioChip {
    file: File,
    path: PathBuf,
    info: GpioChipInfo,
}

impl GpioChip {
    /// Opens a chip node, e.g. `/dev/gpiochip0`.
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        // Read-write as libgpiod does; read-only still answers info queries
        // where write access is not granted.
        let file = match OpenOptions::new().read(true).write(true).open(&path) {
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => File::open(&path)?,
            other => other?,
        };
        let raw = sys::chip_info(file.as_fd())?;
        Ok(Self {
            file,
            path,
            info: GpioChipInfo {
                name: c_string(&raw.name),
                label: c_string(&raw.label),
                lines: raw.lines,
            },
        })
    }

    /// Opens the first chip whose label (e.g. `pinctrl-rp1`) matches.
    pub fn open_by_label(label: &str) -> io::Result<Self> {
        for path in Self::paths()? {
            if let Ok(chip) = Self::open(&path)
                && chip.info.label == label
            {
                return Ok(chip);
            }
        }
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("no GPIO chip labelled {label}"),
        ))
    }

    /// `/dev/gpiochip*` nodes, sorted.
    pub fn paths() -> io::Result<Vec<PathBuf>> {
        Self::paths_in(Path::new("/dev"))
    }

    /// `gpiochip*` nodes in `dev_root`, sorted.
    pub fn paths_in(dev_root: &Path) -> io::Result<Vec<PathBuf>> {
        let mut paths: Vec<PathBuf> = std::fs::read_dir(dev_root)?
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("gpiochip"))
            .map(|e| e.path())
            .collect();
        paths.sort();
        Ok(paths)
    }

    /// The node path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Name, label and line count.
    pub fn info(&self) -> &GpioChipInfo {
        &self.info
    }

    /// Number of lines.
    pub fn num_lines(&self) -> u32 {
        self.info.lines
    }

    /// Information about one line.
    pub fn line_info(&self, offset: u32) -> io::Result<GpioLineInfo> {
        sys::line_info(self.file.as_fd(), offset).map(|raw| GpioLineInfo::from_raw(&raw))
    }

    /// The offset of the line called `name`.
    pub fn find_line(&self, name: &str) -> io::Result<Option<u32>> {
        for offset in 0..self.info.lines {
            if self.line_info(offset)?.name == name {
                return Ok(Some(offset));
            }
        }
        Ok(None)
    }

    /// Requests `lines` (offset, settings) for `consumer`. Fails with `EBUSY`
    /// if any is held.
    pub fn request(&self, consumer: &str, lines: &[(u32, LineSettings)]) -> io::Result<GpioLines> {
        for (offset, _) in lines {
            if *offset >= self.info.lines {
                return Err(invalid_input(format!(
                    "line {offset} beyond {} on {}",
                    self.info.lines, self.info.name
                )));
            }
        }
        let req = build_request(consumer, lines)?;
        let fd = sys::request_lines(self.file.as_fd(), &req)?;
        Ok(GpioLines {
            file: File::from(fd),
            offsets: lines.iter().map(|(o, _)| *o).collect(),
            settings: lines.iter().map(|(_, s)| *s).collect(),
        })
    }

    /// Requests one line.
    pub fn request_line(
        &self,
        consumer: &str,
        offset: u32,
        settings: LineSettings,
    ) -> io::Result<GpioLine> {
        self.request(consumer, &[(offset, settings)])
            .map(|lines| GpioLine { lines })
    }
}

impl AsFd for GpioChip {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.file.as_fd()
    }
}

/// Which edge an event saw.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EdgeKind {
    /// Logical low to high.
    Rising,
    /// Logical high to low.
    Falling,
}

/// An edge event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EdgeEvent {
    /// Chip offset of the line.
    pub offset: u32,
    /// The edge.
    pub kind: EdgeKind,
    /// `CLOCK_MONOTONIC` nanoseconds.
    pub timestamp_ns: u64,
    /// Sequence number across the request.
    pub seqno: u32,
    /// Sequence number on this line.
    pub line_seqno: u32,
}

/// Requested lines; released when dropped.
#[derive(Debug)]
pub struct GpioLines {
    file: File,
    offsets: Vec<u32>,
    settings: Vec<LineSettings>,
}

fn all_mask(n: usize) -> u64 {
    if n >= 64 { u64::MAX } else { (1u64 << n) - 1 }
}

impl GpioLines {
    /// Chip offsets, in request order.
    pub fn offsets(&self) -> &[u32] {
        &self.offsets
    }

    /// Current settings, in request order.
    pub fn settings(&self) -> &[LineSettings] {
        &self.settings
    }

    fn index_of(&self, offset: u32) -> io::Result<usize> {
        self.offsets
            .iter()
            .position(|&o| o == offset)
            .ok_or_else(|| invalid_input(format!("line {offset} not in this request")))
    }

    /// Logical values, bit `i` for request index `i`.
    pub fn values(&self) -> io::Result<u64> {
        sys::get_values(self.file.as_fd(), all_mask(self.offsets.len()))
    }

    /// The logical value of the line at chip `offset`.
    pub fn value(&self, offset: u32) -> io::Result<bool> {
        let index = self.index_of(offset)?;
        Ok(sys::get_values(self.file.as_fd(), 1 << index)? != 0)
    }

    /// Sets logical values by request index, all at once.
    pub fn set_values(&mut self, values: &[(usize, bool)]) -> io::Result<()> {
        let mut v = sys::LineValues::default();
        for &(index, value) in values {
            if index >= self.offsets.len() {
                return Err(invalid_input(format!(
                    "line index {index} of {}",
                    self.offsets.len()
                )));
            }
            v.mask |= 1 << index;
            if value {
                v.bits |= 1 << index;
            }
        }
        sys::set_values(self.file.as_fd(), v)?;
        for &(index, value) in values {
            self.settings[index].output_value = value;
        }
        Ok(())
    }

    /// Sets the line at chip `offset` to a logical value.
    pub fn set(&mut self, offset: u32, value: bool) -> io::Result<()> {
        let index = self.index_of(offset)?;
        self.set_values(&[(index, value)])
    }

    /// Changes the settings of every line (request order) without releasing
    /// them.
    pub fn reconfigure(&mut self, settings: &[LineSettings]) -> io::Result<()> {
        if settings.len() != self.offsets.len() {
            return Err(invalid_input(format!(
                "{} settings for {} lines",
                settings.len(),
                self.offsets.len()
            )));
        }
        sys::set_config(self.file.as_fd(), &build_config(settings)?)?;
        self.settings.copy_from_slice(settings);
        Ok(())
    }

    /// Waits up to `timeout` (`None`: forever) for an edge event.
    pub fn wait_event(&mut self, timeout: Option<Duration>) -> io::Result<Option<EdgeEvent>> {
        let ready = poll::poll_one(self.file.as_fd(), poll::POLLIN, timeout)?;
        if ready & poll::POLLIN == 0 {
            return Ok(None);
        }
        let mut raw = [0u8; sys::LINE_EVENT_SIZE];
        self.file.read_exact(&mut raw)?;
        let ev =
            sys::LineEvent::from_bytes(&raw).ok_or_else(|| io::Error::other("short GPIO event"))?;
        Ok(Some(EdgeEvent {
            offset: ev.offset,
            kind: if ev.id == sys::EVENT_RISING_EDGE {
                EdgeKind::Rising
            } else {
                EdgeKind::Falling
            },
            timestamp_ns: ev.timestamp_ns,
            seqno: ev.seqno,
            line_seqno: ev.line_seqno,
        }))
    }

    /// The next pending edge event, without waiting.
    pub fn read_event(&mut self) -> io::Result<Option<EdgeEvent>> {
        self.wait_event(Some(Duration::ZERO))
    }

    /// Releases the lines (same as dropping).
    pub fn release(self) {}
}

impl AsFd for GpioLines {
    /// Readable when an edge event is pending.
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.file.as_fd()
    }
}

/// One requested line: an embedded-hal input and output pin. Released when
/// dropped.
#[derive(Debug)]
pub struct GpioLine {
    lines: GpioLines,
}

impl GpioLine {
    /// The chip offset.
    pub fn offset(&self) -> u32 {
        self.lines.offsets[0]
    }

    /// The current settings.
    pub fn settings(&self) -> LineSettings {
        self.lines.settings[0]
    }

    /// The logical value.
    pub fn get(&self) -> io::Result<bool> {
        Ok(self.lines.values()? & 1 != 0)
    }

    /// Sets the logical value (outputs).
    pub fn set(&mut self, value: bool) -> io::Result<()> {
        self.lines.set_values(&[(0, value)])
    }

    /// Changes the settings without releasing the line.
    pub fn reconfigure(&mut self, settings: LineSettings) -> io::Result<()> {
        self.lines.reconfigure(&[settings])
    }

    /// Waits up to `timeout` for an edge event.
    pub fn wait_event(&mut self, timeout: Option<Duration>) -> io::Result<Option<EdgeEvent>> {
        self.lines.wait_event(timeout)
    }

    /// The underlying request.
    pub fn into_lines(self) -> GpioLines {
        self.lines
    }
}

impl AsFd for GpioLine {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.lines.as_fd()
    }
}

impl ErrorType for GpioLine {
    type Error = IoError;
}

impl OutputPin for GpioLine {
    fn set_low(&mut self) -> Result<(), IoError> {
        self.set(false).map_err(IoError::from)
    }

    fn set_high(&mut self) -> Result<(), IoError> {
        self.set(true).map_err(IoError::from)
    }
}

impl StatefulOutputPin for GpioLine {
    fn is_set_high(&mut self) -> Result<bool, IoError> {
        self.get().map_err(IoError::from)
    }

    fn is_set_low(&mut self) -> Result<bool, IoError> {
        self.get().map(|v| !v).map_err(IoError::from)
    }
}

impl InputPin for GpioLine {
    fn is_high(&mut self) -> Result<bool, IoError> {
        self.get().map_err(IoError::from)
    }

    fn is_low(&mut self) -> Result<bool, IoError> {
        self.get().map(|v| !v).map_err(IoError::from)
    }
}

#[cfg(test)]
mod tests;
