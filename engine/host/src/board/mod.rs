//! The board file: which devices a board has and how they are wired. A parser
//! only. It knows no drivers and no kinds of device.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use lemnos_core::ConfigValue;
use lemnos_wire::ring::Layout;
use lemnos_wire::{MAX_HANDLE_BUFFERS, MAX_PLANES};
use serde::Deserialize;

use crate::contain::{MAX_RETRY_CAP, Options};

pub const FORMAT: &str = "lemnos.board";
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum BoardError {
    #[error("{0}")]
    Parse(String),
    #[error("format is {0:?}, expected {FORMAT:?}")]
    Format(String),
    #[error("schema_version {0} is newer than {SCHEMA_VERSION}")]
    Version(u32),
    #[error("device id {0:?} is used twice")]
    Duplicate(String),
    #[error("device {0:?} uses itself")]
    SelfUse(String),
    #[error("device {0:?} config key {1:?}: values are numbers, booleans or strings")]
    Config(String, String),
    #[error("device {0:?} stream {1:?}: its numbers are out of range")]
    Stream(String, String),
    #[error(
        "device {0:?} stream {1:?}: {2} buffers, and a stream of dma-bufs has at most \
         {MAX_HANDLE_BUFFERS}"
    )]
    StreamHandles(String, String, u32),
    #[error("device {0:?} stream {1:?}: heap {2:?} is not the name of a heap")]
    StreamHeap(String, String, String),
    #[error("device {0:?} stream {1:?}: `heap` is only for `memory = \"dma-buf\"`")]
    StreamHeapUnused(String, String),
    #[error(
        "device {0:?} stream {1:?}: `planes` are only for `memory = \"dma-buf\"`, 1 to \
         {MAX_PLANES} of them, none empty, adding up to `buffer_bytes`, and at most \
         {MAX_HANDLE_BUFFERS} descriptors over every buffer's planes"
    )]
    StreamPlanes(String, String),
    #[error("[host] {0}: {1}")]
    Host(&'static str, String),
    #[error(
        "device {0:?} auto_off_after: at least 1, the attempts in a row before it is turned off"
    )]
    AutoOff(String),
    #[error("id {0:?} has an `@`, which only the ids of devices an `[[each]]` entry makes have")]
    At(String),
    #[error("each {0:?}: `match` is empty")]
    EmptyMatch(String),
    #[error("each {0:?}: need {1:?} is named twice, by `need` and in `uses` or `streams`")]
    NeedTwice(String, String),
}

/// The longest `call_deadline_ms` a board file may set.
pub const MAX_CALL_DEADLINE_MS: u64 = 60_000;
/// The longest `retry_cap_ms` a board file may set: the failure rule's 8 s cap.
pub const MAX_RETRY_CAP_MS: u64 = MAX_RETRY_CAP.as_millis() as u64;

/// How the host treats every driver, from the board file's `[host]` section. Each key left out
/// keeps what the host was started with, which for `lemnosd` is the default in the table of
/// `docs/boards.md`. Nothing here can turn a device off: that is each device's own
/// `auto_off_after`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostSpec {
    /// How long one driver call may run, in milliseconds: 1 to [`MAX_CALL_DEADLINE_MS`].
    #[serde(default)]
    pub call_deadline_ms: Option<u64>,
    /// The wait before the first retry of a device that failed, in milliseconds. At least 1,
    /// and at most the cap.
    #[serde(default)]
    pub retry_first_ms: Option<u64>,
    /// The longest wait between retries, in milliseconds: at most [`MAX_RETRY_CAP_MS`]. It is
    /// also how long a driver must stay up before its wait starts again from the shortest.
    #[serde(default)]
    pub retry_cap_ms: Option<u64>,
}

impl HostSpec {
    /// `options` with what this section sets put over it.
    #[must_use]
    pub fn apply(&self, options: Options) -> Options {
        let ms = Duration::from_millis;
        Options {
            call_deadline: self.call_deadline_ms.map_or(options.call_deadline, ms),
            retry_first: self.retry_first_ms.map_or(options.retry_first, ms),
            retry_cap: self.retry_cap_ms.map_or(options.retry_cap, ms),
            ..options
        }
    }

    /// Checks each value against its range, and the first retry against the cap it doubles
    /// to. A first retry with no cap set is held to the default cap, which is the most there is.
    fn check(&self) -> Result<(), BoardError> {
        let out = |key, why: &str| Err(BoardError::Host(key, why.to_string()));
        if let Some(deadline) = self.call_deadline_ms
            && !(1..=MAX_CALL_DEADLINE_MS).contains(&deadline)
        {
            return out(
                "call_deadline_ms",
                &format!("{deadline} is not from 1 to {MAX_CALL_DEADLINE_MS}"),
            );
        }
        if let Some(cap) = self.retry_cap_ms
            && !(1..=MAX_RETRY_CAP_MS).contains(&cap)
        {
            return out(
                "retry_cap_ms",
                &format!(
                    "{cap} is not from 1 to {MAX_RETRY_CAP_MS}: the failure rule caps the wait at 8 s"
                ),
            );
        }
        let cap = self.retry_cap_ms.unwrap_or(MAX_RETRY_CAP_MS);
        if let Some(first) = self.retry_first_ms
            && !(1..=cap).contains(&first)
        {
            return out(
                "retry_first_ms",
                &format!("{first} is not from 1 to the cap, {cap}"),
            );
        }
        Ok(())
    }
}

/// What a stream's buffers are made of.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
pub enum StreamMemory {
    /// Memory shared with the consumer, and nothing more. A driver fills it with `write`.
    #[default]
    #[serde(rename = "shared")]
    Shared,
    /// One dma-buf per buffer, so a driver can also hand a buffer to hardware.
    #[serde(rename = "dma-buf")]
    DmaBuf,
}

/// The heap a stream of dma-bufs comes from when its board file names none.
pub const DEFAULT_HEAP: &str = "system";

/// One frame stream a device offers.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamSpec {
    pub buffers: u32,
    pub buffer_bytes: u32,
    /// How many jobs the consumer can have waiting.
    #[serde(default = "default_job_slots")]
    pub job_slots: u32,
    /// The largest job. Its meaning is between the consumer and the driver.
    #[serde(default = "default_job_bytes")]
    pub job_bytes: u32,
    /// `"shared"` when left out, or `"dma-buf"`.
    #[serde(default)]
    pub memory: StreamMemory,
    /// The dma-heap the buffers come from, by its name under `/dev/dma_heap`. Only with
    /// `memory = "dma-buf"`, where it is [`DEFAULT_HEAP`] when left out.
    #[serde(default)]
    pub heap: Option<String>,
    /// A buffer's memory planes, by length: each its own dma-buf, for hardware that writes a
    /// frame in several planes at addresses of their own. Only with `memory = "dma-buf"`, and
    /// `buffer_bytes` is their sum. Left out, a buffer is one plane.
    #[serde(default)]
    pub planes: Option<Vec<u32>>,
    /// Memory is kept ready while no consumer is attached, so the producer can fill buffers
    /// then and the next consumer gets the newest frame the moment it attaches, with nothing
    /// copied. The memory is still made anew after each consumer leaves.
    #[serde(default)]
    pub standby: bool,
}

const fn default_job_slots() -> u32 {
    4
}

const fn default_job_bytes() -> u32 {
    256
}

impl StreamSpec {
    fn checked_layout(&self) -> Option<Layout> {
        Layout::new(
            self.buffers,
            self.buffer_bytes,
            self.job_slots,
            self.job_bytes,
        )
        .ok()
    }

    /// The heap this stream's dma-bufs come from. Means nothing for a stream of shared memory.
    #[must_use]
    pub fn heap_name(&self) -> &str {
        self.heap.as_deref().unwrap_or(DEFAULT_HEAP)
    }

    /// The length of each plane of a buffer, in order: the board file's `planes`, or the whole
    /// buffer as one.
    #[must_use]
    pub fn plane_bytes(&self) -> Box<[u32]> {
        match &self.planes {
            Some(planes) => planes.as_slice().into(),
            None => Box::new([self.buffer_bytes]),
        }
    }

    /// Checks `planes`: dma-bufs only, within the limits, and adding up to the buffer.
    fn check_planes(&self, device: &str, name: &str) -> Result<(), BoardError> {
        let Some(planes) = &self.planes else {
            return Ok(());
        };
        let sum = planes
            .iter()
            .try_fold(0u32, |sum, plane| sum.checked_add(*plane));
        let count = u32::try_from(planes.len()).unwrap_or(u32::MAX);
        let fits = self.memory == StreamMemory::DmaBuf
            && (1..=MAX_PLANES).contains(&count)
            && planes.iter().all(|plane| *plane > 0)
            && sum == Some(self.buffer_bytes)
            && self
                .buffers
                .checked_mul(count)
                .is_some_and(|descriptors| descriptors <= MAX_HANDLE_BUFFERS);
        if fits {
            Ok(())
        } else {
            Err(BoardError::StreamPlanes(
                device.to_string(),
                name.to_string(),
            ))
        }
    }

    /// Checks what [`Layout`] doesn't: the memory the stream asks for.
    fn check_memory(&self, device: &str, name: &str) -> Result<(), BoardError> {
        self.check_planes(device, name)?;
        let (device, name) = (device.to_string(), name.to_string());
        match (self.memory, &self.heap) {
            (StreamMemory::Shared, None) => Ok(()),
            (StreamMemory::Shared, Some(_)) => Err(BoardError::StreamHeapUnused(device, name)),
            (StreamMemory::DmaBuf, heap) => {
                if self.buffers > MAX_HANDLE_BUFFERS {
                    return Err(BoardError::StreamHandles(device, name, self.buffers));
                }
                match heap {
                    Some(heap) if !is_heap_name(heap) => {
                        Err(BoardError::StreamHeap(device, name, heap.clone()))
                    }
                    _ => Ok(()),
                }
            }
        }
    }

    /// The stream's layout. A spec that didn't come through [`parse`] and is out of range falls
    /// back to the smallest stream there is.
    pub(crate) fn layout(&self) -> Layout {
        self.checked_layout().unwrap_or(Layout {
            buffers: 1,
            buffer_bytes: 1,
            frame_slots: 1,
            job_slots: 1,
            job_bytes: 0,
        })
    }
}

/// A heap's name is one file name: it is looked up in one directory and never leads out of it.
fn is_heap_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b','))
}

/// Who owns a device's hardware.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Owner {
    /// Lemnos drives it.
    #[default]
    Lemnos,
    /// The operating system's own driver does, and Lemnos is a guest on it: a call reaches the
    /// driver only when the device's description says the method only reads.
    Host,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoardInfo {
    pub id: String,
    /// The SoC map that says what this board's hardware is (`socs/<soc>.toml`). Only the device
    /// tree generator reads it: the daemon does not.
    #[serde(default)]
    pub soc: Option<String>,
    /// The base tree the board is built on, by its name in the SoC map. Only the generator reads it.
    #[serde(default)]
    pub base: Option<String>,
}

/// A piece of hardware the board uses: a controller of the SoC map, with the pin set it is wired
/// through. The runtime name a device uses for it is `name`, when the device names it. Only the
/// device tree generator reads these; the daemon does not.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HardwareSpec {
    /// The name devices use for it, as `i2c-1`, `spidev0.0` or `udc:...`. Left out for hardware
    /// that no device names by it, such as the watchdog.
    #[serde(default)]
    pub name: Option<String>,
    /// The controller's name in the SoC map.
    pub controller: String,
    /// The pin set in the map that wires the controller, when it has more than one.
    #[serde(default)]
    pub pins: Option<String>,
}

/// A GPIO line the board gives to Lemnos, and the state the line is in before the kernel runs.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LineSpec {
    /// The line in the SoC's naming, as `rp1:34`.
    pub line: String,
    /// `"low"` or `"high"`: the level the firmware drives the line to at boot, held until the
    /// kernel takes it. Left out, the firmware leaves the line alone.
    #[serde(default)]
    pub boot: Option<String>,
    /// Why the board holds the line in that state, written beside the firmware line. Required with
    /// `boot`.
    #[serde(default)]
    pub reason: Option<String>,
}

/// One device.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceSpec {
    pub id: String,
    pub driver: String,
    /// The bus or block it sits on. What the text means is the backend's.
    #[serde(default)]
    pub bus: Option<String>,
    /// Need name to device id or bus name.
    #[serde(default)]
    pub uses: BTreeMap<String, String>,
    /// Another device this one is wired to. The host only hands it over.
    #[serde(default)]
    pub link: Option<String>,
    #[serde(default)]
    pub config: toml::Table,
    #[serde(default)]
    pub disabled: bool,
    /// `"lemnos"` when left out.
    #[serde(default)]
    pub owner: Owner,
    /// The frame streams this device offers, by the name a consumer asks for and a driver needs.
    #[serde(default)]
    pub streams: BTreeMap<String, StreamSpec>,
    /// Opts this device in to being turned off after this many failed binds in a row. Left
    /// out, it is retried forever, which is the failure rule.
    #[serde(default)]
    pub auto_off_after: Option<u32>,
}

/// One device for each resource the backend finds that fits a pattern: a USB camera of any make
/// plugged into any port, say. The host looks when it starts, when a backend hints that
/// something under the pattern came or went, and every retry cap without a hint.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EachSpec {
    /// The devices made are named `ID@RESOURCE`, after the name the backend found the resource
    /// by, so a device keeps its id for as long as its hardware is wired the same way.
    pub id: String,
    pub driver: String,
    /// The pattern, in the backend's terms, as any resource name is.
    #[serde(rename = "match")]
    pub pattern: String,
    /// The need the resource found fills. Left out, it is the device's `bus`.
    #[serde(default)]
    pub need: Option<String>,
    /// Names and patterns this entry never takes a resource by.
    #[serde(default)]
    pub except: Vec<String>,
    /// Other needs, as a device's `uses`.
    #[serde(default)]
    pub uses: BTreeMap<String, String>,
    #[serde(default)]
    pub config: toml::Table,
    #[serde(default)]
    pub owner: Owner,
    /// The streams each device made offers.
    #[serde(default)]
    pub streams: BTreeMap<String, StreamSpec>,
    /// As in a device, for each device made.
    #[serde(default)]
    pub auto_off_after: Option<u32>,
}

/// What joins an `[[each]]` entry's id to the resource a device of it was made for.
pub const EACH_SEPARATOR: char = '@';

impl EachSpec {
    /// The id of the device made for the resource `found`.
    #[must_use]
    pub fn device_id(&self, found: &str) -> String {
        format!("{}{EACH_SEPARATOR}{found}", self.id)
    }

    /// The board entry of the device made for the resource `found`.
    #[must_use]
    pub fn device(&self, found: &str) -> DeviceSpec {
        let mut uses = self.uses.clone();
        let bus = match &self.need {
            Some(need) => {
                uses.insert(need.clone(), found.to_string());
                None
            }
            None => Some(found.to_string()),
        };
        DeviceSpec {
            id: self.device_id(found),
            driver: self.driver.clone(),
            bus,
            uses,
            link: None,
            config: self.config.clone(),
            disabled: false,
            owner: self.owner,
            streams: self.streams.clone(),
            auto_off_after: self.auto_off_after,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoardFile {
    pub format: String,
    pub schema_version: u32,
    pub board: BoardInfo,
    /// How the host treats every driver. Left out, it keeps what it was started with.
    #[serde(default)]
    pub host: HostSpec,
    #[serde(default)]
    pub devices: Vec<DeviceSpec>,
    /// Devices made for whatever the backend finds that fits.
    #[serde(default)]
    pub each: Vec<EachSpec>,
    /// The hardware the devices use, for the device tree generator.
    #[serde(default)]
    pub hardware: Vec<HardwareSpec>,
    /// The GPIO lines the board gives to Lemnos, for the device tree generator.
    #[serde(default)]
    pub lines: Vec<LineSpec>,
}

/// A config value the host owns.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum OwnedValue {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
}

impl OwnedValue {
    pub(crate) fn as_value(&self) -> ConfigValue<'_> {
        match self {
            Self::Int(v) => ConfigValue::Int(*v),
            Self::Float(v) => ConfigValue::Float(*v),
            Self::Bool(v) => ConfigValue::Bool(*v),
            Self::Str(v) => ConfigValue::Str(v),
        }
    }
}

impl DeviceSpec {
    pub(crate) fn owned_config(&self) -> Result<Vec<(String, OwnedValue)>, BoardError> {
        self.config
            .iter()
            .map(|(key, value)| {
                let owned = match value {
                    toml::Value::Integer(v) => OwnedValue::Int(*v),
                    toml::Value::Float(v) => OwnedValue::Float(*v),
                    toml::Value::Boolean(v) => OwnedValue::Bool(*v),
                    toml::Value::String(v) => OwnedValue::Str(v.clone()),
                    _ => return Err(BoardError::Config(self.id.clone(), key.clone())),
                };
                Ok((key.clone(), owned))
            })
            .collect()
    }
}

/// Parses and checks a board file. Unknown keys are refused by name.
pub fn parse(text: &str) -> Result<BoardFile, BoardError> {
    let board: BoardFile = toml::from_str(text).map_err(|e| BoardError::Parse(e.to_string()))?;
    if board.format != FORMAT {
        return Err(BoardError::Format(board.format));
    }
    if board.schema_version > SCHEMA_VERSION {
        return Err(BoardError::Version(board.schema_version));
    }
    board.host.check()?;
    let mut seen = BTreeSet::new();
    for device in &board.devices {
        if !seen.insert(device.id.as_str()) {
            return Err(BoardError::Duplicate(device.id.clone()));
        }
        if device.id.contains(EACH_SEPARATOR) {
            return Err(BoardError::At(device.id.clone()));
        }
        if device.uses.values().any(|used| *used == device.id) {
            return Err(BoardError::SelfUse(device.id.clone()));
        }
        if device.auto_off_after == Some(0) {
            return Err(BoardError::AutoOff(device.id.clone()));
        }
        device.owned_config()?;
        check_streams(&device.id, &device.streams)?;
    }
    let mut rules = BTreeSet::new();
    for each in &board.each {
        if !rules.insert(each.id.as_str()) {
            return Err(BoardError::Duplicate(each.id.clone()));
        }
        if each.id.contains(EACH_SEPARATOR) {
            return Err(BoardError::At(each.id.clone()));
        }
        if each.pattern.is_empty() {
            return Err(BoardError::EmptyMatch(each.id.clone()));
        }
        if each.auto_off_after == Some(0) {
            return Err(BoardError::AutoOff(each.id.clone()));
        }
        if let Some(need) = &each.need
            && (each.uses.contains_key(need) || each.streams.contains_key(need))
        {
            return Err(BoardError::NeedTwice(each.id.clone(), need.clone()));
        }
        // Checked as the entry of a device made for it would be.
        let mut device = each.device(&each.pattern);
        device.id.clone_from(&each.id);
        device.owned_config()?;
        check_streams(&each.id, &each.streams)?;
    }
    Ok(board)
}

fn check_streams(id: &str, streams: &BTreeMap<String, StreamSpec>) -> Result<(), BoardError> {
    for (name, stream) in streams {
        if stream.checked_layout().is_none() {
            return Err(BoardError::Stream(id.to_string(), name.clone()));
        }
        stream.check_memory(id, name)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEAD: &str = "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n";

    fn with(devices: &str) -> Result<BoardFile, BoardError> {
        parse(&format!("{HEAD}{devices}"))
    }

    #[test]
    fn a_full_device_parses() {
        let board = with(
            r#"
            [[devices]]
            id = "cam0-sensor"
            driver = "some-chip"
            bus = "i2c-cam0"
            link = "csi0"
            disabled = true
            uses = { supply = "rail" }
            config = { rate = 120, gain = 1.5, flip = true, mode = "fast" }
            "#,
        )
        .unwrap();
        let device = &board.devices[0];
        assert_eq!(device.bus.as_deref(), Some("i2c-cam0"));
        assert_eq!(device.link.as_deref(), Some("csi0"));
        assert!(device.disabled);
        assert_eq!(device.owner, Owner::Lemnos);
        assert_eq!(device.uses["supply"], "rail");
        let config = device.owned_config().unwrap();
        assert!(config.contains(&("rate".to_string(), OwnedValue::Int(120))));
        assert!(config.contains(&("mode".to_string(), OwnedValue::Str("fast".to_string()))));
    }

    #[test]
    fn no_devices_is_a_board() {
        assert!(with("").unwrap().devices.is_empty());
    }

    #[test]
    fn an_unknown_key_is_refused_by_name() {
        let error = with("[[devices]]\nid = \"a\"\ndriver = \"b\"\nspeeed = 1\n").unwrap_err();
        assert!(error.to_string().contains("speeed"), "{error}");
    }

    #[test]
    fn an_unknown_top_level_key_is_refused_by_name() {
        let error = parse(&format!("{HEAD}looks = 1\n")).unwrap_err();
        assert!(error.to_string().contains("looks"), "{error}");
    }

    #[test]
    fn the_wrong_format_is_refused() {
        let text = HEAD.replace("lemnos.board", "something.else");
        assert_eq!(
            parse(&text),
            Err(BoardError::Format("something.else".to_string()))
        );
    }

    #[test]
    fn a_newer_schema_is_refused() {
        let text = HEAD.replace("schema_version = 1", "schema_version = 2");
        assert_eq!(parse(&text), Err(BoardError::Version(2)));
    }

    #[test]
    fn a_duplicate_id_is_refused() {
        let text =
            "[[devices]]\nid = \"a\"\ndriver = \"x\"\n[[devices]]\nid = \"a\"\ndriver = \"y\"\n";
        assert_eq!(with(text), Err(BoardError::Duplicate("a".to_string())));
    }

    #[test]
    fn a_device_using_itself_is_refused() {
        let text = "[[devices]]\nid = \"a\"\ndriver = \"x\"\nuses = { b = \"a\" }\n";
        assert_eq!(with(text), Err(BoardError::SelfUse("a".to_string())));
    }

    #[test]
    fn a_nested_config_value_is_refused() {
        let text = "[[devices]]\nid = \"a\"\ndriver = \"x\"\nconfig = { list = [1] }\n";
        assert_eq!(
            with(text),
            Err(BoardError::Config("a".to_string(), "list".to_string()))
        );
    }

    #[test]
    fn a_stream_parses_and_its_job_numbers_default() {
        let board = with(
            "[[devices]]\nid = \"a\"\ndriver = \"x\"\n\
             [devices.streams.out]\nbuffers = 4\nbuffer_bytes = 1024\n",
        )
        .unwrap();
        let stream = &board.devices[0].streams["out"];
        assert_eq!(stream.buffers, 4);
        assert_eq!(stream.job_slots, 4);
        assert_eq!(stream.job_bytes, 256);
        assert_eq!(stream.memory, StreamMemory::Shared);
        assert_eq!(stream.heap, None);
    }

    fn stream(keys: &str) -> Result<StreamSpec, BoardError> {
        with(&format!(
            "[[devices]]\nid = \"a\"\ndriver = \"x\"\n\
             [devices.streams.out]\nbuffers = 4\nbuffer_bytes = 1024\n{keys}"
        ))
        .map(|board| board.devices[0].streams["out"].clone())
    }

    #[test]
    fn a_stream_is_shared_memory_unless_it_asks_for_dma_bufs() {
        assert_eq!(stream("").unwrap().memory, StreamMemory::Shared);
        assert_eq!(
            stream("memory = \"shared\"\n").unwrap().memory,
            StreamMemory::Shared
        );
        let asked = stream("memory = \"dma-buf\"\n").unwrap();
        assert_eq!(asked.memory, StreamMemory::DmaBuf);
        assert_eq!(asked.heap_name(), "system");
        let named = stream("memory = \"dma-buf\"\nheap = \"linux,cma\"\n").unwrap();
        assert_eq!(named.heap_name(), "linux,cma");
    }

    #[test]
    fn a_memory_that_isnt_known_is_refused_by_name() {
        let error = stream("memory = \"dmabuf\"\n").unwrap_err();
        assert!(error.to_string().contains("dmabuf"), "{error}");
        assert!(error.to_string().contains("memory"), "{error}");
        assert!(error.to_string().contains("dma-buf"), "{error}");
    }

    #[test]
    fn a_heap_is_one_file_name_and_only_for_dma_bufs() {
        for bad in [
            "",
            ".",
            "..",
            "../mem",
            "a/b",
            "/dev/dma_heap/system",
            "a b",
        ] {
            assert_eq!(
                stream(&format!("memory = \"dma-buf\"\nheap = \"{bad}\"\n")),
                Err(BoardError::StreamHeap(
                    "a".to_string(),
                    "out".to_string(),
                    bad.to_string()
                )),
                "{bad:?}"
            );
        }
        assert_eq!(
            stream("heap = \"system\"\n"),
            Err(BoardError::StreamHeapUnused(
                "a".to_string(),
                "out".to_string()
            ))
        );
    }

    #[test]
    fn a_stream_of_dma_bufs_has_at_most_as_many_buffers_as_one_message_carries() {
        let board = |buffers: u32, memory: &str| {
            with(&format!(
                "[[devices]]\nid = \"a\"\ndriver = \"x\"\n\
                 [devices.streams.out]\nbuffers = {buffers}\nbuffer_bytes = 64\n{memory}"
            ))
        };
        let dma = "memory = \"dma-buf\"\n";
        assert!(board(MAX_HANDLE_BUFFERS, dma).is_ok());
        assert_eq!(
            board(MAX_HANDLE_BUFFERS + 1, dma),
            Err(BoardError::StreamHandles(
                "a".to_string(),
                "out".to_string(),
                MAX_HANDLE_BUFFERS + 1
            ))
        );
        // Shared memory keeps the limit it had.
        assert!(board(MAX_HANDLE_BUFFERS + 1, "").is_ok());
    }

    #[test]
    fn a_stream_of_planes_adds_up_to_its_buffer_and_is_dma_bufs() {
        let dma = "memory = \"dma-buf\"\n";
        let planar = stream(&format!("{dma}planes = [768, 256]\n")).unwrap();
        assert_eq!(&*planar.plane_bytes(), &[768, 256]);
        assert_eq!(&*stream(dma).unwrap().plane_bytes(), &[1024], "one plane");
        assert!(!planar.standby, "off unless asked for");
        assert!(stream("standby = true\n").unwrap().standby);
        let refused = Err(BoardError::StreamPlanes("a".to_string(), "out".to_string()));
        for bad in [
            format!("{dma}planes = [768, 255]\n"),
            format!("{dma}planes = [1024, 0]\n"),
            format!("{dma}planes = []\n"),
            format!("{dma}planes = [128, 128, 128, 128, 128, 128, 128, 64, 64]\n"),
            format!("{dma}planes = [4294967295, 1025]\n"),
            "planes = [768, 256]\n".to_string(),
        ] {
            assert_eq!(stream(&bad), refused, "{bad}");
        }
        // Every buffer's planes travel in one message.
        let board = |buffers: u32| {
            with(&format!(
                "[[devices]]\nid = \"a\"\ndriver = \"x\"\n\
                 [devices.streams.out]\nbuffers = {buffers}\nbuffer_bytes = 64\n{dma}\
                 planes = [32, 32]\n"
            ))
        };
        assert!(board(MAX_HANDLE_BUFFERS / 2).is_ok());
        assert_eq!(
            board(MAX_HANDLE_BUFFERS / 2 + 1),
            Err(BoardError::StreamPlanes("a".to_string(), "out".to_string()))
        );
    }

    #[test]
    fn a_stream_out_of_range_is_refused_by_name() {
        let text = "[[devices]]\nid = \"a\"\ndriver = \"x\"\n\
                    [devices.streams.out]\nbuffers = 0\nbuffer_bytes = 1024\n";
        assert_eq!(
            with(text),
            Err(BoardError::Stream("a".to_string(), "out".to_string()))
        );
    }

    #[test]
    fn owner_is_lemnos_or_host_and_anything_else_is_refused_by_name() {
        let device =
            |owner: &str| with(&format!("[[devices]]\nid = \"a\"\ndriver = \"x\"\n{owner}"));
        assert_eq!(device("").unwrap().devices[0].owner, Owner::Lemnos);
        assert_eq!(
            device("owner = \"lemnos\"\n").unwrap().devices[0].owner,
            Owner::Lemnos
        );
        assert_eq!(
            device("owner = \"host\"\n").unwrap().devices[0].owner,
            Owner::Host
        );
        let error = device("owner = \"guest\"\n").unwrap_err();
        assert!(error.to_string().contains("guest"), "{error}");
        assert!(error.to_string().contains("owner"), "{error}");
    }

    const CAMERA: &str = r#"
        [[each]]
        id = "camera"
        driver = "uvc"
        match = "usb:class:0e"
        need = "usb"
        except = ["usb:1-1.3"]
        uses = { light = "lamp" }
        config = { frame_bytes = 4096 }
        [each.streams.frames]
        buffers = 4
        buffer_bytes = 4096
        "#;

    #[test]
    fn an_each_entry_parses_and_makes_a_device_per_resource() {
        let board = with(CAMERA).unwrap();
        let each = &board.each[0];
        assert_eq!(each.pattern, "usb:class:0e");
        assert_eq!(each.except, ["usb:1-1.3"]);
        let device = each.device("usb:1-1.4");
        assert_eq!(device.id, "camera@usb:1-1.4");
        assert_eq!(device.driver, "uvc");
        assert_eq!(device.bus, None);
        assert_eq!(device.uses["usb"], "usb:1-1.4");
        assert_eq!(device.uses["light"], "lamp");
        assert_eq!(device.streams["frames"].buffers, 4);
        assert_eq!(
            device.owned_config().unwrap(),
            [("frame_bytes".to_string(), OwnedValue::Int(4096))]
        );
    }

    #[test]
    fn an_each_entry_without_a_need_gives_the_resource_as_the_bus() {
        let board = with("[[each]]\nid = \"c\"\ndriver = \"x\"\nmatch = \"p\"\n").unwrap();
        let device = board.each[0].device("r");
        assert_eq!(device.bus.as_deref(), Some("r"));
        assert!(device.uses.is_empty());
    }

    #[test]
    fn an_each_entry_is_checked_as_a_device_is() {
        let entry = |extra: &str| {
            with(&format!(
                "[[each]]\nid = \"c\"\ndriver = \"x\"\nmatch = \"p\"\n{extra}"
            ))
        };
        let error = entry("matches = 1\n").unwrap_err();
        assert!(error.to_string().contains("matches"), "{error}");
        assert_eq!(
            entry("config = { list = [1] }\n"),
            Err(BoardError::Config("c".to_string(), "list".to_string()))
        );
        assert_eq!(
            entry("[each.streams.out]\nbuffers = 0\nbuffer_bytes = 1\n"),
            Err(BoardError::Stream("c".to_string(), "out".to_string()))
        );
        assert_eq!(
            entry("need = \"usb\"\nuses = { usb = \"a\" }\n"),
            Err(BoardError::NeedTwice("c".to_string(), "usb".to_string()))
        );
        assert_eq!(
            with("[[each]]\nid = \"c\"\ndriver = \"x\"\nmatch = \"\"\n"),
            Err(BoardError::EmptyMatch("c".to_string()))
        );
        let error = with("[[each]]\nid = \"c\"\ndriver = \"x\"\n").unwrap_err();
        assert!(error.to_string().contains("match"), "{error}");
        let twice = "[[each]]\nid = \"c\"\ndriver = \"x\"\nmatch = \"p\"\n";
        assert_eq!(
            with(&format!("{twice}{twice}")),
            Err(BoardError::Duplicate("c".to_string()))
        );
    }

    #[test]
    fn an_at_is_only_for_devices_an_each_entry_makes() {
        assert_eq!(
            with("[[devices]]\nid = \"a@b\"\ndriver = \"x\"\n"),
            Err(BoardError::At("a@b".to_string()))
        );
        assert_eq!(
            with("[[each]]\nid = \"a@b\"\ndriver = \"x\"\nmatch = \"p\"\n"),
            Err(BoardError::At("a@b".to_string()))
        );
    }

    #[test]
    fn host_settings_default_to_what_the_host_was_started_with() {
        let board = with("").unwrap();
        assert_eq!(board.host, HostSpec::default());
        assert_eq!(board.host.apply(Options::default()), Options::default());
        let started = Options {
            call_deadline: Duration::from_millis(30),
            ..Options::default()
        };
        assert_eq!(board.host.apply(started), started);
    }

    #[test]
    fn host_settings_parse_and_win_over_what_the_host_was_started_with() {
        let board = parse(&format!(
            "{HEAD}[host]\ncall_deadline_ms = 500\nretry_first_ms = 250\nretry_cap_ms = 4000\n"
        ))
        .unwrap();
        let options = board.host.apply(Options::default());
        assert_eq!(options.call_deadline, Duration::from_millis(500));
        assert_eq!(options.retry_first, Duration::from_millis(250));
        assert_eq!(options.retry_cap, Duration::from_secs(4));
        assert_eq!(options.error_limit, Options::default().error_limit);
        // One key alone changes that one only.
        let board = parse(&format!("{HEAD}[host]\nretry_first_ms = 100\n")).unwrap();
        let options = board.host.apply(Options::default());
        assert_eq!(options.retry_first, Duration::from_millis(100));
        assert_eq!(options.retry_cap, MAX_RETRY_CAP);
        assert_eq!(options.call_deadline, Options::default().call_deadline);
    }

    fn host(keys: &str) -> Result<BoardFile, BoardError> {
        parse(&format!("{HEAD}[host]\n{keys}"))
    }

    #[test]
    fn the_retry_cap_never_goes_past_eight_seconds() {
        assert!(host("retry_cap_ms = 8000\n").is_ok());
        for bad in ["8001", "60000", "0"] {
            let error = host(&format!("retry_cap_ms = {bad}\n")).unwrap_err();
            assert!(
                matches!(error, BoardError::Host("retry_cap_ms", _)),
                "{bad}: {error}"
            );
        }
        // Negative and too large for the type are not numbers of milliseconds at all.
        assert!(host("retry_cap_ms = -1\n").is_err());
    }

    #[test]
    fn the_first_retry_is_at_most_the_cap_and_never_zero() {
        assert!(host("retry_first_ms = 8000\n").is_ok());
        assert!(host("retry_first_ms = 2000\nretry_cap_ms = 2000\n").is_ok());
        for keys in [
            "retry_first_ms = 0\n",
            "retry_first_ms = 8001\n",
            "retry_first_ms = 3000\nretry_cap_ms = 2000\n",
        ] {
            let error = host(keys).unwrap_err();
            assert!(
                matches!(error, BoardError::Host("retry_first_ms", _)),
                "{keys}: {error}"
            );
        }
    }

    #[test]
    fn the_call_deadline_is_in_range() {
        assert!(host("call_deadline_ms = 1\n").is_ok());
        assert!(host(&format!("call_deadline_ms = {MAX_CALL_DEADLINE_MS}\n")).is_ok());
        for bad in [0, MAX_CALL_DEADLINE_MS + 1] {
            let error = host(&format!("call_deadline_ms = {bad}\n")).unwrap_err();
            assert!(
                matches!(error, BoardError::Host("call_deadline_ms", _)),
                "{bad}: {error}"
            );
        }
    }

    #[test]
    fn an_unknown_host_key_is_refused_by_name() {
        let error = host("auto_off_after = 3\n").unwrap_err();
        assert!(error.to_string().contains("auto_off_after"), "{error}");
        let error = host("retry_cap = 8000\n").unwrap_err();
        assert!(error.to_string().contains("retry_cap"), "{error}");
    }

    #[test]
    fn auto_off_is_each_devices_own_and_at_least_one_attempt() {
        let board = with(
            "[[devices]]\nid = \"a\"\ndriver = \"x\"\nauto_off_after = 3\n\
             [[devices]]\nid = \"b\"\ndriver = \"x\"\n",
        )
        .unwrap();
        assert_eq!(board.devices[0].auto_off_after, Some(3));
        assert_eq!(board.devices[1].auto_off_after, None);
        assert_eq!(
            with("[[devices]]\nid = \"a\"\ndriver = \"x\"\nauto_off_after = 0\n"),
            Err(BoardError::AutoOff("a".to_string()))
        );
    }

    #[test]
    fn missing_required_keys_are_named() {
        let error = with("[[devices]]\nid = \"a\"\n").unwrap_err();
        assert!(error.to_string().contains("driver"), "{error}");
    }
}
