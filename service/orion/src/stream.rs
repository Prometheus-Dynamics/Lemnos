//! Streamed transfers: a file pushed to a device that takes it as a stream is fed to the device's
//! long job as its bytes arrive, instead of being written to disk first.
//!
//! The board names the streams a device takes, in `LEMNOS_ORION_STREAMS`, as file names:
//! `flash=image.img.xz+image.img`. A transfer with one of those names is a stream, and its
//! transport manifest is the same name with `.json` after it. The manifest is pushed first to the
//! device's directory like any file (it is small). Its optional `signature` field is the 108-byte
//! signature record, hex; this module decodes it and passes it with the manifest to the device's
//! `manifest` method. When the stream begins, this module calls `manifest`, then starts the
//! `write` job with the stream's size and format (`xz` for a name ending `.xz`, raw otherwise), and
//! then gives each chunk to its `data` method in order. There is no sidecar file: a signed image
//! carries its record as the second xz stream of the file itself, and the device checks the two
//! agree.
//!
//! This module is the job's starter: it runs on its own lemnosd connection, because only the
//! starter may send a job's data. Each chunk is acknowledged to the sender only after the device
//! took it, so a slow device stops the sender (Orion keeps only a few chunks in flight). Memory
//! is one chunk. The stream's SHA-256 is checked against the transfer's digest at its commit; the
//! job's own result (the device checks the decoded image) is the answer the sender gets.
//!
//! The protocol the device's methods take is the one its contract documents (`system.flash/1`:
//! `manifest(bytes, bytes)`, `write(u64, u8)` a long job, `data(u64, bytes)`); the arguments are
//! encoded here as Lemnos's wire encodes them: little-endian, a `u32` length before each byte run.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use lemnos_client::{Client, Handle};
use lemnos_slot_store::json::Json;
use lemnos_wire::text::find_method;
use orion::files::FileState;
use sha2::{Digest, Sha256};

use crate::files::Answer;

/// The signature record's length: the first-flash contract's 108 bytes.
const RECORD_LEN: usize = 108;

/// How long the commit waits for the device's job to end. A device writes at a few MB/s, and its
/// job ends with its last chunk, so this is a backstop, not a pace.
const JOB_WAIT: Duration = Duration::from_secs(900);
/// The contract tag a stream's device serves.
const TAG: &str = "system.flash/1";

/// The streams a device takes, and where their manifests are. It holds its own connection to
/// lemnosd, which is the job's starter.
pub struct Streamer {
    device: String,
    dir: PathBuf,
    names: Vec<String>,
    socket: PathBuf,
    client: Option<Client>,
    /// The stream being received, if one is.
    active: Option<Active>,
}

/// A streamer that goes (the Orion session its transfers came on was lost, and the files thread
/// builds a new one for the next) ends the stream it was receiving: the job is cancelled on its
/// connection, so the device is free and the next stream can start. The same path as the stall rule
/// ([`Streamer::drop_active`]), except that a connection to lemnosd that is already gone is not made
/// again: the job went with it.
impl Drop for Streamer {
    fn drop(&mut self) {
        if let Some(active) = self.active.take()
            && let Some(client) = self.client.as_ref().filter(|client| client.is_connected())
        {
            let _ = client.cancel_job(active.job);
        }
    }
}

/// A stream being received: its transfer, the job it feeds, and the bytes so far.
struct Active {
    transfer: u64,
    size: u64,
    got: u64,
    digest: [u8; 32],
    hasher: Sha256,
    job: u64,
    handle: Handle,
    data: u16,
    /// When the sender last sent a chunk, or the stream began.
    seen: Instant,
}

impl std::fmt::Debug for Active {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Active")
            .field("transfer", &self.transfer)
            .field("got", &self.got)
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

impl Streamer {
    /// A streamer for `device`, whose files go into `dir`, taking the files named in `names`, and
    /// reaching lemnosd at `socket`.
    pub fn new(device: &str, dir: &Path, names: Vec<String>, socket: &Path) -> Self {
        Self {
            device: device.to_owned(),
            dir: dir.to_path_buf(),
            names,
            socket: socket.to_path_buf(),
            client: None,
            active: None,
        }
    }

    /// Whether a transfer named `name` is a stream of this device.
    pub fn takes(&self, name: &str) -> bool {
        self.names.iter().any(|n| n == name)
    }

    /// Whether `transfer` is the stream being received.
    pub fn owns(&self, transfer: u64) -> bool {
        self.active.as_ref().is_some_and(|a| a.transfer == transfer)
    }

    /// The connection to lemnosd, made when there is none.
    fn client(&mut self) -> Result<&Client, String> {
        if !self.client.as_ref().is_some_and(Client::is_connected) {
            self.client = Some(
                Client::connect(&self.socket, STREAMS_CLIENT_NAME)
                    .map_err(|e| format!("lemnosd is not there: {e}"))?,
            );
        }
        self.client
            .as_ref()
            .ok_or_else(|| "lemnosd is not there".to_string())
    }

    /// Cancels the job of the stream being received, if one is, and forgets the stream.
    fn drop_active(&mut self) {
        if let Some(active) = self.active.take()
            && let Ok(client) = self.client()
        {
            let _ = client.cancel_job(active.job);
        }
    }

    /// Drops the stream being received when no chunk has come for `idle`: the job it feeds would
    /// otherwise hold the device for ever. The receiver expires the file at the same time, so the
    /// sender could not send on either way. Nothing says a sender went; a quiet one is dropped the same.
    // Not at Orion R2: the file sink's events (`Handed`) do not name the session that
    // sent a chunk, so the stream cannot be ended at its sender's `CallerGone`. The bound is the one
    // `idle` gives, for a sender that is alive and silent or gone.
    pub fn expire(&mut self, idle: Duration) {
        if self
            .active
            .as_ref()
            .is_some_and(|a| a.seen.elapsed() >= idle)
        {
            self.drop_active();
        }
    }

    /// A stream begins: its manifest goes to the device and its job starts. A stream begun while
    /// another is received ends the other, and its job with it.
    pub fn begin(&mut self, transfer: u64, name: &str, size: u64, digest: [u8; 32]) -> Answer {
        self.drop_active();
        match self.start(transfer, name, size, digest) {
            Ok(active) => {
                self.active = Some(active);
                Answer {
                    received: 0,
                    state: FileState::Receiving,
                    detail: String::new(),
                }
            }
            Err(detail) => rejected(detail),
        }
    }

    fn start(
        &mut self,
        transfer: u64,
        name: &str,
        size: u64,
        digest: [u8; 32],
    ) -> Result<Active, String> {
        let manifest = fs::read(self.dir.join(format!("{name}.json")))
            .map_err(|_| format!("no manifest: send {name}.json first"))?;
        let text =
            std::str::from_utf8(&manifest).map_err(|_| "the manifest is not text".to_string())?;
        let record = json_record(text)?;
        let format: u8 = if name.ends_with(".xz") { 1 } else { 0 };
        let device = self.device.clone();
        let client = self.client()?;
        let description = client
            .list()
            .map_err(|e| format!("lemnosd: {e}"))?
            .into_iter()
            .find(|d| d.name == device)
            .ok_or_else(|| format!("lemnosd has no device {device:?}"))
            .and_then(|entry| {
                client
                    .describe(entry.index)
                    .map_err(|e| format!("lemnosd: {e}"))
            })?;
        let (contract, manifest_method) = find_method(&description, "manifest")?;
        if description.contracts[contract].tag != TAG {
            return Err(format!("device {device:?} does not take a stream"));
        }
        let (_, write) = find_method(&description, "write")?;
        let (_, data) = find_method(&description, "data")?;
        let (manifest_id, write_id, data_id) = (manifest_method.id, write.id, data.id);
        let mut handle = client
            .open_tag(&device, TAG)
            .map_err(|e| format!("lemnosd: {e}"))?;
        let mut args = Vec::new();
        put_bytes(&mut args, &manifest);
        put_bytes(&mut args, &record);
        client
            .call(&mut handle, manifest_id, &args, &mut [0u8; 0])
            .map_err(|e| format!("the device refused the manifest: {e}"))?;
        let mut args = Vec::with_capacity(9);
        args.extend_from_slice(&size.to_le_bytes());
        args.push(format);
        let job = client
            .start_job(&mut handle, write_id, &args)
            .map_err(|e| format!("the device did not start the write: {e}"))?;
        Ok(Active {
            transfer,
            size,
            got: 0,
            digest,
            hasher: Sha256::new(),
            job,
            handle,
            data: data_id,
            seen: Instant::now(),
        })
    }

    /// The next chunk of the stream. The device takes it before the sender is told it arrived, so
    /// a slow device slows the sender.
    pub fn chunk(&mut self, transfer: u64, offset: u64, bytes: &[u8]) -> Answer {
        let Some(got) = self
            .active
            .as_ref()
            .filter(|a| a.transfer == transfer)
            .map(|a| a.got)
        else {
            return rejected("no stream is being received".into());
        };
        if offset != got {
            self.drop_active();
            return rejected("a chunk came out of order".into());
        }
        let mut args = Vec::with_capacity(12 + bytes.len());
        args.extend_from_slice(&offset.to_le_bytes());
        put_bytes(&mut args, bytes);
        let Some(active) = self.active.as_mut() else {
            return rejected("no stream is being received".into());
        };
        let data = active.data;
        let sent = match self.client.as_ref().filter(|c| c.is_connected()) {
            Some(client) => client
                .call(&mut active.handle, data, &args, &mut [0u8; 0])
                .map_err(|e| format!("the device refused the data: {e}")),
            None => Err("lemnosd went away during the stream".to_string()),
        };
        if let Err(detail) = sent {
            self.drop_active();
            return rejected(detail);
        }
        let Some(active) = self.active.as_mut() else {
            return rejected("no stream is being received".into());
        };
        active.hasher.update(bytes);
        active.got += bytes.len() as u64;
        active.seen = Instant::now();
        Answer {
            received: active.got,
            state: FileState::Receiving,
            detail: String::new(),
        }
    }

    /// The stream is all sent: its size and digest are checked, and the job's result is the answer.
    pub fn commit(&mut self, transfer: u64) -> Answer {
        if !self.owns(transfer) {
            return rejected("no stream is being received".into());
        }
        let Some(active) = self.active.take() else {
            return rejected("no stream is being received".into());
        };
        let job = active.job;
        if active.got != active.size {
            self.active = Some(active);
            self.drop_active();
            return rejected("the stream is shorter than the size it was sent with".into());
        }
        let digest: [u8; 32] = active.hasher.clone().finalize().into();
        if digest != active.digest {
            if let Ok(client) = self.client() {
                let _ = client.cancel_job(job);
            }
            return rejected("the stream's SHA-256 is not the sender's".into());
        }
        let Ok(client) = self.client() else {
            return rejected("lemnosd went away before the write ended".into());
        };
        match client.wait_job(job, JOB_WAIT, &mut |_, _| {}) {
            Some((status, result)) if status.0 == 0 => {
                let raw = result
                    .get(..8)
                    .and_then(|b| <[u8; 8]>::try_from(b).ok())
                    .map_or(0, u64::from_le_bytes);
                // The outcome code after the byte count: how the signature stood (see `manifest`).
                let signed = match result.get(8) {
                    Some(0) => "verified",
                    Some(1) => "not checked: unsigned",
                    Some(2) => "not checked: signed by an unknown key",
                    _ => "not reported",
                };
                Answer {
                    received: active.got,
                    state: FileState::Committed,
                    detail: format!("{raw} bytes written; signature {signed}"),
                }
            }
            Some((status, _)) => rejected(format!("the write ended with status {}", status.0)),
            None => rejected("the write did not end".into()),
        }
    }
}

/// The name this streamer gives lemnosd.
pub const STREAMS_CLIENT_NAME: &str = "lemnos-orion-streams";

/// The answer for a refused transfer.
fn rejected(detail: String) -> Answer {
    Answer {
        received: 0,
        state: FileState::Rejected,
        detail,
    }
}

/// The signature record the transport JSON's `signature` field holds, decoded: empty when the
/// field is absent. The record is 108 bytes, 216 lowercase hex digits in the field.
fn json_record(text: &str) -> Result<Vec<u8>, String> {
    let json = Json::parse(text).ok_or("the manifest is not JSON")?;
    let Some(hex) = json.get("signature").and_then(Json::text) else {
        return Ok(Vec::new());
    };
    let bad = || "the manifest's signature is not 216 lowercase hex digits".to_string();
    if hex.len() != RECORD_LEN * 2
        || !hex
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(bad());
    }
    hex.as_bytes()
        .chunks(2)
        .map(|pair| {
            u8::from_str_radix(std::str::from_utf8(pair).map_err(|_| bad())?, 16).map_err(|_| bad())
        })
        .collect()
}

/// A byte run as the wire takes it: a `u32` length, then the bytes.
fn put_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(bytes);
}
