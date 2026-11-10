//! The board file: which devices a board has and how they are wired. A parser
//! only. It knows no drivers and no kinds of device.

use std::collections::{BTreeMap, BTreeSet};

use lemnos_core::ConfigValue;
use lemnos_wire::MAX_HANDLE_BUFFERS;
use lemnos_wire::ring::Layout;
use serde::Deserialize;

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

    /// Checks what [`Layout`] doesn't: the memory the stream asks for.
    fn check_memory(&self, device: &str, name: &str) -> Result<(), BoardError> {
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
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoardFile {
    pub format: String,
    pub schema_version: u32,
    pub board: BoardInfo,
    #[serde(default)]
    pub devices: Vec<DeviceSpec>,
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
    let mut seen = BTreeSet::new();
    for device in &board.devices {
        if !seen.insert(device.id.as_str()) {
            return Err(BoardError::Duplicate(device.id.clone()));
        }
        if device.uses.values().any(|used| *used == device.id) {
            return Err(BoardError::SelfUse(device.id.clone()));
        }
        device.owned_config()?;
        for (name, stream) in &device.streams {
            if stream.checked_layout().is_none() {
                return Err(BoardError::Stream(device.id.clone(), name.clone()));
            }
            stream.check_memory(&device.id, name)?;
        }
    }
    Ok(board)
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

    #[test]
    fn missing_required_keys_are_named() {
        let error = with("[[devices]]\nid = \"a\"\n").unwrap_err();
        assert!(error.to_string().contains("driver"), "{error}");
    }
}
