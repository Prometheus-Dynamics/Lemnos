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
use crate::stream::StreamWorker;
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

    /// The transfer ran out of time: nothing came for `idle`. Orion's file states have no timed-out
    /// state, so this is a rejection whose words say so.
    pub(crate) fn timed_out(received: u64, idle: Duration) -> Self {
        Self {
            received,
            state: FileState::Rejected,
            detail: format!(
                "ran out of time: nothing came for {} seconds, so the file was deleted",
                idle.as_secs()
            ),
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

    /// Deletes the files that have had nothing for `idle`, and says for each one how it ended: it ran
    /// out of time, with the bytes it had. The sender is told, so it does not wait on silence.
    pub fn expire(&mut self, idle: Duration) -> Vec<(u64, Answer)> {
        let old: Vec<(u64, u64)> = self
            .partials
            .iter()
            .filter(|(_, p)| p.last.elapsed() >= idle)
            .map(|(t, p)| (*t, p.received))
            .collect();
        let mut ended = Vec::with_capacity(old.len());
        for (transfer, received) in old {
            self.drop_partial(transfer);
            ended.push((transfer, Answer::timed_out(received, idle)));
        }
        ended
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
    /// The session the sink is on, its own.
    _orion: Orion,
    receiver: Receiver,
    /// The device's streams: its index in the workers [`run`] keeps across Orion sessions.
    streamer: usize,
}

/// A device that takes files on a board that gave the bridge no place for them: every push to it
/// is refused at its start, with why (see [`crate::pushes`]).
struct Refuser {
    sink: Sink,
    /// The session the sink is on, its own.
    _orion: Orion,
    why: String,
}

/// Receives files for the devices `config.files` names, and refuses them, with why, for each
/// device that takes files and that it does not name, until the service stops.
pub fn run(config: &Config, shared: &Shared) {
    let Some(unplaced) = find_unplaced(config, shared) else {
        return;
    };
    if config.files.is_empty() && unplaced.is_empty() {
        return;
    }
    let mut backoff = Backoff::new();
    let mut lemnosd: Option<Client> = None;
    // One stream worker per device, for as long as the service runs: a stream and its job outlive a
    // broken Orion session, so the sender's resumed begin finds them.
    let mut workers: Vec<StreamWorker> = config
        .files
        .iter()
        .map(|(device, dir)| {
            let names = config
                .streams
                .iter()
                .find(|(d, _)| d == device)
                .map(|(_, names)| names.clone())
                .unwrap_or_default();
            StreamWorker::start(device, dir, names, &config.lemnosd_socket, config.stall)
        })
        .collect();
    while !shared.stopping() {
        // One Orion session for each receiver: a session hands each kind of file frame to the
        // first receiver on it, so two prefixes on one session would leave the second unanswered.
        let mut takers = Vec::new();
        for (index, (device, dir)) in config.files.iter().enumerate() {
            if let Some((orion, prefix, sink)) = receiver_for(config, shared, device) {
                // Said once the sink is Orion's: a push from now on reaches this thread.
                shared.with(|mirror| mirror.set_pushes(device, crate::pushes::ACCEPTED));
                takers.push(Taker {
                    device: device.clone(),
                    prefix,
                    sink,
                    _orion: orion,
                    receiver: Receiver::new(dir),
                    streamer: index,
                });
            }
        }
        let mut refusers = Vec::new();
        for device in &unplaced {
            if let Some((orion, _, sink)) = receiver_for(config, shared, device) {
                let text = crate::pushes::refused_text(device);
                shared.with(|mirror| mirror.set_pushes(device, &text));
                refusers.push(Refuser {
                    sink,
                    _orion: orion,
                    why: crate::pushes::refused_why(device),
                });
            }
        }
        shared.changed();
        if takers.is_empty() && refusers.is_empty() {
            std::thread::sleep(backoff.next_delay());
            continue;
        }
        backoff.reset();
        let lost = serve(
            config,
            shared,
            &mut takers,
            &mut refusers,
            &mut workers,
            &mut lemnosd,
        );
        if let Err(error) = lost {
            log(&format!("files: Orion lost: {error}"));
        }
        // The sinks went with the session: a push now reaches no receiver, until the next ones
        // are accepted. Said at once, so no status reads `accepted` across the gap.
        not_ready(shared, takers.iter().map(|taker| taker.device.as_str()));
    }
}

/// Says each of `devices` takes no pushes for now ([`crate::pushes::NOT_READY`]), and wakes the
/// publisher.
fn not_ready<'a>(shared: &Shared, devices: impl Iterator<Item = &'a str>) {
    shared.with(|mirror| {
        for device in devices {
            mirror.set_pushes(device, crate::pushes::NOT_READY);
        }
    });
    shared.changed();
}

/// A session of its own on Orion, as the receiver of every file sent to `lemnos/<device>/`, with
/// that prefix. `None`, said in the log, when Orion refuses the session or the prefix, or the
/// service stops: the caller tries again on its backoff.
fn receiver_for(config: &Config, shared: &Shared, device: &str) -> Option<(Orion, String, Sink)> {
    // Orion's patience keeps trying while nobody answers, so this only sees a refusal.
    let orion = Orion::connect_watching(
        &config.orion,
        FILES_CLIENT_NAME,
        Options::default(),
        |status| {
            if status.failures == 1 {
                log(&format!(
                    "files: Orion not available at {}: waiting for it",
                    config.orion
                ));
            }
            !shared.stopping()
        },
    )
    .map_err(|error| {
        log(&format!(
            "files: Orion at {} refused the session: {error}",
            config.orion
        ));
    })
    .ok()?;
    let prefix = format!("{DESTINATION_PREFIX}{device}/");
    match orion.files().sink(&prefix) {
        Ok(sink) => Some((orion, prefix, sink)),
        Err(error) => {
            log(&format!("files: Orion refused {prefix}: {error}"));
            None
        }
    }
}

/// Asks lemnosd, until it answers, which devices take files and have no place given, and says so
/// once for each. `None` when the service stops first.
fn find_unplaced(config: &Config, shared: &Shared) -> Option<Vec<String>> {
    let mut backoff = Backoff::new();
    let mut said = false;
    while !shared.stopping() {
        let found = Client::connect(&config.lemnosd_socket, LEMNOSD_FILES_NAME)
            .map_err(|e| format!("lemnosd: {e}"))
            .and_then(|client| crate::pushes::unplaced(&client, config));
        match found {
            Ok(unplaced) => {
                for notice in crate::pushes::notices(&unplaced) {
                    log(&notice);
                }
                return Some(unplaced);
            }
            Err(why) => {
                if !said {
                    log(&format!(
                        "files: can't tell yet which devices take files: {why}"
                    ));
                    said = true;
                }
                std::thread::sleep(backoff.next_delay());
            }
        }
    }
    None
}

fn serve(
    config: &Config,
    shared: &Shared,
    takers: &mut [Taker],
    refusers: &mut [Refuser],
    workers: &mut [StreamWorker],
    lemnosd: &mut Option<Client>,
) -> Result<(), orion::ClientError> {
    let look = LOOK
        / u32::try_from(takers.len() + refusers.len())
            .unwrap_or(1)
            .max(1);
    while !shared.stopping() {
        for refuser in refusers.iter_mut() {
            let Some(handed) = refuser.sink.next(look)? else {
                continue;
            };
            let (Handed::Begin { transfer, .. }
            | Handed::Chunk { transfer, .. }
            | Handed::Commit { transfer }) = handed;
            refuser
                .sink
                .status(transfer, 0, FileState::Rejected, &refuser.why)?;
        }
        for taker in takers.iter_mut() {
            for (transfer, answer) in taker.receiver.expire(config.stall) {
                taker
                    .sink
                    .status(transfer, answer.received, answer.state, &answer.detail)?;
            }
            let worker = &mut workers[taker.streamer];
            // The stream worker's answers, as it made them: this thread never waits for lemnosd on a
            // stream, so the session is pumped while a device call or a job's end takes its time.
            for (transfer, answer) in worker.answers() {
                taker
                    .sink
                    .status(transfer, answer.received, answer.state, &answer.detail)?;
                if answer.state == FileState::Receiving {
                    shared.transferred();
                }
            }
            let Some(handed) = taker.sink.next(look)? else {
                continue;
            };
            let (transfer, answer) = match handed {
                Handed::Begin {
                    transfer,
                    destination,
                    size,
                    digest,
                } if worker.takes(destination.strip_prefix(&taker.prefix).unwrap_or("")) => {
                    // A stream: its bytes go to the device's job as they arrive, through the worker.
                    let name = destination.strip_prefix(&taker.prefix).unwrap_or("");
                    match worker.begin(transfer, name, size, digest) {
                        Some(answer) => (transfer, answer),
                        None => continue,
                    }
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
                } if worker.owns(transfer) => match worker.chunk(transfer, offset, data) {
                    Some(answer) => (transfer, answer),
                    None => continue,
                },
                Handed::Chunk {
                    transfer,
                    offset,
                    data,
                } => (
                    transfer,
                    accepted(shared, taker.receiver.chunk(transfer, offset, &data)),
                ),
                Handed::Commit { transfer } if worker.owns(transfer) => {
                    match worker.commit(transfer) {
                        Some(answer) => (transfer, answer),
                        None => continue,
                    }
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
