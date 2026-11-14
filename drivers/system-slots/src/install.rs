//! An install: a bundle streamed in, unpacked into a work directory as it comes, checked against
//! its signed manifest, and moved into place.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ed25519_dalek::{Signature, VerifyingKey};
use lemnos_core::{CallerId, JobId, Status};
use lemnos_slot_store::bundle::{self, Member};
use lemnos_slot_store::signing::RECORD_LEN;
use lemnos_slot_store::signing::{self, Check, Outcome, Refusal};
use sha2::{Digest, Sha256};

use crate::boot::{self, hex};
use crate::manifest::Manifest;
use crate::store::{Layout, Meta};
use crate::tar::{Kind, Sink, Tar};
use lemnos_slot_store::ubi::{self, UbiWrite};

/// The length of a bundle's `slot.sig`: the shared signature record (`lemnos_slot_store::signing`),
/// 108 bytes. The bare 64-byte signature of the first version is no longer accepted.
pub const SIGNATURE_LEN: usize = RECORD_LEN;
/// No space left on the device.
pub const NO_SPACE: Status = Status::from_errno(28);
/// What an install is called in its work directory's name.
pub const WORK_PREFIX: &str = ".install.";
/// How many bytes of a member are written before they are flushed to the disk.
const SYNC_EVERY: u64 = 4 << 20;

/// Why an install stopped: the status the job ends with, and words for the board's state.
pub type Failure = (Status, String);

fn bad(text: impl Into<String>) -> Failure {
    (Status::INVALID_ARGUMENT, text.into())
}

fn disk(what: &str) -> impl Fn(io::Error) -> Failure + '_ {
    move |error| (Status::IO, format!("{what}: {error}"))
}

/// A key the board trusts, and whose it is.
#[derive(Debug, Clone)]
pub struct TrustedKey {
    pub key: VerifyingKey,
    /// Who holds the private half, as the key file names them.
    pub owner: String,
}

/// The keys the board trusts: one per line, 64 hex digits, a space, then the key's owner (any
/// text, which `list` shows for a slot that key signed). Blank lines and lines starting with `#`
/// are skipped.
pub fn parse_keys(text: &str) -> Result<Vec<TrustedKey>, String> {
    let mut keys = Vec::new();
    for (at, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (hex, owner) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        let owner = owner.trim();
        if owner.is_empty() {
            return Err(format!("line {} of the key file names no owner", at + 1));
        }
        let bytes = unhex(hex)
            .and_then(|b| <[u8; 32]>::try_from(b).ok())
            .ok_or_else(|| format!("line {} of the key file is not 64 hex digits", at + 1))?;
        keys.push(TrustedKey {
            key: VerifyingKey::from_bytes(&bytes)
                .map_err(|_| format!("line {} of the key file is not a key", at + 1))?,
            owner: owner.to_string(),
        });
    }
    Ok(keys)
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(text.get(at..at + 2)?, 16).ok())
        .collect()
}

/// The slots a bundle may not replace.
#[derive(Debug, Clone, Default)]
pub struct Guard {
    pub running: String,
    pub default: String,
}

impl Guard {
    fn check(&self, id: &str) -> Result<(), Failure> {
        bundle::check_slot_id(id).map_err(bad)?;
        if id == self.running {
            return Err((Status::BUSY, format!("{id} is the running slot")));
        }
        if id == self.default {
            return Err((Status::BUSY, format!("{id} is the default slot")));
        }
        Ok(())
    }
}

struct Writing {
    file: File,
    hasher: Sha256,
    name: String,
    /// Bytes written since the last flush.
    unsynced: u64,
}

/// Where the image goes while it comes, on raw NAND: the device's writer and the work volume.
#[derive(Clone)]
struct WorkVolume {
    writer: Arc<dyn UbiWrite>,
    name: String,
}

/// The tar's members, into the work directory (and the image into its work volume on raw NAND).
struct Unpack {
    work: PathBuf,
    volume: Option<WorkVolume>,
    guard: Guard,
    /// The board's keys and policy, given when the install started.
    admit: Admit,
    manifest: Vec<u8>,
    signature: Vec<u8>,
    /// slot.json as it parses, once it has come.
    parsed: Option<Manifest>,
    /// Every member begun, in order (directories included).
    entries: Vec<String>,
    /// The signature decision and the board's checks: made once, when the bundle first needs
    /// it (the end of slot.sig, or the first payload member), and the same answer after.
    decided: Option<Result<Outcome, Failure>>,
    /// The decision's words for `info`: the outcome, or the refusal's text.
    signed: Option<Result<Outcome, String>>,
    current: Option<(Member, Option<Writing>)>,
    /// Each file written, with its SHA-256.
    hashes: Vec<(String, String)>,
    failure: Option<Failure>,
}

impl Unpack {
    fn fail<T>(&mut self, failure: Failure) -> Result<T, String> {
        let text = failure.1.clone();
        self.failure.get_or_insert(failure);
        Err(text)
    }

    /// The member's place in the bundle's order, refused by a sentence naming the rule.
    fn order(&mut self, name: &str, kind: Kind) -> Result<Member, Failure> {
        let what = bundle::check_order(&self.entries, name, kind).map_err(bad)?;
        if matches!(what, Member::Root | Member::Boot) {
            // The first payload member: the signature and the board are decided now, so a
            // refused bundle has written nothing and the root image is not consumed.
            self.settle()?;
            if what == Member::Boot && kind == Kind::File {
                let file = name.strip_prefix("boot/").unwrap_or(name);
                if let Some(manifest) = &self.parsed {
                    bundle::check_listed(manifest, file).map_err(bad)?;
                } else {
                    return Err(bad(format!(
                        "boot file {file} isn't in slot.json: a file the manifest does not list is refused"
                    )));
                }
            }
        }
        Ok(what)
    }

    /// The signature decision and the board's checks, made once.
    fn settle(&mut self) -> Result<Outcome, Failure> {
        if self.decided.is_none() {
            let result = self.decide();
            self.signed = Some(match &result {
                Ok(outcome) => Ok(outcome.clone()),
                Err((_, text)) => Err(text.clone()),
            });
            self.decided = Some(result);
        }
        self.decided
            .clone()
            .unwrap_or_else(|| Err(bad("the signature was not decided")))
    }

    fn decide(&self) -> Result<Outcome, Failure> {
        let check = signature_check(&self.admit.keys, &self.manifest, &self.signature);
        let outcome = match signing::decide(self.admit.require_signed, "the bundle", check) {
            Ok(outcome) => outcome,
            Err(Refusal::Malformed(text)) => return Err(bad(text)),
            Err(Refusal::Policy(text)) => return Err((Status::NOT_PERMITTED, text)),
        };
        if self.admit.board_id.is_empty() {
            return Err((
                Status::NO_DEVICE,
                "the board file has no [board] id to match a slot's board against".into(),
            ));
        }
        let Some(parsed) = &self.parsed else {
            return Err(bad("slot.json isn't a slot manifest"));
        };
        if parsed.board != self.admit.board_id {
            return Err((
                Status::NOT_SUPPORTED,
                format!(
                    "this slot is built for board {}, and this is board {}",
                    parsed.board, self.admit.board_id
                ),
            ));
        }
        Ok(outcome)
    }
}

impl Sink for Unpack {
    fn begin(&mut self, name: &str, kind: Kind, size: u64) -> Result<(), String> {
        let what = match self.order(name, kind) {
            Ok(what) => what,
            Err(failure) => return self.fail(failure),
        };
        self.entries.push(name.to_string());
        match (what, kind) {
            (Member::Boot, Kind::Dir) => {
                self.current = Some((what, None));
                fs::create_dir_all(self.work.join(name)).map_err(|e| e.to_string())
            }
            (_, Kind::Dir) => self.fail(bad(format!("{name} is a directory in the bundle"))),
            (Member::Manifest | Member::Signature, _) => {
                if let Err(why) = bundle::check_member_size(what, size) {
                    return self.fail(bad(why));
                }
                self.current = Some((what, None));
                Ok(())
            }
            (Member::Root, _) if self.volume.is_some() => {
                let Some(volume) = self.volume.clone() else {
                    return Ok(());
                };
                let opened = volume
                    .writer
                    .make(&volume.name, size)
                    .and_then(|()| volume.writer.update(&volume.name, size));
                match opened {
                    Ok(file) => {
                        self.current = Some((
                            what,
                            Some(Writing {
                                file,
                                hasher: Sha256::new(),
                                name: name.to_string(),
                                unsynced: 0,
                            }),
                        ));
                        Ok(())
                    }
                    Err(error) => self.fail(disk("the image's volume")(error)),
                }
            }
            (Member::Root | Member::Boot, _) => {
                let path = self.work.join(name);
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                match File::create(&path) {
                    Ok(file) => {
                        self.current = Some((
                            what,
                            Some(Writing {
                                file,
                                hasher: Sha256::new(),
                                name: name.to_string(),
                                unsynced: 0,
                            }),
                        ));
                        Ok(())
                    }
                    Err(error) => self.fail(disk("the work directory")(error)),
                }
            }
        }
    }

    fn data(&mut self, bytes: &[u8]) -> Result<(), String> {
        match &mut self.current {
            Some((Member::Manifest, _)) => self.manifest.extend_from_slice(bytes),
            Some((Member::Signature, _)) => self.signature.extend_from_slice(bytes),
            Some((_, Some(writing))) => {
                writing.hasher.update(bytes);
                if let Err(error) = writing.file.write_all(bytes) {
                    return self.fail(disk("the work directory")(error));
                }
                // Flushed as it comes, so no one call waits on a whole image reaching the disk:
                // a call has the host's deadline (2 s by default).
                writing.unsynced += bytes.len() as u64;
                if writing.unsynced >= SYNC_EVERY {
                    writing.unsynced = 0;
                    if let Err(error) = writing.file.sync_data() {
                        return self.fail(disk("the work directory")(error));
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn end(&mut self) -> Result<(), String> {
        match self.current.take() {
            Some((Member::Manifest, _)) => {
                let parsed = match core::str::from_utf8(&self.manifest) {
                    Ok(text) => Manifest::parse(text).map_err(bad),
                    Err(_) => Err(bad("slot.json isn't text")),
                };
                let parsed = match parsed {
                    Ok(parsed) => parsed,
                    Err(failure) => return self.fail(failure),
                };
                if let Err(failure) = self.guard.check(&parsed.id) {
                    return self.fail(failure);
                }
                self.parsed = Some(parsed);
            }
            Some((Member::Signature, _)) => {
                // The signature is complete: decided now, before any payload member begins.
                if let Err(failure) = self.settle() {
                    return self.fail(failure);
                }
            }
            Some((_, Some(writing))) => {
                if let Err(error) = writing.file.sync_all() {
                    return self.fail(disk("the work directory")(error));
                }
                let sha = hex(&writing.hasher.finalize());
                self.hashes.push((writing.name, sha));
            }
            _ => {}
        }
        Ok(())
    }
}

/// What `slot.sig` shows, as the shared policy takes it: the signature record names its key by
/// id (SHA-256 of the public key). A key the board holds is checked; any other is unknown. The
/// signed message is `lemnos_slot_store::signing::slot_bundle_message` over the manifest's bytes.
pub fn signature_check<'a>(keys: &'a [TrustedKey], manifest: &[u8], signature: &[u8]) -> Check<'a> {
    if signature.is_empty() {
        return Check::Unsigned;
    }
    let record = match signing::parse_record(signature) {
        Ok(record) => record,
        Err(why) => return Check::Malformed(format!("slot.sig: {why}")),
    };
    let Some(trusted) = keys
        .iter()
        .find(|t| signing::key_id(t.key.as_bytes()) == record.key_id)
    else {
        return Check::UnknownKey;
    };
    let signature = Signature::from_bytes(&record.signature);
    let message = signing::slot_bundle_message(manifest);
    if trusted.key.verify_strict(&message, &signature).is_ok() {
        Check::Verified {
            owner: &trusted.owner,
        }
    } else {
        Check::BadSignature {
            owner: &trusted.owner,
        }
    }
}

/// The board's keys and policy an install is admitted by. Given when the install starts, so the
/// signature is decided as the bundle comes, before any payload byte is written.
#[derive(Debug, Clone, Default)]
pub struct Admit {
    /// The keys the board trusts.
    pub keys: Vec<TrustedKey>,
    /// Refuse a bundle the signature table refuses (unsigned, or signed by a key the board does
    /// not trust). Off, such a bundle installs and its slot shows as untrusted or unsigned.
    pub require_signed: bool,
    /// The board file's `[board] id`: a slot built for another board is refused.
    pub board_id: String,
}

/// What finishing needs from the board.
pub struct Finish<'a> {
    pub layout: &'a Layout,
    pub boot_args: &'a str,
    /// The board's `boot_sources` entries (`source=path`): the bundle's command line must carry
    /// `<boot_args>.src.<entry>` for each.
    pub boot_sources: &'a [String],
    pub guard: Guard,
    /// The board's boot record, which may add its own files to the slot's boot directory.
    pub record: &'a dyn crate::boot::BootRecord,
}

/// One install, from its first byte to its slot in place.
pub struct Install {
    pub owner: CallerId,
    pub job: JobId,
    pub size: u64,
    pub got: u64,
    pub pin: bool,
    pub keep_old: bool,
    tar: Tar,
    unpack: Unpack,
    /// How it ended: the slot's id, or why not.
    pub end: Option<Result<String, Failure>>,
    /// The signature policy's result for this bundle, once it was decided (at the end of slot.sig,
    /// or at the first payload member, or at `finish`): the outcome, or the refusal's text.
    pub signed: Option<Result<Outcome, String>>,
    /// The bundle's file in `.incoming`, for an install from a file: read on the driver's ticks,
    /// and deleted when the install ends.
    pub source: Option<Source>,
}

/// A bundle in `.incoming`, open.
pub struct Source {
    pub file: File,
    pub path: PathBuf,
}

/// The directory a sender puts a bundle in for `install_file`, under the images directory.
pub const INCOMING: &str = ".incoming";
/// How much of a file one tick reads: small enough that a tick stays far inside a call's
/// deadline.
pub const READ_EVERY_TICK: usize = 1 << 20;

/// Whether `name` may name a file in `.incoming`: one path component of `[A-Za-z0-9._-]`, not
/// `.` or `..`.
pub fn valid_incoming(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name.len() <= 255
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Opens `name` in `incoming` for an install: never through a link, and only a regular file.
pub fn open_incoming(incoming: &Path, name: &str) -> Result<(Source, u64), Status> {
    use std::os::unix::fs::OpenOptionsExt;
    if !valid_incoming(name) {
        return Err(Status::INVALID_ARGUMENT);
    }
    let path = incoming.join(name);
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&path)
        .map_err(|error| match error.raw_os_error() {
            Some(libc::ENOENT) => Status::NOT_FOUND,
            // ELOOP: the name is a link.
            Some(libc::ELOOP) => Status::INVALID_ARGUMENT,
            _ => Status::from_io_error(&error),
        })?;
    let meta = file.metadata().map_err(|e| Status::from_io_error(&e))?;
    if !meta.is_file() {
        return Err(Status::INVALID_ARGUMENT);
    }
    Ok((Source { file, path }, meta.len()))
}

/// Clears `.incoming` and makes sure it is there: mode 0770, group `lemnos` when the system
/// has one, so a sender in that group can write into it.
pub fn reset_incoming(layout: &Layout) -> io::Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let dir = layout.images().join(INCOMING);
    fs::create_dir_all(&dir)?;
    for entry in fs::read_dir(&dir)?.flatten() {
        let path = entry.path();
        let _ = if entry.file_type().is_ok_and(|t| t.is_dir()) {
            fs::remove_dir_all(&path)
        } else {
            fs::remove_file(&path)
        };
    }
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o770))?;
    if let Some(gid) = group_id(
        &fs::read_to_string("/etc/group").unwrap_or_default(),
        "lemnos",
    ) {
        // Left as it is when the service may not change it: the sender then needs another way in.
        let _ = std::os::unix::fs::chown(&dir, None, Some(gid));
    }
    Ok(dir)
}

/// A group's id from `/etc/group`'s text.
pub fn group_id(text: &str, name: &str) -> Option<u32> {
    text.lines().find_map(|line| {
        let mut fields = line.split(':');
        (fields.next()? == name).then_some(())?;
        fields.nth(1)?.parse().ok()
    })
}

impl Install {
    /// Starts one into a new work directory in the images directory.
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        layout: &Layout,
        owner: CallerId,
        job: JobId,
        size: u64,
        pin: bool,
        keep_old: bool,
        guard: Guard,
        admit: Admit,
        writer: Option<Arc<dyn UbiWrite>>,
    ) -> Result<Self, Failure> {
        let work = layout.images().join(format!("{WORK_PREFIX}{}", job.0));
        let volume = writer.map(|writer| WorkVolume {
            writer,
            name: ubi::work_volume(job.0),
        });
        if let (Some(volume), Some(device)) = (&volume, &layout.ubi)
            && device.volume(&volume.name).is_some()
        {
            volume
                .writer
                .remove(&volume.name)
                .map_err(disk("the work volume"))?;
        }
        let _ = fs::remove_dir_all(&work);
        fs::create_dir_all(&work).map_err(disk("the work directory"))?;
        Ok(Self {
            owner,
            job,
            size,
            got: 0,
            pin,
            keep_old,
            tar: Tar::default(),
            unpack: Unpack {
                work,
                volume,
                guard,
                admit,
                manifest: Vec::new(),
                signature: Vec::new(),
                parsed: None,
                entries: Vec::new(),
                decided: None,
                signed: None,
                current: None,
                hashes: Vec::new(),
                failure: None,
            },
            end: None,
            signed: None,
            source: None,
        })
    }

    /// Takes the next bytes. An error ends the install; the caller sets `end`.
    pub fn take(&mut self, offset: u64, bytes: &[u8]) -> Result<(), Failure> {
        if offset != self.got {
            return Err(bad(format!(
                "bytes at {offset}, where the install is at {}",
                self.got
            )));
        }
        let after = self.got.saturating_add(bytes.len() as u64);
        if after > self.size {
            return Err(bad(format!("more than the {} bytes announced", self.size)));
        }
        let fed = self.tar.feed(bytes, &mut self.unpack);
        self.signed = self.unpack.signed.clone();
        if let Err(text) = fed {
            return Err(self.unpack.failure.take().unwrap_or_else(|| bad(text)));
        }
        self.got = after;
        Ok(())
    }

    /// Whether every byte has come.
    pub fn complete(&self) -> bool {
        self.got == self.size
    }

    /// Checks what came against the signed manifest and puts the slot in place. Returns its id.
    pub fn finish(&mut self, board: &Finish<'_>) -> Result<String, Failure> {
        if !self.tar.finished() {
            return Err(bad("the bundle ends in the middle"));
        }
        if self.unpack.manifest.is_empty() {
            return Err(bad("not a slot bundle: no slot.json"));
        }
        // The signature and the board were decided as the bundle came (before any payload byte);
        // a bundle with no payload is decided here, as unsigned or by its slot.sig.
        let decided = self.unpack.settle();
        self.signed = self.unpack.signed.clone();
        let outcome = decided?;
        let unpack = &self.unpack;
        let manifest = unpack
            .parsed
            .clone()
            .ok_or_else(|| bad("slot.json isn't a slot manifest"))?;
        let id = manifest.id.clone();
        board.guard.check(&id)?;
        let root_hash = manifest.verity.root_hash.as_str();
        let root = unpack.work.join("rootfs.erofs");
        let size = match (&unpack.volume, &board.layout.ubi) {
            (Some(volume), Some(device)) => device.volume(&volume.name).map(|v| v.bytes),
            _ => fs::metadata(&root).ok().map(|m| m.len()),
        }
        .filter(|_| unpack.hashes.iter().any(|(n, _)| n == "rootfs.erofs"));
        bundle::check_root(&manifest, size, &unpack.hashes).map_err(bad)?;
        bundle::check_boot(&manifest, &unpack.hashes).map_err(bad)?;
        let boot_dir = unpack.work.join("boot");
        let args = board.boot_args;
        if args.is_empty() || !args.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            return Err((
                Status::NO_DEVICE,
                "the board file names no boot_args for the slot's initramfs".into(),
            ));
        }
        // The command line that boots is the bundle's, byte for byte: its hash is in the signed
        // manifest's boot files, so nothing is written into it here. The bundle names its own slot
        // and root hash; a bundle whose line does not is refused.
        let line = fs::read_to_string(boot_dir.join("cmdline.txt"))
            .map_err(|_| bad("the bundle has no boot/cmdline.txt"))?;
        let cmdline = bundle::cmdline_line(&line).to_string();
        bundle::check_cmdline(args, &id, root_hash, board.boot_sources, &cmdline).map_err(bad)?;
        // The boot side, kept on the data partition. A slot whose directory is already on the
        // boot partition (installed again) gets its new files there now.
        let io = disk("the slot's boot files");
        fs::write(boot_dir.join("slot.json"), &unpack.manifest).map_err(&io)?;
        let readme = boot_dir.join("overlays/README");
        if boot_dir.join("overlays").is_dir() && !readme.exists() {
            fs::write(
                readme,
                "Overlays for this slot. The firmware reads them from os_prefix only when this file exists.\n",
            )
            .map_err(&io)?;
        }
        board
            .record
            .prepare(&boot_dir, &id, &cmdline)
            .map_err(|error| bad(format!("the boot loader's files for this slot: {error}")))?;
        // The payloads go into a free bay of the boot partition, refusals first (a payload missing or
        // larger than its bay is refused by name before any write); no pointer moves. Then the copy
        // kept on the data partition, by content.
        let mut stored = Vec::with_capacity(lemnos_slot_store::bays::ROLES.len());
        for role in lemnos_slot_store::bays::ROLES.iter() {
            let bytes = fs::read(boot_dir.join(role.path))
                .map_err(|_| bad(format!("the bundle has no boot/{}", role.path)))?;
            stored.push(bytes);
        }
        let payloads: [&[u8]; 5] = std::array::from_fn(|i| stored[i].as_slice());
        board
            .record
            .install(&id, &payloads)
            .map_err(|error| bad(format!("slot {id}'s boot files: {error}")))?;
        let layout = board.layout;
        boot::put(layout, &id, &boot_dir).map_err(&io)?;
        // The data side: the image, then the manifest, each by rename.
        let images = layout.images();
        let manifest_tmp = images.join(format!(".{id}.json.tmp"));
        fs::write(&manifest_tmp, &unpack.manifest).map_err(&io)?;
        File::open(&manifest_tmp)
            .and_then(|f| f.sync_all())
            .map_err(&io)?;
        match &unpack.volume {
            Some(volume) => volume
                .writer
                .rename(&volume.name, &ubi::image_volume(&id))
                .map_err(disk("the image's volume"))?,
            None => fs::rename(&root, layout.image(&id)).map_err(&io)?,
        }
        crate::store::sync_dir(&images).map_err(&io)?;
        fs::rename(&manifest_tmp, layout.manifest(&id)).map_err(&io)?;
        crate::store::sync_dir(&images).map_err(&io)?;
        let _ = fs::remove_dir_all(&unpack.work);
        let mut meta = Meta::default();
        meta.set("PINNED", if self.pin { "yes" } else { "no" });
        meta.set("GOOD", "untried");
        // `SIGNATURE` is the outcome word; `SIGNED` keeps the label older boards and readers use.
        meta.set("SIGNATURE", outcome.word());
        meta.set("SIGNED", outcome.legacy_word());
        meta.set("SIGNER", outcome.signer());
        meta.set("INSTALLED", &crate::store::now_utc());
        let seq = crate::store::next_seq(layout).map_err(&io)?;
        meta.set("SEQ", &seq.to_string());
        meta.save(layout, &id).map_err(&io)?;
        Ok(id)
    }

    /// Removes what came so far.
    pub fn discard(&mut self) {
        let _ = fs::remove_dir_all(&self.unpack.work);
        if let Some(volume) = &self.unpack.volume {
            let _ = volume.writer.remove(&volume.name);
        }
        self.drop_source();
    }

    /// Deletes the bundle's file in `.incoming`, if the install came from one.
    pub fn drop_source(&mut self) {
        if let Some(source) = self.source.take() {
            let _ = fs::remove_file(&source.path);
        }
    }

    /// Reads the next piece of the source file into `buf`: how much.
    pub fn pull(&mut self, buf: &mut [u8]) -> Result<usize, Failure> {
        use std::io::Read;
        let Some(source) = self.source.as_mut() else {
            return Ok(0);
        };
        let want = usize::try_from(self.size - self.got)
            .unwrap_or(usize::MAX)
            .min(buf.len());
        let mut read = 0;
        while read < want {
            match source.file.read(&mut buf[read..want]) {
                Ok(0) => break,
                Ok(n) => read += n,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(disk("the bundle's file")(error)),
            }
        }
        if read == 0 && want > 0 {
            return Err(bad(format!(
                "the bundle's file ended at {} of its {} bytes",
                self.got, self.size
            )));
        }
        Ok(read)
    }
}

/// Work an install left when the service stopped in the middle: removed. Whether there was any.
pub fn clear_leftovers(layout: &Layout, writer: Option<&dyn UbiWrite>) -> bool {
    let mut found = false;
    if let (Some(device), Some(writer)) = (&layout.ubi, writer) {
        for volume in device.volumes() {
            if volume.name.starts_with(ubi::WORK_PREFIX) {
                found = true;
                let _ = writer.remove(&volume.name);
            }
        }
    }
    for entry in fs::read_dir(layout.images())
        .into_iter()
        .flatten()
        .flatten()
    {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(WORK_PREFIX) || (name.starts_with('.') && name.ends_with(".tmp")) {
            found = true;
            let _ = if entry.path().is_dir() {
                fs::remove_dir_all(entry.path())
            } else {
                fs::remove_file(entry.path())
            };
        }
    }
    found
}
