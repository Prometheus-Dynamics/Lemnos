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
//! is the chunks in flight, bounded ([`StreamWorker`]). The calls run on the stream's worker thread, never
//! on the thread that pumps the Orion session, and a stream begun again (its sender's session broke) resumes
//! at the bytes the device has taken (`docs/slots.md`, "Over Orion"). The stream's SHA-256 is checked against the transfer's digest at its commit; the
//! job's own result (the device checks the decoded image) is the answer the sender gets: its fields,
//! read with the contract's `read_result`, as the commit's detail (see [`committed_detail`]).
//!
//! The protocol the device's methods take is the one its contract documents (`system.flash/1`:
//! `manifest(bytes, bytes)`, `write(u64, u8)` a long job, `data(u64, bytes)`); the arguments are
//! encoded here as Lemnos's wire encodes them: little-endian, a `u32` length before each byte run.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use lemnos_client::{Client, Handle};
use lemnos_contract_system_flash::read_result;
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
    /// The last stream that ended at its commit, and how: a begin or commit of it again is answered
    /// with this, never by starting it over. A committed one is kept until another stream ends; a
    /// rejected one is answered once, so the sender's next push of it starts it afresh.
    finished: Option<(u64, Answer)>,
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
    /// The device's `info`: read when the write fails, for the reason the device gives.
    info: u16,
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
            finished: None,
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
    /// otherwise hold the device for ever, and its write is cancelled with it. The receiver expires
    /// the file at the same time. Says how the stream ended, for the sender, as the receiver does.
    /// Nothing says a sender went; a quiet one is dropped the same.
    // ORION-R2 (C33): the sender's session end (Orion's Cancel with its reason) would say the stream is
    // over at once; until then the bound is the one `idle` gives.
    pub fn expire(&mut self, idle: Duration) -> Option<(u64, Answer)> {
        if !self
            .active
            .as_ref()
            .is_some_and(|a| a.seen.elapsed() >= idle)
        {
            return None;
        }
        let received = self.active.as_ref().map_or(0, |a| a.got);
        let transfer = self.active.as_ref().map_or(0, |a| a.transfer);
        self.drop_active();
        Some((transfer, Answer::timed_out(received, idle)))
    }

    /// A stream begins: its manifest goes to the device and its job starts. A stream begun while
    /// another is received ends the other, and its job with it. The stream being received begun again
    /// (the sender's session broke and it resumes) is answered with the bytes the device has taken, and
    /// goes on: it never goes back to 0. One that ended at its commit is answered with how it ended.
    pub fn begin(&mut self, transfer: u64, name: &str, size: u64, digest: [u8; 32]) -> Answer {
        if let Some(active) = self.active.as_mut().filter(|a| a.transfer == transfer) {
            active.seen = Instant::now();
            return Answer {
                received: active.got,
                state: FileState::Receiving,
                detail: String::new(),
            };
        }
        if let Some(answer) = self.ended(transfer) {
            return answer;
        }
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
        let (_, info) = find_method(&description, "info")?;
        let (manifest_id, write_id, data_id, info_id) =
            (manifest_method.id, write.id, data.id, info.id);
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
            info: info_id,
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
        // A chunk the device already has (sent again on a resumed session) is answered with where the
        // stream is; one that reaches past it gives only its new bytes; one after a gap is refused.
        if offset > got {
            self.drop_active();
            return rejected("a chunk came out of order".into());
        }
        let skip = usize::try_from(got - offset).unwrap_or(usize::MAX);
        if skip >= bytes.len() {
            if let Some(active) = self.active.as_mut() {
                active.seen = Instant::now();
            }
            return Answer {
                received: got,
                state: FileState::Receiving,
                detail: String::new(),
            };
        }
        let (offset, bytes) = (got, &bytes[skip..]);
        let mut args = Vec::with_capacity(12 + bytes.len());
        args.extend_from_slice(&offset.to_le_bytes());
        put_bytes(&mut args, bytes);
        let Some(active) = self.active.as_mut() else {
            return rejected("no stream is being received".into());
        };
        let data = active.data;
        let sent = match self.client.as_ref().filter(|c| c.is_connected()) {
            Some(client) => offer(client, &mut active.handle, data, &args),
            None => Err("lemnosd went away during the stream".to_string()),
        };
        if let Err(detail) = sent {
            let detail = match self.client.as_ref() {
                Some(client) => with_reason(detail, client, &mut active.handle, active.info),
                None => detail,
            };
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

    /// How the stream `transfer` ended at its commit, when it is the last one that did. A rejection is
    /// told once.
    fn ended(&mut self, transfer: u64) -> Option<Answer> {
        let (last, answer) = self.finished.as_ref()?;
        if *last != transfer {
            return None;
        }
        let answer = answer.clone();
        if answer.state != FileState::Committed {
            self.finished = None;
        }
        Some(answer)
    }

    /// The stream is all sent: its size and digest are checked, and the job's result is the answer.
    /// It is kept, so a commit sent again (the sender's session broke while the job was ending) is
    /// answered the same.
    pub fn commit(&mut self, transfer: u64) -> Answer {
        if !self.owns(transfer) {
            return self
                .ended(transfer)
                .unwrap_or_else(|| rejected("no stream is being received".into()));
        }
        let answer = self.commit_active();
        self.finished = Some((transfer, answer.clone()));
        answer
    }

    fn commit_active(&mut self) -> Answer {
        let Some(mut active) = self.active.take() else {
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
            Some((status, result)) if status.0 == 0 => match read_result(&result) {
                Some(fields) => Answer {
                    received: active.got,
                    state: FileState::Committed,
                    detail: committed_detail(fields.raw_bytes, fields.outcome),
                },
                // The image is written, but its result is not the contract's: say so, never a count.
                None => rejected(format!(
                    "the write ended, but its result is not system.flash's fields: {}",
                    String::from_utf8_lossy(&result)
                )),
            },
            // A failed job carries no result: the device's `info` says why (the byte a failed write
            // stopped at, and that the target holds a partial image).
            Some((status, _)) => rejected(with_reason(
                format!("the write ended with status {}", status.0),
                client,
                &mut active.handle,
                active.info,
            )),
            None => rejected("the write did not end".into()),
        }
    }
}

/// Chunks the pump may have handed the worker and not yet had answered. Orion keeps only a few chunks
/// in flight, so a stream past this is a sender that is not paced by its answers: it is refused.
const QUEUED_LIMIT: usize = 64;

/// How long the worker waits for work before it looks for a quiet stream.
const WORKER_LOOK: Duration = Duration::from_millis(100);

/// What the pump hands the worker.
enum Op {
    Begin {
        transfer: u64,
        name: String,
        size: u64,
        digest: [u8; 32],
    },
    Chunk {
        transfer: u64,
        offset: u64,
        data: Vec<u8>,
    },
    Commit {
        transfer: u64,
    },
}

/// What the pump knows of the worker without waiting for it.
#[derive(Debug, Default)]
struct View {
    /// The stream whose commit the worker is waiting on, and its size.
    committing: Option<(u64, u64)>,
}

/// A [`Streamer`] on a thread of its own, so the thread that pumps the Orion session never waits for
/// lemnosd: a device call or a job's end may take as long as the device needs, and the session is
/// pumped all that time. The pump hands each begin, chunk and commit over ([`StreamWorker::begin`],
/// [`StreamWorker::chunk`], [`StreamWorker::commit`]) and sends the answers [`StreamWorker::answers`]
/// gives back, in the order the worker made them. The worker outlives the Orion session: a stream
/// and its job are kept across a broken session, and the sender's resumed begin is answered with the
/// bytes the device has taken.
pub struct StreamWorker {
    names: Vec<String>,
    /// The last stream begun through this worker: its chunks and commit are the worker's.
    mine: Option<u64>,
    ops: mpsc::Sender<Op>,
    answers: mpsc::Receiver<(u64, Answer)>,
    view: Arc<Mutex<View>>,
    queued: Arc<AtomicUsize>,
}

impl StreamWorker {
    /// Starts the worker for `device`'s streams `names`, whose manifests are in `dir`, reaching
    /// lemnosd at `socket`. `stall` is how long a stream may go without a chunk before it is dropped.
    pub fn start(
        device: &str,
        dir: &Path,
        names: Vec<String>,
        socket: &Path,
        stall: Duration,
    ) -> Self {
        let (ops, work) = mpsc::channel();
        let (tell, answers) = mpsc::channel();
        let view = Arc::new(Mutex::new(View::default()));
        let queued = Arc::new(AtomicUsize::new(0));
        let mut streamer = Streamer::new(device, dir, names.clone(), socket);
        let (worker_view, worker_queued) = (Arc::clone(&view), Arc::clone(&queued));
        // The thread ends when the pump's end of the channel goes (the service stops); a commit it is
        // waiting on is let finish first.
        std::thread::spawn(move || {
            loop {
                let op = match work.recv_timeout(WORKER_LOOK) {
                    Ok(op) => op,
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if let Some(ended) = streamer.expire(stall)
                            && tell.send(ended).is_err()
                        {
                            return;
                        }
                        continue;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                };
                let answer = match op {
                    Op::Begin {
                        transfer,
                        name,
                        size,
                        digest,
                    } => (transfer, streamer.begin(transfer, &name, size, digest)),
                    Op::Chunk {
                        transfer,
                        offset,
                        data,
                    } => {
                        worker_queued.fetch_sub(1, Ordering::Relaxed);
                        (transfer, streamer.chunk(transfer, offset, &data))
                    }
                    Op::Commit { transfer } => {
                        let size = streamer.active.as_ref().map_or(0, |a| a.size);
                        lock(&worker_view).committing = Some((transfer, size));
                        let answer = streamer.commit(transfer);
                        lock(&worker_view).committing = None;
                        (transfer, answer)
                    }
                };
                if tell.send(answer).is_err() {
                    return;
                }
            }
        });
        Self {
            names,
            mine: None,
            ops,
            answers,
            view,
            queued,
        }
    }

    /// Whether the files named `name` are this worker's streams.
    pub fn takes(&self, name: &str) -> bool {
        self.names.iter().any(|n| n == name)
    }

    /// Whether `transfer` is the last stream begun through this worker.
    pub fn owns(&self, transfer: u64) -> bool {
        self.mine == Some(transfer)
    }

    /// A stream begins (or the sender resumes it). Answered at once when the worker is waiting on that
    /// stream's commit: every byte is taken, and the commit sent again is answered with how it ended.
    pub fn begin(
        &mut self,
        transfer: u64,
        name: &str,
        size: u64,
        digest: [u8; 32],
    ) -> Option<Answer> {
        self.mine = Some(transfer);
        if let Some((committing, size)) = lock(&self.view).committing
            && committing == transfer
        {
            return Some(Answer {
                received: size,
                state: FileState::Receiving,
                detail: String::new(),
            });
        }
        self.hand(Op::Begin {
            transfer,
            name: name.to_owned(),
            size,
            digest,
        })
    }

    /// A chunk of the stream: answered by the worker once the device has taken it.
    pub fn chunk(&mut self, transfer: u64, offset: u64, data: Vec<u8>) -> Option<Answer> {
        if self.queued.load(Ordering::Relaxed) >= QUEUED_LIMIT {
            return Some(rejected(format!(
                "more than {QUEUED_LIMIT} chunks were sent before the first was answered"
            )));
        }
        self.queued.fetch_add(1, Ordering::Relaxed);
        self.hand(Op::Chunk {
            transfer,
            offset,
            data,
        })
    }

    /// The stream is all sent: answered by the worker when the device's job has ended.
    pub fn commit(&mut self, transfer: u64) -> Option<Answer> {
        self.hand(Op::Commit { transfer })
    }

    /// The answers the worker has made since the last look, in order.
    pub fn answers(&self) -> impl Iterator<Item = (u64, Answer)> + '_ {
        self.answers.try_iter()
    }

    fn hand(&self, op: Op) -> Option<Answer> {
        self.ops
            .send(op)
            .err()
            .map(|_| rejected("the bridge's stream worker has stopped".into()))
    }
}

fn lock(view: &Mutex<View>) -> std::sync::MutexGuard<'_, View> {
    view.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The name this streamer gives lemnosd.
pub const STREAMS_CLIENT_NAME: &str = "lemnos-orion-streams";

/// The detail of a committed image: fields, not a sentence, so a sender maps them without parsing words.
/// `raw_bytes=<the raw image's bytes written> outcome=<code>`, separated by one space; the code is the write
/// job's signature outcome after its byte count: `0` verified, `1` not checked (unsigned), `2` not checked
/// (signed by a key the board does not know), `none` when the job reported none. A reader ignores a field it
/// does not know, so a field may be added after these.
pub fn committed_detail(raw: u64, outcome: Option<u8>) -> String {
    match outcome {
        Some(code @ 0..=2) => format!("raw_bytes={raw} outcome={code}"),
        _ => format!("raw_bytes={raw} outcome=none"),
    }
}

/// The first wait before a chunk the device answered `BUSY` is offered again, and the longest.
const BEHIND_FIRST: Duration = Duration::from_millis(2);
const BEHIND_CAP: Duration = Duration::from_millis(200);

/// Gives the device one chunk. `BUSY` is the device's writer behind, with nothing taken: the same chunk is
/// offered again, after a wait that doubles to [`BEHIND_CAP`], for as long as the device answers so. The
/// sender is not answered meanwhile, which is what paces it. Any other refusal is the answer.
/// `detail` with the device's own reason after it: `info`'s `error`, when the device gives one.
fn with_reason(detail: String, client: &Client, handle: &mut Handle, info: u16) -> String {
    let mut text = vec![0u8; 8192];
    let cap = u32::try_from(text.len()).unwrap_or(u32::MAX).to_le_bytes();
    let error = client
        .call(handle, info, &cap, &mut text)
        .ok()
        .and_then(|n| std::str::from_utf8(&text[..n]).ok().and_then(Json::parse))
        .and_then(|json| json.get("error").and_then(Json::text).map(str::to_string))
        .filter(|error| !error.is_empty());
    match error {
        Some(error) => format!("{detail}: {error}"),
        None => detail,
    }
}

fn offer(client: &Client, handle: &mut Handle, data: u16, args: &[u8]) -> Result<(), String> {
    let mut wait = BEHIND_FIRST;
    loop {
        match client.call(handle, data, args, &mut [0u8; 0]) {
            Ok(_) => return Ok(()),
            Err(lemnos_client::Error::Refused { status, .. })
                if status == lemnos_core::Status::BUSY =>
            {
                std::thread::sleep(wait);
                wait = (wait * 2).min(BEHIND_CAP);
            }
            Err(e) => return Err(format!("the device refused the data: {e}")),
        }
    }
}

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

#[cfg(test)]
mod tests {
    use super::committed_detail;

    #[test]
    fn a_committed_image_s_detail_is_its_fields_for_every_outcome() {
        assert_eq!(
            committed_detail(16384, Some(0)),
            "raw_bytes=16384 outcome=0"
        );
        assert_eq!(
            committed_detail(16384, Some(1)),
            "raw_bytes=16384 outcome=1"
        );
        assert_eq!(committed_detail(7, Some(2)), "raw_bytes=7 outcome=2");
        assert_eq!(committed_detail(7, None), "raw_bytes=7 outcome=none");
        assert_eq!(committed_detail(7, Some(9)), "raw_bytes=7 outcome=none");
    }
}
