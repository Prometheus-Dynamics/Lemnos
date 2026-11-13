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
    StreamRingHandles(String, String, u32),
    #[error("device {0:?} stream {1:?}: heap {2:?} is not the name of a heap")]
    StreamHeap(String, String, String),
    #[error("device {0:?} stream {1:?}: `heap` is only for `memory = \"dma-buf\"`")]
    StreamHeapUnused(String, String),
    #[error(
        "device {0:?} stream {1:?}: `readers` is 0 to {MAX_READER_SLOTS}, `reader_held_max` is 1 \
         to the stream's buffers, `reader_hold_ms_max` is 1 to {MAX_READER_HOLD_MS}, and \
         `tail_bytes` is 0 to {MAX_TAIL_BYTES}, only with `memory = \"dma-buf\"`"
    )]
    StreamReaders(String, String),
    #[error("device {0:?} stream {1:?}: {2}")]
    StreamWriter(String, String, String),
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
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
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
    /// The dma-heaps the daemon opens, by name under `/dev/dma_heap`: the heaps its own drivers
    /// allocate from, which are not named by a stream. Left out, it is [`DEFAULT_HOST_HEAP`];
    /// an empty list names none. Each is one file name, as a stream's `heap` is. The generated
    /// udev fragment and the daemon's start check read the set with [`BoardFile::dma_heaps`].
    #[serde(default)]
    pub dma_heaps: Option<Vec<String>>,
}

/// The dma-heap a board's drivers allocate from when its `[host]` section names none: the CMA heap.
pub const DEFAULT_HOST_HEAP: &str = "linux,cma";

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
        if let Some(heaps) = &self.dma_heaps
            && let Some(bad) = heaps.iter().find(|name| !is_heap_name(name))
        {
            return out(
                "dma_heaps",
                &format!("{bad:?} is not the name of a heap: one file name under /dev/dma_heap"),
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
    /// How many outside readers the stream's consumer may give a slot to at once, beyond itself:
    /// 0 to 63. A reader is a third process that reads the stream's own ring and buffers, read-only,
    /// with no connection to the daemon. Left out: 4 for a stream of shared memory, and 0 for a stream
    /// of dma-bufs (their buffers are then writable, as they always were; see `writer`).
    #[serde(default)]
    pub readers: Option<u32>,
    /// Who writes a stream of dma-bufs on the CPU: `"hardware"` (the default: a device, or a kernel
    /// driver through its own mapping, fills the buffers; nothing maps them writable here) or
    /// `"consumer"` (the consumer maps them writable and fills them). Only with `memory = "dma-buf"`.
    /// A stream of dma-bufs that readers may read is `"hardware"`: a read-only buffer can't be filled by
    /// a CPU writer. The daemon never writes a dma-buf on the CPU, so there is no daemon writer.
    #[serde(default)]
    pub writer: StreamWriter,
    /// The most buffers any one outside reader may be granted at once, 1 to the stream's
    /// `buffers`. A reader asks for fewer; the producer always keeps at least two buffers.
    #[serde(default)]
    pub reader_held_max: Option<u32>,
    /// The longest any one outside reader may be granted to hold one buffer, in milliseconds,
    /// 1 to 10000. A reader that holds one longer is cut off.
    #[serde(default = "default_reader_hold_ms_max")]
    pub reader_hold_ms_max: u32,
    /// Whether readers may import this stream's dma-bufs (hand their descriptors to a device) while the
    /// daemon runs in signed mode. Importing lets the importer write the frame for every other reader, so
    /// it defaults to off. With the daemon's signed mode off, imports are granted regardless. Only with
    /// `memory = "dma-buf"`.
    #[serde(default)]
    pub reader_import: bool,
    /// Bytes of slack past each buffer, for an importer (a GPU wants 256): 0 to 4096, and only
    /// with `memory = "dma-buf"`. Each dma-buf plane is allocated this much longer.
    #[serde(default)]
    pub tail_bytes: u32,
}

/// The most outside readers one stream can have: the pool page's cut-off words, less the consumer's.
pub const MAX_READER_SLOTS: u32 = shm_ring_buffers::MAX_READERS - 1;
/// The longest a reader may hold one buffer.
pub const MAX_READER_HOLD_MS: u32 = 10_000;

/// The default number of outside readers of a stream of shared memory.
const DEFAULT_SHARED_READERS: u32 = 4;

/// Who writes a stream of dma-bufs on the CPU. See [`StreamSpec::writer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
pub enum StreamWriter {
    /// A device, or a kernel driver through its own mapping, fills the buffers.
    #[default]
    #[serde(rename = "hardware")]
    Hardware,
    /// The consumer maps the buffers writable and fills them.
    #[serde(rename = "consumer")]
    Consumer,
}

/// The most buffers a reader is granted when the board doesn't say: four, or the stream's buffers if fewer.
const DEFAULT_RESERVED_HELD_MAX: u32 = 4;

const fn default_reader_hold_ms_max() -> u32 {
    2000
}

/// The most slack a buffer may have past its end.
pub const MAX_TAIL_BYTES: u32 = 4096;

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

    /// Checks the reader limits: the slots, and what one reader may hold, and for how long.
    fn check_readers(&self, device: &str, name: &str) -> Result<(), BoardError> {
        let readers = self.readers();
        let fits = readers <= MAX_READER_SLOTS
            && self
                .reader_held_max
                .is_none_or(|max| (1..=self.buffers).contains(&max))
            && (1..=MAX_READER_HOLD_MS).contains(&self.reader_hold_ms_max)
            && self.tail_bytes <= MAX_TAIL_BYTES
            && (self.tail_bytes == 0 || self.memory == StreamMemory::DmaBuf);
        if !fits {
            return Err(BoardError::StreamReaders(
                device.to_string(),
                name.to_string(),
            ));
        }
        if self.reader_import && self.memory == StreamMemory::Shared {
            return Err(BoardError::StreamWriter(
                device.to_string(),
                name.to_string(),
                "`reader_import` is only for `memory = \"dma-buf\"`".to_string(),
            ));
        }
        if self.writer == StreamWriter::Consumer && self.memory == StreamMemory::Shared {
            return Err(BoardError::StreamWriter(
                device.to_string(),
                name.to_string(),
                "`writer` is only for `memory = \"dma-buf\"`".to_string(),
            ));
        }
        // Readers need buffers nothing maps writable on the CPU. A stream the consumer fills is mapped
        // writable, so it can't also be read by readers.
        if self.memory == StreamMemory::DmaBuf
            && self.writer == StreamWriter::Consumer
            && readers > 0
        {
            return Err(BoardError::StreamWriter(
                device.to_string(),
                name.to_string(),
                format!(
                    "`readers = {readers}` needs read-only buffers, and a stream the consumer \
                     fills (`writer = \"consumer\"`) is mapped writable: set `readers = 0`"
                ),
            ));
        }
        Ok(())
    }

    /// How many outside readers the stream takes: `readers` as the board file gives it, or the
    /// default for the stream's memory.
    #[must_use]
    pub fn readers(&self) -> u32 {
        self.readers.unwrap_or(match self.memory {
            StreamMemory::Shared => DEFAULT_SHARED_READERS,
            StreamMemory::DmaBuf => 0,
        })
    }

    /// The most buffers one reader may be granted: `reader_held_max`, or four, and never more than the
    /// stream's buffers.
    #[must_use]
    pub fn reader_held_max(&self) -> u32 {
        self.reader_held_max
            .unwrap_or(DEFAULT_RESERVED_HELD_MAX)
            .min(self.buffers)
    }

    /// Whether the stream's dma-bufs are allocated read-only: a stream of dma-bufs that has reader
    /// slots and is filled by hardware. Nothing maps them writable on the CPU, so no reader can be
    /// given a writable one.
    #[must_use]
    pub fn read_only_buffers(&self) -> bool {
        self.memory == StreamMemory::DmaBuf
            && self.writer == StreamWriter::Hardware
            && self.readers() > 0
    }

    /// Checks what [`Layout`] doesn't: the memory the stream asks for.
    fn check_memory(&self, device: &str, name: &str) -> Result<(), BoardError> {
        self.check_planes(device, name)?;
        self.check_readers(device, name)?;
        let (device, name) = (device.to_string(), name.to_string());
        match (self.memory, &self.heap) {
            (StreamMemory::Shared, None) => Ok(()),
            (StreamMemory::Shared, Some(_)) => Err(BoardError::StreamHeapUnused(device, name)),
            (StreamMemory::DmaBuf, heap) => {
                if self.buffers > MAX_HANDLE_BUFFERS {
                    return Err(BoardError::StreamRingHandles(device, name, self.buffers));
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

impl BoardFile {
    /// Every dma-heap the daemon opens for this board, by name, sorted and without repeats: the
    /// `[host]` section's `dma_heaps` (or [`DEFAULT_HOST_HEAP`]), and the heap of each dma-buf
    /// stream of every device and `[[each]]` entry. A stream that names no heap is on
    /// [`DEFAULT_HEAP`], which is what the daemon allocates from for it.
    #[must_use]
    pub fn dma_heaps(&self) -> Vec<String> {
        let mut heaps = std::collections::BTreeSet::new();
        match &self.host.dma_heaps {
            Some(named) => heaps.extend(named.iter().cloned()),
            None => {
                heaps.insert(DEFAULT_HOST_HEAP.to_owned());
            }
        }
        let streams = self
            .devices
            .iter()
            .flat_map(|device| device.streams.values())
            .chain(self.each.iter().flat_map(|each| each.streams.values()));
        for stream in streams.filter(|s| s.memory == StreamMemory::DmaBuf) {
            heaps.insert(stream.heap_name().to_owned());
        }
        heaps.into_iter().collect()
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
    /// The kernel module's role for this node, when the module drives it rather than a stock
    /// driver: `"owned"` for a node the module owns, or the role a driver needs of it (such as
    /// `"pisp_be"`). Only the device tree generator reads it, to write the module's options.
    #[serde(default)]
    pub module: Option<String>,
    /// Whether the module may map the node's registers though no IOMMU confines its DMA. Needs
    /// `module`. Only the generator reads it.
    #[serde(default)]
    pub unconfined: bool,
    /// Whether an IOMMU really confines the node's DMA, so the module may map its registers.
    /// Needs `module`. Only the generator reads it.
    #[serde(default)]
    pub confined: bool,
    /// The port's role, for a controller the map makes a USB device port: `"peripheral"`, the only
    /// role the generator makes. Left out, it is `"peripheral"`. Only the generator reads it.
    #[serde(default)]
    pub role: Option<String>,
    /// The options the map gives the controller, each adding nodes the port needs (such as
    /// `"usb3"` for a USB 3 PHY the port's SuperSpeed lanes use). Only the generator reads it.
    #[serde(default)]
    pub options: Vec<String>,
    /// For a device port: the GPIO line that switches the port's VBUS, as `gpio:LABEL:LINE`. The
    /// generator holds it at `vbus_off` from power-on. Needs `vbus_reason`; exclusive with
    /// `vbus_undriven`. Only the device tree generator reads it.
    #[serde(default)]
    pub vbus: Option<String>,
    /// The level the `vbus` line holds when VBUS is off: `"low"` or `"high"`. Only the generator reads it.
    #[serde(default)]
    pub vbus_off: Option<String>,
    /// For a device port whose VBUS is not driven on this board at all: the board author's assertion,
    /// made from the schematic and checked with a meter. Exclusive with `vbus`. Needs `vbus_reason`.
    #[serde(default)]
    pub vbus_undriven: bool,
    /// Why the port's VBUS is handled as `vbus` or `vbus_undriven` says. Written beside the boot
    /// hold, or in the generator's refusal. Only the device tree generator reads it.
    #[serde(default)]
    pub vbus_reason: Option<String>,
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
    /// The name a person reads for the device, shown in place of its id. Optional: the id is
    /// the name when it is left out.
    #[serde(default)]
    pub label: Option<String>,
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
    /// Makes this device the daemon's keep-alive, such as a hardware watchdog. Its ticks run only
    /// while the host's [`Liveness`](crate::Liveness) says the daemon is healthy, so the device
    /// stops being fed when the daemon is not. Left out, it is an ordinary device.
    #[serde(default)]
    pub keepalive: bool,
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
            label: None,
            driver: self.driver.clone(),
            bus,
            uses,
            link: None,
            config: self.config.clone(),
            disabled: false,
            owner: self.owner,
            streams: self.streams.clone(),
            auto_off_after: self.auto_off_after,
            keepalive: false,
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
            .filter(|(_, value)| !matches!(value, toml::Value::Array(_)))
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

    /// The config's arrays, which must all hold strings (`health = ["device:x"]`). A driver built
    /// into the host reads them with [`lemnos_core::Config::list`]; [`owned_config`] refuses any
    /// other array.
    ///
    /// [`owned_config`]: DeviceSpec::owned_config
    pub(crate) fn owned_lists(&self) -> Result<Vec<(String, Vec<String>)>, BoardError> {
        self.config
            .iter()
            .filter_map(|(key, value)| match value {
                toml::Value::Array(items) => Some(
                    items
                        .iter()
                        .map(|item| match item {
                            toml::Value::String(text) => Ok(text.clone()),
                            _ => Err(BoardError::Config(self.id.clone(), key.clone())),
                        })
                        .collect::<Result<Vec<String>, BoardError>>()
                        .map(|list| (key.clone(), list)),
                ),
                _ => None,
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
        device.owned_lists()?;
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
        device.owned_lists()?;
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

    /// The reader rules, class by class: who writes a stream decides whether its buffers are read-only
    /// and whether readers may have them.
    #[test]
    fn who_writes_a_dma_buf_stream_decides_whether_readers_may_have_it() {
        // Hardware fills it, and nothing maps it writable: read-only, with readers.
        let hardware =
            stream("memory = \"dma-buf\"\nreaders = 2\n").expect("hardware with readers");
        assert!(hardware.read_only_buffers() && hardware.readers() == 2);
        // Hardware fills it and nobody reads it: the buffers stay writable, as they always were.
        let plain = stream("memory = \"dma-buf\"\n").expect("the default");
        assert!(
            !plain.read_only_buffers() && plain.readers() == 0,
            "no readers by default for dma-bufs"
        );
        // The consumer fills it: writable, and no readers.
        let consumer =
            stream("memory = \"dma-buf\"\nwriter = \"consumer\"\n").expect("consumer, no readers");
        assert!(!consumer.read_only_buffers() && consumer.readers() == 0);
        // The consumer fills it and readers are asked for: refused, and says why.
        let refused = stream("memory = \"dma-buf\"\nwriter = \"consumer\"\nreaders = 2\n");
        assert!(
            matches!(&refused, Err(BoardError::StreamWriter(_, _, why)) if why.contains("mapped writable")),
            "{refused:?}"
        );
        // A writer on a stream of shared memory is refused: it is always writable there.
        let shared = stream("writer = \"consumer\"\n");
        assert!(
            matches!(&shared, Err(BoardError::StreamWriter(..))),
            "{shared:?}"
        );
        // Shared memory keeps its default of four readers.
        assert_eq!(stream("").expect("shared").readers(), 4);
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
            Err(BoardError::StreamRingHandles(
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
    fn the_dma_heaps_are_the_host_section_or_the_cma_heap_plus_every_dma_buf_stream() {
        // Left out: the CMA heap the daemon's drivers allocate from.
        let board = host("retry_first_ms = 100\n").unwrap();
        assert_eq!(board.dma_heaps(), vec![DEFAULT_HOST_HEAP.to_string()]);
        // Named: exactly those, and nothing else.
        let board = host("dma_heaps = [\"system\"]\n").unwrap();
        assert_eq!(board.dma_heaps(), vec!["system".to_string()]);
        let board = host("dma_heaps = []\n").unwrap();
        assert!(board.dma_heaps().is_empty());
        // A dma-buf stream's heap is added, once; a shared stream adds none.
        let text = format!(
            "{HEAD}[host]\ndma_heaps = [\"linux,cma\"]\n\
             [[devices]]\nid = \"cam\"\ndriver = \"x\"\n\
             [devices.streams.a]\nbuffers = 2\nbuffer_bytes = 64\nmemory = \"dma-buf\"\nheap = \"linux,cma\"\n\
             [devices.streams.b]\nbuffers = 2\nbuffer_bytes = 64\nmemory = \"dma-buf\"\n\
             [devices.streams.c]\nbuffers = 2\nbuffer_bytes = 64\n"
        );
        let board = parse(&text).unwrap();
        assert_eq!(
            board.dma_heaps(),
            vec!["linux,cma".to_string(), "system".to_string()]
        );
    }

    #[test]
    fn a_dma_heap_the_host_section_names_is_one_file_name() {
        for bad in ["\"\"", "\"../x\"", "\"a/b\"", "\"a b\""] {
            let error = host(&format!("dma_heaps = [{bad}]\n")).unwrap_err();
            assert!(
                matches!(error, BoardError::Host("dma_heaps", _)),
                "{bad}: {error}"
            );
        }
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
