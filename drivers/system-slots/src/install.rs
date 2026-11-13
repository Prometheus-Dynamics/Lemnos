//! An install: a bundle streamed in, unpacked into a work directory as it comes, checked against
//! its signed manifest, and moved into place.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use ed25519_dalek::{Signature, VerifyingKey};
use lemnos_core::{CallerId, JobId, Status};
use sha2::{Digest, Sha256};

use crate::boot::{self, hex};
use crate::json::Json;
use crate::manifest::Manifest;
use crate::store::{Layout, Meta, valid_id};
use crate::tar::{Kind, Sink, Tar};

/// The largest manifest taken.
const MAX_MANIFEST: u64 = 64 * 1024;
/// An Ed25519 signature's length.
pub const SIGNATURE_LEN: usize = 64;
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

/// How a slot's manifest was signed: by a key the board trusts (and whose), by another key, or
/// not at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Signed {
    Trusted(String),
    Untrusted,
    Unsigned,
}

impl Signed {
    /// The word `list` and the slot's state show.
    pub fn word(&self) -> &'static str {
        match self {
            Self::Trusted(_) => "trusted",
            Self::Untrusted => "untrusted",
            Self::Unsigned => "unsigned",
        }
    }
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

/// What a bundle may hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Member {
    Manifest,
    Signature,
    Root,
    Boot,
}

fn member(name: &str) -> Option<Member> {
    match name {
        "slot.json" => Some(Member::Manifest),
        "slot.sig" => Some(Member::Signature),
        "rootfs.erofs" => Some(Member::Root),
        name if name.starts_with("boot/") => Some(Member::Boot),
        _ => None,
    }
}

/// The slots a bundle may not replace.
#[derive(Debug, Clone, Default)]
pub struct Guard {
    pub running: String,
    pub default: String,
}

impl Guard {
    fn check(&self, id: &str) -> Result<(), Failure> {
        if !valid_id(id) {
            return Err(bad(format!("slot.json: `{id}` isn't a slot id")));
        }
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

/// The tar's members, into the work directory.
struct Unpack {
    work: PathBuf,
    guard: Guard,
    manifest: Vec<u8>,
    signature: Vec<u8>,
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
}

impl Sink for Unpack {
    fn begin(&mut self, name: &str, kind: Kind, size: u64) -> Result<(), String> {
        if name == "boot" && kind == Kind::Dir {
            self.current = Some((Member::Boot, None));
            return fs::create_dir_all(self.work.join("boot")).map_err(|e| e.to_string());
        }
        let Some(what) = member(name) else {
            return self.fail(bad(format!(
                "the bundle has {name}: only slot.json, slot.sig, rootfs.erofs and boot/"
            )));
        };
        if self.hashes.iter().any(|(n, _)| n == name) {
            return self.fail(bad(format!("the bundle has {name} twice")));
        }
        match (what, kind) {
            (Member::Boot, Kind::Dir) => {
                self.current = Some((what, None));
                fs::create_dir_all(self.work.join(name)).map_err(|e| e.to_string())
            }
            (_, Kind::Dir) => self.fail(bad(format!("{name} is a directory in the bundle"))),
            (Member::Manifest, _) if size > MAX_MANIFEST => {
                self.fail(bad("slot.json is too large"))
            }
            (Member::Signature, _) if size != SIGNATURE_LEN as u64 => {
                self.fail(bad("slot.sig is not one Ed25519 signature"))
            }
            (Member::Manifest | Member::Signature, _) => {
                self.current = Some((what, None));
                Ok(())
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
                // Refused as soon as it says what it is, not after the image has come.
                let id = core::str::from_utf8(&self.manifest)
                    .ok()
                    .and_then(Json::parse)
                    .and_then(|json| json.get("id").and_then(Json::text).map(str::to_string));
                let Some(id) = id else {
                    return self.fail(bad("slot.json isn't a slot manifest"));
                };
                if let Err(failure) = self.guard.check(&id) {
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

/// What finishing needs from the board.
pub struct Finish<'a> {
    pub layout: &'a Layout,
    pub keys: &'a [TrustedKey],
    /// Refuse a bundle not signed by a trusted key. Off, such a bundle installs and its slot
    /// shows as untrusted or unsigned.
    pub require_signed: bool,
    pub partuuid: &'a str,
    pub boot_args: &'a str,
    /// The board file's `[board] id`: a slot built for another board is refused.
    pub board_id: &'a str,
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
    ) -> Result<Self, Failure> {
        let work = layout.images().join(format!("{WORK_PREFIX}{}", job.0));
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
                guard,
                manifest: Vec::new(),
                signature: Vec::new(),
                current: None,
                hashes: Vec::new(),
                failure: None,
            },
            end: None,
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
        if let Err(text) = self.tar.feed(bytes, &mut self.unpack) {
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
        let unpack = &self.unpack;
        if unpack.manifest.is_empty() {
            return Err(bad("not a slot bundle: no slot.json"));
        }
        // A signature that is there is always checked, and what it showed is recorded.
        let signed = if unpack.signature.len() == SIGNATURE_LEN {
            let signature = Signature::from_bytes(
                &<[u8; SIGNATURE_LEN]>::try_from(unpack.signature.as_slice())
                    .map_err(|_| bad("slot.sig"))?,
            );
            board
                .keys
                .iter()
                .find(|trusted| {
                    trusted
                        .key
                        .verify_strict(&unpack.manifest, &signature)
                        .is_ok()
                })
                .map_or(Signed::Untrusted, |trusted| {
                    Signed::Trusted(trusted.owner.clone())
                })
        } else {
            Signed::Unsigned
        };
        if board.require_signed {
            match signed {
                Signed::Trusted(_) => {}
                Signed::Untrusted => {
                    return Err((
                        Status::NOT_PERMITTED,
                        "slot.json is not signed by a key this board trusts".into(),
                    ));
                }
                Signed::Unsigned => {
                    return Err((
                        Status::NOT_PERMITTED,
                        "the bundle is not signed (no slot.sig), and this board takes only signed ones".into(),
                    ));
                }
            }
        }
        // From here the manifest says what everything else must be.
        let text =
            core::str::from_utf8(&unpack.manifest).map_err(|_| bad("slot.json isn't text"))?;
        let manifest = Manifest::parse(text).map_err(bad)?;
        if board.board_id.is_empty() {
            return Err((
                Status::NO_DEVICE,
                "the board file has no [board] id to match a slot's board against".into(),
            ));
        }
        if manifest.board != board.board_id {
            return Err((
                Status::NOT_SUPPORTED,
                format!(
                    "this slot is built for board {}, and this is board {}",
                    manifest.board, board.board_id
                ),
            ));
        }
        let id = manifest.id.clone();
        board.guard.check(&id)?;
        let root_hash = manifest.verity.root_hash.as_str();
        let image_size = manifest.image_size;
        let image_sha = manifest.image_sha256.as_str();
        let hash_of = |name: &str| {
            unpack
                .hashes
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, sha)| sha.as_str())
        };
        let root = unpack.work.join("rootfs.erofs");
        let size = fs::metadata(&root)
            .map_err(|_| bad("not a slot bundle: no rootfs.erofs"))?
            .len();
        if size != image_size {
            return Err(bad("rootfs.erofs isn't the size slot.json says"));
        }
        if hash_of("rootfs.erofs") != Some(image_sha) {
            return Err(bad("rootfs.erofs doesn't match slot.json's sha256"));
        }
        let listed = &manifest.boot_files;
        for (name, want) in listed {
            match hash_of(&format!("boot/{name}")) {
                Some(got) if got == want => {}
                None => return Err(bad(format!("the bundle has no boot file {name}"))),
                _ => return Err(bad(format!("boot file {name} doesn't match slot.json"))),
            }
        }
        for (name, _) in &unpack.hashes {
            if let Some(file) = name.strip_prefix("boot/")
                && !listed.iter().any(|(listed, _)| listed == file)
            {
                return Err(bad(format!("boot file {file} isn't in slot.json")));
            }
        }
        let boot_dir = unpack.work.join("boot");
        let base = fs::read_to_string(boot_dir.join("cmdline.txt"))
            .map_err(|_| bad("the bundle has no boot/cmdline.txt"))?;
        let base = base.split_whitespace().collect::<Vec<_>>().join(" ");
        if board.partuuid.is_empty() {
            return Err((
                Status::NO_DEVICE,
                "the data partition's PARTUUID isn't known".into(),
            ));
        }
        let args = board.boot_args;
        if args.is_empty() || !args.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            return Err((
                Status::NO_DEVICE,
                "the board file names no boot_args for the slot's initramfs".into(),
            ));
        }
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
        let cmdline = format!(
            "{base} {args}.slot={id} {args}.roothash={root_hash} {args}.data=PARTUUID={} panic=5",
            board.partuuid
        );
        fs::write(boot_dir.join("cmdline.txt"), format!("{cmdline}\n")).map_err(&io)?;
        board
            .record
            .prepare(&boot_dir, &id, &cmdline)
            .map_err(|error| bad(format!("the boot loader's files for this slot: {error}")))?;
        let layout = board.layout;
        boot::put(layout, &id, &boot_dir).map_err(&io)?;
        if layout.boot_dir(&id).is_dir() {
            boot::stage(layout, &id, true).map_err(|text| (Status::IO, text))?;
        }
        // The data side: the image, then the manifest, each by rename.
        let images = layout.images();
        let manifest_tmp = images.join(format!(".{id}.json.tmp"));
        fs::write(&manifest_tmp, &unpack.manifest).map_err(&io)?;
        File::open(&manifest_tmp)
            .and_then(|f| f.sync_all())
            .map_err(&io)?;
        fs::rename(&root, layout.image(&id)).map_err(&io)?;
        crate::store::sync_dir(&images).map_err(&io)?;
        fs::rename(&manifest_tmp, layout.manifest(&id)).map_err(&io)?;
        crate::store::sync_dir(&images).map_err(&io)?;
        let _ = fs::remove_dir_all(&unpack.work);
        let mut meta = Meta::default();
        meta.set("PINNED", if self.pin { "yes" } else { "no" });
        meta.set("GOOD", "untried");
        meta.set("SIGNED", signed.word());
        meta.set(
            "SIGNER",
            match &signed {
                Signed::Trusted(owner) => owner,
                _ => "",
            },
        );
        meta.set("INSTALLED", &crate::store::now_utc());
        let seq = crate::store::next_seq(layout).map_err(&io)?;
        meta.set("SEQ", &seq.to_string());
        meta.save(layout, &id).map_err(&io)?;
        Ok(id)
    }

    /// Removes what came so far.
    pub fn discard(&mut self) {
        let _ = fs::remove_dir_all(&self.unpack.work);
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
pub fn clear_leftovers(layout: &Layout) -> bool {
    let mut found = false;
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
