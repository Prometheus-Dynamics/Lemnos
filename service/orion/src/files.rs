//! Files sent through Orion to a device that takes them.
//!
//! The board names, in `LEMNOS_ORION_FILES`, each device that takes files and the directory they
//! go into (`slots=/data/slots/.incoming`). For each, this thread asks Orion to be the receiver
//! of every file sent to `lemnos/<device>/<name>`, so a file is written once, straight where the
//! device reads it, and never kept by the node too. Before a file is taken, the device's
//! `accept` method, when its description has one, is called with the file's size: a device that
//! has no room refuses it there. The file is written under a hidden name, checked against its
//! SHA-256 when the sender commits it, and only then renamed to `<name>`. A file that fails the
//! check, is refused, or stops arriving for [`IDLE`] (`Config::stall`) is deleted.
//!
//! Nothing here knows what a device does with the file: the board says which device takes
//! files, and the device's own method says whether it has room. A sender then runs the
//! device's action that uses the file, as it would any other.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use lemnos_client::Client;
use lemnos_wire::text::{encode_args, find_method};
use orion::files::{FileState, Handed, Sink};
use orion::{Options, Orion};
use sha2::{Digest, Sha256};

use crate::backoff::Backoff;
use crate::config::Config;
use crate::stream::Streamer;
use crate::{Shared, log};

/// The name this thread gives Orion.
pub const FILES_CLIENT_NAME: &str = "lemnos-orion-files";
/// The name this thread gives lemnosd.
pub const LEMNOSD_FILES_NAME: &str = "orion-files";
/// What every destination starts with: `lemnos/<device>/<name>`.
pub const DESTINATION_PREFIX: &str = "lemnos/";
/// A file that has had nothing for this long is given up and deleted.
pub const IDLE: Duration = Duration::from_secs(600);
/// How long one look at a receiver waits.
const LOOK: Duration = Duration::from_millis(50);

/// A file name a device may be sent: one path component of `[A-Za-z0-9._-]`, not starting with
/// a dot (those are this thread's own files), at most 128 bytes.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// How a transfer stands, as the sender is told.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    pub received: u64,
    pub state: FileState,
    pub detail: String,
}

impl Answer {
    fn receiving(received: u64) -> Self {
        Self {
            received,
            state: FileState::Receiving,
            detail: String::new(),
        }
    }

    fn rejected(detail: impl Into<String>) -> Self {
        Self {
            received: 0,
            state: FileState::Rejected,
            detail: detail.into(),
        }
    }
}

/// One file on its way in.
#[derive(Debug)]
struct Partial {
    name: String,
    path: PathBuf,
    file: File,
    size: u64,
    digest: [u8; 32],
    received: u64,
    hasher: Sha256,
    last: Instant,
}

/// The files coming into one directory, each written under a hidden name until it is whole.
#[derive(Debug)]
pub struct Receiver {
    dir: PathBuf,
    partials: HashMap<u64, Partial>,
}

impl Receiver {
    /// Deletes what an earlier run of this thread left half-written in `dir`.
    pub fn new(dir: &Path) -> Self {
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with(".orion-") && name.ends_with(".part") {
                    let _ = fs::remove_file(entry.path());
                }
            }
        }
        Self {
            dir: dir.to_path_buf(),
            partials: HashMap::new(),
        }
    }

    /// A file begins, or begins again. `accept` asks the device whether `size` bytes fit.
    pub fn begin(
        &mut self,
        transfer: u64,
        name: &str,
        size: u64,
        digest: [u8; 32],
        accept: &mut dyn FnMut(u64) -> Result<(), String>,
    ) -> Answer {
        if !valid_name(name) {
            return Answer::rejected(format!(
                "{name:?} isn't a file name a device takes: one part of [A-Za-z0-9._-], not starting with a dot"
            ));
        }
        if let Some(partial) = self.partials.get_mut(&transfer) {
            if partial.name == name && partial.size == size && partial.digest == digest {
                partial.last = Instant::now();
                return Answer::receiving(partial.received);
            }
            self.drop_partial(transfer);
        }
        if let Err(why) = accept(size) {
            return Answer::rejected(why);
        }
        let path = self.dir.join(format!(".orion-{transfer:016x}.part"));
        let file = match File::create(&path) {
            Ok(file) => file,
            Err(error) => {
                return Answer::rejected(format!(
                    "cannot write into {}: {error}",
                    self.dir.display()
                ));
            }
        };
        self.partials.insert(
            transfer,
            Partial {
                name: name.to_owned(),
                path,
                file,
                size,
                digest,
                received: 0,
                hasher: Sha256::new(),
                last: Instant::now(),
            },
        );
        Answer::receiving(0)
    }

    /// Some of a file's bytes. Bytes that aren't the next ones are left out; the answer says
    /// where to go on from.
    pub fn chunk(&mut self, transfer: u64, offset: u64, data: &[u8]) -> Answer {
        let Some(partial) = self.partials.get_mut(&transfer) else {
            return Answer::rejected("no such transfer here: begin it again");
        };
        partial.last = Instant::now();
        if offset != partial.received {
            return Answer::receiving(partial.received);
        }
        let len = data.len() as u64;
        if partial.received.saturating_add(len) > partial.size {
            self.drop_partial(transfer);
            return Answer::rejected("more bytes than the file's size");
        }
        if let Err(error) = partial.file.write_all(data) {
            self.drop_partial(transfer);
            return Answer::rejected(format!("cannot write the file: {error}"));
        }
        partial.hasher.update(data);
        partial.received += len;
        Answer::receiving(partial.received)
    }

    /// Every byte was sent: checked, it is put in place under its name; otherwise deleted.
    pub fn commit(&mut self, transfer: u64) -> Answer {
        let Some(partial) = self.partials.remove(&transfer) else {
            return Answer::rejected("no such transfer here: begin it again");
        };
        let fail = |partial: &Partial, why: String| {
            let _ = fs::remove_file(&partial.path);
            Answer::rejected(why)
        };
        if partial.received != partial.size {
            let why = format!("{} of its {} bytes came", partial.received, partial.size);
            return fail(&partial, why);
        }
        let got: [u8; 32] = partial.hasher.clone().finalize().into();
        if got != partial.digest {
            return fail(
                &partial,
                "its SHA-256 isn't the one it was sent with".to_owned(),
            );
        }
        let target = self.dir.join(&partial.name);
        let placed = partial
            .file
            .sync_all()
            .and_then(|()| fs::rename(&partial.path, &target))
            .and_then(|()| File::open(&self.dir)?.sync_all());
        if let Err(error) = placed {
            return fail(&partial, format!("cannot put it in place: {error}"));
        }
        Answer {
            received: partial.size,
            state: FileState::Committed,
            detail: String::new(),
        }
    }

    /// Deletes the files that have had nothing for `idle`.
    pub fn expire(&mut self, idle: Duration) {
        let old: Vec<u64> = self
            .partials
            .iter()
            .filter(|(_, p)| p.last.elapsed() >= idle)
            .map(|(t, _)| *t)
            .collect();
        for transfer in old {
            self.drop_partial(transfer);
        }
    }

    /// Files on their way in.
    pub fn open(&self) -> usize {
        self.partials.len()
    }

    fn drop_partial(&mut self, transfer: u64) {
        if let Some(partial) = self.partials.remove(&transfer) {
            let _ = fs::remove_file(&partial.path);
        }
    }
}

/// Asks device `device` whether `size` bytes fit, through its `accept` method if it has one.
///
/// # Errors
///
/// Why not, as words for the sender.
pub fn ask_accept(client: &Client, device: &str, size: u64) -> Result<(), String> {
    let entry = client
        .list()
        .map_err(|e| format!("lemnosd: {e}"))?
        .into_iter()
        .find(|d| d.name == device)
        .ok_or_else(|| format!("lemnosd has no device {device:?}"))?;
    let description = client
        .describe(entry.index)
        .map_err(|e| format!("lemnosd: {e}"))?;
    let Ok((contract, method)) = find_method(&description, "accept") else {
        return Ok(());
    };
    let args = encode_args(&method.params, &[size.to_string()])?;
    let tag = &description.contracts[contract].tag;
    let mut handle = client
        .open_tag(device, tag)
        .map_err(|e| format!("lemnosd: {e}"))?;
    let mut reply = [0u8; 64];
    client
        .call(&mut handle, method.id, &args, &mut reply)
        .map(|_| ())
        .map_err(|e| format!("{device} doesn't take {size} bytes now: {e}"))
}

/// One device that takes files, as this thread serves it.
struct Taker {
    device: String,
    prefix: String,
    sink: Sink,
    receiver: Receiver,
    streamer: Streamer,
}

/// Receives files for the devices `config.files` names, until the service stops.
pub fn run(config: &Config, shared: &Shared) {
    if config.files.is_empty() {
        return;
    }
    let mut backoff = Backoff::new();
    let mut lemnosd: Option<Client> = None;
    while !shared.stopping() {
        let orion = match Orion::connect_with(&config.orion, FILES_CLIENT_NAME, Options::default())
        {
            Ok(orion) => orion,
            Err(error) => {
                log(&format!(
                    "files: Orion not available at {}: {error}",
                    config.orion
                ));
                std::thread::sleep(backoff.next_delay());
                continue;
            }
        };
        let mut takers = Vec::new();
        for (device, dir) in &config.files {
            let prefix = format!("{DESTINATION_PREFIX}{device}/");
            match orion.files().sink(&prefix) {
                Ok(sink) => {
                    let names = config
                        .streams
                        .iter()
                        .find(|(d, _)| d == device)
                        .map(|(_, names)| names.clone())
                        .unwrap_or_default();
                    takers.push(Taker {
                        device: device.clone(),
                        prefix,
                        sink,
                        receiver: Receiver::new(dir),
                        streamer: Streamer::new(device, dir, names, &config.lemnosd_socket),
                    });
                }
                Err(error) => log(&format!("files: Orion refused {prefix}: {error}")),
            }
        }
        if takers.is_empty() {
            std::thread::sleep(backoff.next_delay());
            continue;
        }
        backoff.reset();
        let lost = serve(config, shared, &mut takers, &mut lemnosd);
        if let Err(error) = lost {
            log(&format!("files: Orion lost: {error}"));
        }
    }
}

fn serve(
    config: &Config,
    shared: &Shared,
    takers: &mut [Taker],
    lemnosd: &mut Option<Client>,
) -> Result<(), orion::ClientError> {
    let look = LOOK / u32::try_from(takers.len()).unwrap_or(1).max(1);
    while !shared.stopping() {
        for taker in takers.iter_mut() {
            taker.receiver.expire(config.stall);
            taker.streamer.expire(config.stall);
            let Some(handed) = taker.sink.next(look)? else {
                continue;
            };
            let (transfer, answer) = match handed {
                Handed::Begin {
                    transfer,
                    destination,
                    size,
                    digest,
                } if taker
                    .streamer
                    .takes(destination.strip_prefix(&taker.prefix).unwrap_or("")) =>
                {
                    // A stream: its bytes go to the device's job as they arrive.
                    let name = destination.strip_prefix(&taker.prefix).unwrap_or("");
                    (transfer, taker.streamer.begin(transfer, name, size, digest))
                }
                Handed::Begin {
                    transfer,
                    destination,
                    size,
                    digest,
                } => {
                    let name = destination.strip_prefix(&taker.prefix).unwrap_or("");
                    let device = taker.device.clone();
                    let mut accept = |size: u64| {
                        if !lemnosd.as_ref().is_some_and(Client::is_connected) {
                            *lemnosd =
                                Client::connect(&config.lemnosd_socket, LEMNOSD_FILES_NAME).ok();
                        }
                        let client = lemnosd.as_ref().ok_or("lemnosd is not there")?;
                        ask_accept(client, &device, size)
                    };
                    (
                        transfer,
                        taker
                            .receiver
                            .begin(transfer, name, size, digest, &mut accept),
                    )
                }
                Handed::Chunk {
                    transfer,
                    offset,
                    data,
                } if taker.streamer.owns(transfer) => (
                    transfer,
                    accepted(shared, taker.streamer.chunk(transfer, offset, &data)),
                ),
                Handed::Chunk {
                    transfer,
                    offset,
                    data,
                } => (
                    transfer,
                    accepted(shared, taker.receiver.chunk(transfer, offset, &data)),
                ),
                Handed::Commit { transfer } if taker.streamer.owns(transfer) => {
                    (transfer, taker.streamer.commit(transfer))
                }
                Handed::Commit { transfer } => (transfer, taker.receiver.commit(transfer)),
            };
            taker
                .sink
                .status(transfer, answer.received, answer.state, &answer.detail)?;
        }
    }
    Ok(())
}

/// A chunk the bridge answers as still receiving renews the caller leases: a transfer making progress
/// is a caller alive (see [`crate::calls`]). Pinned Orion does not say which session sent a chunk, so
/// every caller's lease is renewed, not only the sender's; a dead caller's holds survive while any
/// transfer is being accepted.
// ORION-R2 (C33): the files events gain the sending session, and only that caller's lease is renewed.
fn accepted(shared: &Shared, answer: Answer) -> Answer {
    if answer.state == FileState::Receiving {
        shared.transferred();
    }
    answer
}

/// The SHA-256 of a file, for tests and tools.
///
/// # Errors
///
/// When it can't be read.
pub fn sha256_of(path: &Path) -> io::Result<[u8; 32]> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            return Ok(hasher.finalize().into());
        }
        hasher.update(&buf[..n]);
    }
}

#[cfg(test)]
mod tests;
