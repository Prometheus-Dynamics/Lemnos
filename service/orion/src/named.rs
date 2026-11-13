//! The node's named files (`lemnos/`, `docs/daemon.md`, "Named files") as Orion serves them.
//!
//! A file Orion sends is a [`Snapshot`]: a `Read + Seek` over the bytes the node held when the
//! transfer was opened, read from the node in pieces and closed when the snapshot is dropped. A
//! transfer that resumes seeks to its offset; the bytes do not change under it.
//!
//! The node is reached through [`Node`]. [`Lemnosd`] is the daemon over its client, to the device
//! named by `LEMNOS_ORION_NAMED_FILES`; tests give a fake.
//!
//! ## The offer (not yet switched on)
//!
//! Orion serves a prefix through its offer API: `Files::offer(prefix)` gives an offer, whose
//! `next()` gives each `Asked { name, from }`, answered by `send(source)` or `refuse(why)`. That API
//! is in Orion's rev R2/R3, not at the pinned rev 455a956e, so the glue is not compiled yet. When it
//! is, the glue is this loop, and only this loop is new:
//!
//! ```text
//! // ORION-OFFER: serve `lemnos/` through Orion's offer.
//! let offer = files.offer(PREFIX);
//! while let Some(asked) = offer.next() {
//!     match Snapshot::open(Box::new(Lemnosd::connect(socket, device)?), &asked.name)
//!         .and_then(|s| s.at(asked.from))
//!     {
//!         Ok(snapshot) => asked.send(snapshot),
//!         // The reason is kept in the bridge's state and log; a refusal is never retried silently.
//!         Err(why) => asked.refuse(why),
//!     }
//! }
//! ```
//!
//! Switching it on is that change at the rev move, plus the wiring in `main`. Until then nothing
//! here runs: the bridge serves no `lemnos/` names over Orion.

use std::io::{self, Read, Seek, SeekFrom};

use lemnos_client::Client;

/// The prefix the node's names carry.
pub const PREFIX: &str = "lemnos/";
/// The environment variable naming the node's named-files device.
pub const DEVICE_ENV: &str = "LEMNOS_ORION_NAMED_FILES";

const TAG: &str = "lemnos.files/1";
const OPEN: u16 = 2;
const SIZE: u16 = 3;
const READ: u16 = 4;
const CLOSE: u16 = 5;
/// One read asks for at most this much, well inside a frame.
const PIECE: usize = 32 << 10;

/// The calls a snapshot needs of the node.
pub trait Node {
    /// Takes a snapshot of `name`; the handle names it in the calls below.
    fn open(&mut self, name: &str) -> Result<u64, String>;
    /// The size of snapshot `handle`.
    fn size(&mut self, handle: u64) -> Result<u64, String>;
    /// Fills `into` from `from` in snapshot `handle`: at most `into.len()` bytes, zero at the end.
    fn read(&mut self, handle: u64, from: u64, into: &mut [u8]) -> Result<usize, String>;
    /// Frees snapshot `handle`.
    fn close(&mut self, handle: u64) -> Result<(), String>;
}

/// The daemon, through its client, and the named-files device it serves the names from.
pub struct Lemnosd {
    pub client: Client,
    pub device: String,
}

impl Lemnosd {
    /// The device `LEMNOS_ORION_NAMED_FILES` names, `None` when it is not set.
    pub fn device_from_env() -> Option<String> {
        std::env::var(DEVICE_ENV)
            .ok()
            .filter(|name| !name.is_empty())
    }
}

impl Node for Lemnosd {
    fn open(&mut self, name: &str) -> Result<u64, String> {
        let mut args = Vec::with_capacity(4 + name.len());
        args.extend_from_slice(&(name.len() as u32).to_le_bytes());
        args.extend_from_slice(name.as_bytes());
        let mut handle = self
            .client
            .open_tag(&self.device, TAG)
            .map_err(|e| format!("lemnosd: {e}"))?;
        let mut reply = [0u8; 8];
        let n = self
            .client
            .call(&mut handle, OPEN, &args, &mut reply)
            .map_err(|e| format!("{name}: {e}"))?;
        u64_of(&reply[..n])
    }

    fn size(&mut self, handle: u64) -> Result<u64, String> {
        let mut h = self
            .client
            .open_tag(&self.device, TAG)
            .map_err(|e| format!("lemnosd: {e}"))?;
        let mut reply = [0u8; 8];
        let n = self
            .client
            .call(&mut h, SIZE, &handle.to_le_bytes(), &mut reply)
            .map_err(|e| format!("lemnosd: {e}"))?;
        u64_of(&reply[..n])
    }

    fn read(&mut self, handle: u64, from: u64, into: &mut [u8]) -> Result<usize, String> {
        // handle, from, then the out buffer's capacity (the last argument of an out-buffer call).
        let mut args = [0u8; 20];
        args[..8].copy_from_slice(&handle.to_le_bytes());
        args[8..16].copy_from_slice(&from.to_le_bytes());
        let capacity = into.len().min(PIECE) as u32;
        args[16..].copy_from_slice(&capacity.to_le_bytes());
        let mut h = self
            .client
            .open_tag(&self.device, TAG)
            .map_err(|e| format!("lemnosd: {e}"))?;
        self.client
            .call(&mut h, READ, &args, &mut into[..capacity as usize])
            .map_err(|e| format!("lemnosd: {e}"))
    }

    fn close(&mut self, handle: u64) -> Result<(), String> {
        let mut h = self
            .client
            .open_tag(&self.device, TAG)
            .map_err(|e| format!("lemnosd: {e}"))?;
        self.client
            .call(&mut h, CLOSE, &handle.to_le_bytes(), &mut [])
            .map(|_| ())
            .map_err(|e| format!("lemnosd: {e}"))
    }
}

fn u64_of(bytes: &[u8]) -> Result<u64, String> {
    let array: [u8; 8] = bytes
        .try_into()
        .map_err(|_| "lemnosd sent a reply of the wrong size".to_string())?;
    Ok(u64::from_le_bytes(array))
}

/// One named file, as the bytes the node held when it was opened.
pub struct Snapshot {
    node: Box<dyn Node>,
    handle: u64,
    size: u64,
    pos: u64,
}

impl Snapshot {
    /// Opens `name` on `node`. A name without the `lemnos/` prefix, or one the node refuses, is an
    /// error with the reason as text.
    pub fn open(mut node: Box<dyn Node>, name: &str) -> Result<Self, String> {
        if !name.starts_with(PREFIX) {
            return Err(format!(
                "{name:?} is not a `{PREFIX}` name this bridge serves"
            ));
        }
        let handle = node.open(name)?;
        let size = match node.size(handle) {
            Ok(size) => size,
            Err(why) => {
                let _ = node.close(handle);
                return Err(why);
            }
        };
        Ok(Self {
            node,
            handle,
            size,
            pos: 0,
        })
    }

    /// The file's size, fixed when it was opened.
    pub fn size(&self) -> u64 {
        self.size
    }

    /// The same snapshot, from `offset`: where a transfer resumes. Past the end is the end.
    pub fn at(mut self, offset: u64) -> io::Result<Self> {
        self.seek(SeekFrom::Start(offset))?;
        Ok(self)
    }
}

impl Read for Snapshot {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.size || buf.is_empty() {
            return Ok(0);
        }
        let want = buf.len().min(PIECE).min((self.size - self.pos) as usize);
        let n = self
            .node
            .read(self.handle, self.pos, &mut buf[..want])
            .map_err(io::Error::other)?;
        if n == 0 {
            // The snapshot says it has bytes here; the node giving none is a failure, not an end.
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!("the node stopped at {} of {} bytes", self.pos, self.size),
            ));
        }
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for Snapshot {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let target = match from {
            SeekFrom::Start(offset) => Some(offset),
            SeekFrom::Current(delta) => self.pos.checked_add_signed(delta),
            SeekFrom::End(delta) => self.size.checked_add_signed(delta),
        }
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
        self.pos = target;
        Ok(target)
    }
}

impl Drop for Snapshot {
    fn drop(&mut self) {
        let _ = self.node.close(self.handle);
    }
}
