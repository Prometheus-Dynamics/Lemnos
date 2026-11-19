//! The node's named files (`lemnos/`, `docs/daemon.md`, "Named files") as Orion serves them.
//!
//! A file Orion sends is a [`Snapshot`]: a `Read + Seek` over the bytes the node held when the
//! transfer was opened, read from the node in pieces and closed when the snapshot is dropped. A
//! transfer that resumes seeks to its offset; the bytes do not change under it.
//!
//! The node is reached through [`Node`]. [`Lemnosd`] is the daemon over its client, to the device
//! named by `LEMNOS_ORION_NAMED_FILES`; tests give a fake.
//!
//! ## The offer
//!
//! Orion serves a prefix through its offer API: `Files::offer(prefix)` (`crates/client/files/src/offer.rs`
//! at the pinned rev b76a039c) gives an [`Offer`](orion::files::Offer) whose `next` gives each file asked
//! for, as an [`Asked`](orion::files::Asked). That file is answered with `refuse(why)`, or given with
//! `give_known` and stepped to its end with `finish`. [`run`] is the loop that connects the offer to these
//! snapshots. It runs on its own thread with its own Orion session, and it runs only when
//! `LEMNOS_ORION_NAMED_FILES` names a device (see `main`). A failed or ended offer is taken again on the
//! bridge's backoff (1 s, doubling, capped at 8 s): nothing gives up, and the reason is kept in
//! [`Shared::named_files`] and logged.

use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;
use std::time::Duration;

use lemnos_client::Client;
use orion::files::{Asked, Giving, Known};
use orion::{Address, Options, Orion};
use sha2::{Digest, Sha256};

use crate::backoff::Backoff;
use crate::{Shared, log};

/// The prefix the node's names carry.
pub const PREFIX: &str = "lemnos/";
/// The environment variable naming the node's named-files device.
pub const DEVICE_ENV: &str = "LEMNOS_ORION_NAMED_FILES";
/// The name this thread's Orion session has, and the name its lemnosd connections have.
pub const CLIENT_NAME: &str = "lemnos-orion-named";
pub const LEMNOSD_NAME: &str = "orion-named";
/// How long one look at the offer waits for a file asked for.
const LOOK: Duration = Duration::from_millis(50);

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
    /// A connection to lemnosd for `device`. Each transfer takes its own, so a node that has gone is
    /// found by the next transfer, not kept as a dead handle.
    ///
    /// # Errors
    ///
    /// When lemnosd cannot be reached, with the reason as text.
    pub fn connect(socket: &Path, device: &str) -> Result<Self, String> {
        let client = Client::connect(socket, LEMNOSD_NAME)
            .map_err(|e| format!("lemnosd is not there: {e}"))?;
        Ok(Self {
            client,
            device: device.to_owned(),
        })
    }

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

/// What the offer's files are answered with: the name asked for, where the client's copy stops, and
/// how to answer. [`Asked`] is the real one; tests give a fake.
pub trait Asker {
    /// The file asked for, as the client named it.
    fn name(&self) -> &str;
    /// Where the client already has the file up to, in bytes from its start.
    fn resume_at(&self) -> u64;
    /// Does not give the file: the reason goes to the client.
    fn refuse(self, why: &str) -> Result<(), String>;
    /// Gives `source` from [`Asker::resume_at`] and waits until the client has answered.
    fn give(self, source: Snapshot, known: Known) -> Result<Giving, String>;
}

impl Asker for Asked {
    fn name(&self) -> &str {
        Asked::name(self)
    }

    fn resume_at(&self) -> u64 {
        Asked::from(self)
    }

    fn refuse(self, why: &str) -> Result<(), String> {
        Asked::refuse(self, why).map_err(|e| format!("{e}"))
    }

    fn give(self, source: Snapshot, known: Known) -> Result<Giving, String> {
        self.give_known(source, known)
            .map_err(|e| format!("{e}"))?
            .finish()
            .map_err(|e| format!("{e}"))
    }
}

/// The digest and size of a snapshot, read from its start. `pump` runs before each read, so the Orion
/// session is answered while a file is read for its digest, as the client's own pass does.
///
/// # Errors
///
/// When the snapshot cannot be read, or `pump` fails.
pub fn digest(
    snapshot: &mut Snapshot,
    pump: &mut dyn FnMut() -> Result<(), String>,
) -> Result<Known, String> {
    snapshot
        .seek(SeekFrom::Start(0))
        .map_err(|e| format!("{e}"))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; PIECE];
    loop {
        pump()?;
        let n = snapshot.read(&mut buf).map_err(|e| format!("{e}"))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(Known {
        digest: hasher.finalize().into(),
        size: snapshot.size(),
    })
}

/// How one asked file ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ended {
    /// Not given, and the client was told why.
    Refused(String),
    /// Given, and the client answered: taken or not, and why.
    Gave(Giving),
    /// The gift began and then broke. The client is not told by this bridge: the pinned Orion has no
    /// call that ends a gift already under way, so the client's own silence limit ends it.
    Broke(String),
}

/// Answers one asked file: a name outside `lemnos/` is refused before any node is reached; a node that
/// cannot be reached, a name it refuses, or a read that fails before the gift begins is refused with
/// its reason; otherwise the file is digested and given from the client's offset.
pub fn serve<A: Asker>(
    asked: A,
    node: impl FnOnce() -> Result<Box<dyn Node>, String>,
    pump: &mut dyn FnMut() -> Result<(), String>,
) -> Ended {
    let name = asked.name().to_owned();
    if !name.starts_with(PREFIX) {
        return refused(
            asked,
            format!("{name:?} is not a `{PREFIX}` name this bridge serves"),
        );
    }
    let mut snapshot = match node().and_then(|node| Snapshot::open(node, &name)) {
        Ok(snapshot) => snapshot,
        Err(why) => return refused(asked, why),
    };
    let known = match digest(&mut snapshot, pump) {
        Ok(known) => known,
        Err(why) => return refused(asked, why),
    };
    match asked.give(snapshot, known) {
        Ok(giving) => Ended::Gave(giving),
        Err(why) => Ended::Broke(why),
    }
}

fn refused<A: Asker>(asked: A, why: String) -> Ended {
    if let Err(error) = asked.refuse(&why) {
        log(&format!(
            "named files: the refusal of {why:?} did not reach Orion: {error}"
        ));
    }
    Ended::Refused(why)
}

/// Runs `attempt` until `stopping`. An attempt returns only when it has ended: `Err` with why, when it
/// could not connect or its offer ended or failed, `Ok` when the service is stopping. Between tries the
/// bridge's backoff sleeps (1 s, doubling, capped at 8 s, never giving up); an attempt that has an
/// offer in place resets it, so a later failure starts again at 1 s.
pub fn keep_offering(
    backoff: &mut Backoff,
    mut attempt: impl FnMut(&mut Backoff) -> Result<(), String>,
    mut sleep: impl FnMut(Duration),
    stopping: impl Fn() -> bool,
    mut note: impl FnMut(&str),
) {
    while !stopping() {
        if let Err(why) = attempt(backoff) {
            let delay = backoff.next_delay();
            note(&format!("{why}: trying again in {} s", delay.as_secs()));
            sleep(delay);
        }
    }
}

/// Serves the `lemnos/` names to Orion until the service stops, from `device` on lemnosd at `lemnosd`.
/// Runs on its own thread; the state and the log say what it is doing.
pub fn run(address: &Address, lemnosd: &Path, device: &str, shared: &Shared) {
    let mut backoff = Backoff::new();
    keep_offering(
        &mut backoff,
        |backoff| offer_once(address, lemnosd, device, shared, backoff),
        std::thread::sleep,
        || shared.stopping(),
        |why| {
            shared.set_named_files(why);
            log(&format!("named files: {why}"));
        },
    );
}

/// One session to Orion and one offer on it, served until the offer fails or the service stops.
fn offer_once(
    address: &Address,
    lemnosd: &Path,
    device: &str,
    shared: &Shared,
    backoff: &mut Backoff,
) -> Result<(), String> {
    let orion = Orion::connect_watching(address, CLIENT_NAME, Options::default(), |status| {
        if status.failures == 1 {
            log(&format!(
                "named files: Orion not available at {address}: waiting for it"
            ));
        }
        !shared.stopping()
    })
    .map_err(|e| format!("Orion refused the named-files session: {e}"))?;
    let mut offer = orion
        .files()
        .offer(PREFIX)
        .map_err(|e| format!("Orion refused the offer of {PREFIX}: {e}"))?;
    backoff.reset();
    shared.set_named_files(&format!("offering {PREFIX} from {device}"));
    while !shared.stopping() {
        let Some(asked) = offer
            .next(LOOK)
            .map_err(|e| format!("Orion lost the offer: {e}"))?
        else {
            continue;
        };
        let node = || Lemnosd::connect(lemnosd, device).map(|node| Box::new(node) as Box<dyn Node>);
        let mut pump = || {
            orion
                .pump(Duration::ZERO)
                .map(|_| ())
                .map_err(|e| format!("{e}"))
        };
        let name = asked.name().to_owned();
        match serve(asked, node, &mut pump) {
            Ended::Refused(why) => {
                shared.set_named_files(&format!("refused {name}: {why}"));
                log(&format!("named files: refused {name}: {why}"));
            }
            Ended::Gave(giving) => {
                log(&format!("named files: {name}: {giving:?}"));
            }
            Ended::Broke(why) => {
                shared.set_named_files(&format!("broke {name}: {why}"));
                log(&format!("named files: {name} broke: {why}"));
            }
        }
    }
    Ok(())
}
