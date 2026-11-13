//! The first-flash write (`system.flash/1`): a whole disk image, streamed to a board's storage.
//!
//! A flasher's board may be blank: no partition table, no data partition, no slot. So this driver
//! does not write into a slot. It writes an image the sender streams in, from the target's start,
//! and the image defines what the target then holds: its partitions, its one slot, its boot record.
//! The driver invents none of them, and it never restarts the board: the flasher does that next.
//!
//! # Wiring and settings
//!
//! The board file names the one target (`path`). It is the only place a path comes from: no method
//! takes one. Settings, from the device's `config`:
//!
//! - `path` (required): the target, a block device such as `/dev/mmcblk0`.
//! - `kind`: `"block"` by default; `"file"` for a regular file, for tests and dev boards only;
//!   `"mtd"` for a raw NAND partition (`/dev/mtdN`), written in erase blocks (see `nand`).
//! - `image` (mtd only): `"raw"` by default, a linear partition image, or `"ubi"`, a UBI image of
//!   whole erase blocks. The board's format string is then `mtd-raw` or `mtd-ubi`.
//! - `mark_bad` (mtd only): `false` by default; when true, an erase or write failure marks its block bad.
//! - `sysfs` (mtd only): the sysfs root (`/sys`), and `erase_size`, `write_size`, `subpage_size`,
//!   `bad_blocks` (a file of erase-block indices) and `mtd_index` are test overrides for a regular-file target.
//! - `allow_first_flash`: `false` by default. A write is refused unless it is `true`. Only a
//!   flasher's board file sets it.
//! - `max_dict_mb`: 8 by default (1 to 8, the ceiling): the largest xz dictionary a stream may
//!   declare. A board file may lower it, never raise it.
//! - `keys`: the trusted keys file (one per line: 64 hex digits, a space, the owner).
//!   The security switch (`lemnos_slot_store::security`, `docs/slots.md`) sets the signature
//!   policy, read at each decision. The view is not a board-file setting: a board file that names
//!   `security_view` or `require_signed` is refused.
//!   The board file's `require_signed` is refused by name. The policy is the table in the brief
//!   (`manifest::decide`): with the switch off, unsigned and unknown-key images are accepted and reported
//!   as not checked; a bad signature from a known key is refused either way.
//! - `mountinfo`: the mount table the running-system check reads, `/proc/self/mountinfo` by default.
//!
//! The board's id (`[board] id`) is given to the driver; a manifest for another board is refused.
//! The format of a block target is `disk`: the signed message is built from the board's id, that
//! format, the image size and its SHA-256, never from the JSON.
//!
//! # Calls
//!
//! `manifest` accepts a manifest and its signature record. `write(size, format)` starts a job that takes
//! `size` bytes of stream through `data` in order, decodes them (xz, or raw), writes the decoded
//! image to the target from byte 0, hashes it and checks the hash, syncs the target and asks the
//! kernel to read its partition table again. `verify` reads the target back and checks the hash.
//!
//! # Failure
//!
//! A write that fails ends its job with the reason in `info`'s `error`, and the target is left as
//! it is: a partial image, which must not be booted. A cancelled write does the same. Nothing is
//! kept across a restart: a restart ends a write, and the next write starts again from byte 0.
//!
//! # Time
//!
//! The driver is ticked after each call. A verify reads one chunk per tick and asks for another
//! tick while it has more to read; nothing else is timed.

pub mod crc;
pub mod lzma;
pub mod manifest;
pub mod nand;
pub mod target;
pub mod xz;

use std::fs::{self, File};
use std::io;
use std::os::unix::fs::FileExt;

use lemnos_contract_indicator::{Indicator as IndicatorContract, NO_PROGRESS};
use lemnos_contract_security_precondition::SecurityPrecondition;
use lemnos_contract_system_flash::{FORMAT_RAW, FORMAT_XZ, Flash};
use lemnos_core::{
    CallerId, Config, ConfigError, Dev, Health, JobId, JobState, Key, Kind as SettingKind,
    Lifecycle, Reply, Result, Status, driver,
};
use lemnos_slot_store::json::quoted;
use sha2::{Digest, Sha256};

use manifest::{Manifest, Outcome, RECORD_LEN, TrustedKey};
use nand::{Chip, Geometry, Image};
use target::{Found, Kind};
use xz::Decoder;

/// The bytes a verify reads in one tick.
const CHUNK: usize = 64 * 1024;
/// `ENOSPC`: the target is too small, or full.
const NO_SPACE: Status = Status::from_errno(28);
/// The sentence for a file that is not an image with an optional record after it.
const NOT_AN_IMAGE: &str = "not a first-flash image";

/// A manifest the driver accepted, its signature record (empty when unsigned), and how the
/// signature policy decided it.
#[derive(Debug, Clone)]
struct Accepted {
    manifest: Manifest,
    record: Vec<u8>,
    outcome: Outcome,
}

/// A write, or a verify: the job's work.
enum Work {
    Write(Box<Writer>),
    Verify(Checker),
    Nand(Box<nand::Verifier>),
}

/// The raw NAND settings of a `kind = "mtd"` target.
#[derive(Debug, Clone)]
struct Mtd {
    image: Image,
    mark_bad: bool,
    sysfs: std::path::PathBuf,
    /// The test overrides, when the target is a regular file standing in for the partition.
    fake: Option<FakeNand>,
}

/// A stand-in partition: its geometry and bad blocks are given, not read from sysfs.
#[derive(Debug, Clone)]
struct FakeNand {
    erase: u64,
    write: u64,
    subpage: u64,
    bad: Option<std::path::PathBuf>,
    index: Option<u32>,
}

/// Where a write's bytes go: the target file, or a NAND partition's sink.
enum Out {
    File(File),
    Nand(Box<nand::Sink>),
}

/// A write that finished: its byte count, the hash its target must hold (the image's own, or for
/// UBI the hash of the image as written, with the erase counts carried), and the ECC corrections.
struct Finished {
    bytes: u64,
    expect: [u8; 32],
    corrected: u64,
}

/// The image a write finished with, and the hash `verify` checks the target against.
#[derive(Debug, Clone)]
struct Written {
    manifest: Manifest,
    expect: [u8; 32],
}

/// The write job's state: the decoder (for xz), the target and the hash of what is written.
struct Writer {
    decoder: Option<Decoder>,
    out: Out,
    manifest: Manifest,
    written: u64,
    hasher: Sha256,
    /// The sentence for a refusal from the sink, which the decoder reports only as a status.
    refused: Option<String>,
    /// The record the manifest gave: empty, or the record that must be the file's second stream.
    expect: Vec<u8>,
    /// The record the file's second stream decoded to, so far.
    record: Vec<u8>,
    /// The xz streams the file held, once the decoder has read them (0 for a raw image).
    streams: u32,
}

impl Writer {
    /// Takes decoded bytes from the decoder: stream 0 is the image, stream 1 is the file's record.
    fn sink(&mut self, stream: u32, bytes: &[u8]) -> std::result::Result<(), Status> {
        if stream == 0 {
            return self.emit(bytes);
        }
        if self.record.len() + bytes.len() > RECORD_LEN {
            self.refused = Some(NOT_AN_IMAGE.into());
            return Err(Status::INVALID_ARGUMENT);
        }
        self.record.extend_from_slice(bytes);
        Ok(())
    }

    /// The file's own record, against the one the manifest was given. A raw image has no second
    /// stream, so there is nothing to compare; an xz file's record must be the one given, and an
    /// empty record given means the file must have none.
    fn check_record(&self) -> std::result::Result<(), (Status, String)> {
        let mismatch = || {
            (
                Status::INVALID_ARGUMENT,
                "the image's trailing signature is not the one given".to_string(),
            )
        };
        match self.streams {
            0 => Ok(()),
            1 if self.expect.is_empty() => Ok(()),
            1 => Err(mismatch()),
            _ => {
                if self.record.len() != RECORD_LEN || manifest::parse_record(&self.record).is_err()
                {
                    return Err((Status::INVALID_ARGUMENT, NOT_AN_IMAGE.into()));
                }
                if self.record != self.expect {
                    return Err(mismatch());
                }
                Ok(())
            }
        }
    }

    /// Writes decoded bytes at the end of what is written. A refusal sets `refused`.
    fn emit(&mut self, bytes: &[u8]) -> std::result::Result<(), Status> {
        if self.written + bytes.len() as u64 > self.manifest.image_size {
            self.refused = Some("the stream holds more than the manifest's image size".into());
            return Err(Status::INVALID_ARGUMENT);
        }
        match &mut self.out {
            Out::File(file) => {
                if let Err(error) = file.write_all_at(bytes, self.written) {
                    let (status, sentence) = io_failure(&error, "the target can't be written");
                    self.refused = Some(sentence);
                    return Err(status);
                }
            }
            Out::Nand(sink) => {
                if let Err((status, sentence)) = sink.push(bytes) {
                    self.refused = Some(sentence);
                    return Err(status);
                }
            }
        }
        self.hasher.update(bytes);
        self.written += bytes.len() as u64;
        Ok(())
    }
}

/// A verify job's state: the target read back in chunks, and the hash of what was read.
struct Checker {
    file: File,
    buf: Vec<u8>,
    read: u64,
    hasher: Sha256,
    expect: [u8; 32],
    image_size: u64,
}

/// A job: a write or a verify, and how far it has got, until it ends.
struct Job {
    id: JobId,
    starter: CallerId,
    work: Work,
    /// The write's signature outcome code, sent after its byte count in the result.
    code: u8,
    /// The stream's bytes (for a write) or the image's (for a verify), and how many are taken.
    size: u64,
    got: u64,
    /// `Ok` with the job's result number, or the failure's status and sentence.
    end: Option<std::result::Result<u64, (Status, String)>>,
}

/// The first-flash driver: one target, one job at a time.
#[driver("system-flash", provides = [Flash, SecurityPrecondition])]
pub struct SystemFlash {
    /// The board's indicator, when it has one: it shows the write's and the verify's states.
    #[needs(optional)]
    indicator: Option<Dev<dyn IndicatorContract>>,
    /// The state last sent to the indicator, so only a change is sent.
    shown: Option<(&'static str, u16)>,
    path: std::path::PathBuf,
    kind: Kind,
    /// The raw NAND settings, when the target is a `mtd` partition.
    mtd: Option<Mtd>,
    /// The board's own format string: `disk`, `mtd-raw` or `mtd-ubi`. The signed message uses it.
    format: &'static str,
    allow_first_flash: bool,
    max_dict: u64,
    board_id: String,
    /// The security switch's view (`lemnos_slot_store::security`), read at each write's decision.
    keys: Vec<TrustedKey>,
    mountinfo: std::path::PathBuf,
    /// The manifest the next write uses.
    manifest: Option<Accepted>,
    /// The image of the last write that finished: what `verify` checks the target against.
    written: Option<Written>,
    /// The ECC corrections counted by the last NAND write or verify.
    corrected: u64,
    /// The last manifest call's outcome (`""` before any call), and its reason.
    last_signed: &'static str,
    last_reason: String,
    /// `idle`, `writing`, `cancelled`, `written`, `failed`, `verifying` or `verified`.
    state: &'static str,
    /// The last failure's sentence, or a note on the last write (the partition re-read).
    error: String,
    job: Option<Job>,
    next_job: u64,
    calling: CallerId,
}

impl SystemFlash {
    /// Refuses a call: the reason is the board's `error`, and nothing else changes.
    fn refuse(&mut self, status: Status, sentence: impl Into<String>) -> Status {
        self.error = sentence.into();
        status
    }

    /// Why the target can't take a write now, or `None` when it can. The running-system check
    /// reads the mount table, for the target's device and, for NAND, its `mtdblock` device and the
    /// UBI volumes attached from it; a table that can't be read refuses the write.
    fn unwritable(&self, found: &Found) -> Option<String> {
        if !self.allow_first_flash {
            return Some(
                "allow_first_flash is not set: only a flasher's board file sets it".into(),
            );
        }
        let table = match fs::read_to_string(&self.mountinfo) {
            Ok(table) => table,
            Err(error) => {
                return Some(format!(
                    "the mount table can't be read, so the running system can't be checked: {error}"
                ));
            }
        };
        let mut devices = vec![target::major_minor(found.dev)];
        if let (Some(mtd), Some(index)) = (self.mtd.as_ref(), self.mtd_index()) {
            devices.extend(target::mtd_devices(&mtd.sysfs, index));
        }
        devices
            .into_iter()
            .find_map(|dev| target::mounted_on(&table, dev, &target::sysfs_disk))
            .map(|mount| format!("the target holds the running system: {mount} is mounted from it"))
    }

    /// The target as it is now, or the reason it isn't there. A test target for NAND is a regular file.
    fn target(&self) -> std::result::Result<Found, String> {
        target::find(&self.path, self.find_kind())
    }

    /// The kind `find` checks the target as: a NAND stand-in is a regular file.
    fn find_kind(&self) -> Kind {
        match &self.mtd {
            Some(Mtd { fake: Some(_), .. }) => Kind::File,
            _ => self.kind,
        }
    }

    /// The MTD index of the partition: from its `/dev/mtdN` path, or the test's `mtd_index`.
    fn mtd_index(&self) -> Option<u32> {
        match &self.mtd.as_ref()?.fake {
            Some(fake) => fake.index,
            None => nand::index_of(&self.path).ok(),
        }
    }

    /// The NAND geometry of the target: from its sysfs node, or the test's overrides.
    fn mtd_geometry(&self, found: &Found) -> std::result::Result<Geometry, String> {
        let mtd = self
            .mtd
            .as_ref()
            .ok_or("the target is not a NAND partition")?;
        match &mtd.fake {
            Some(fake) => Ok(Geometry {
                erase: fake.erase,
                write: fake.write,
                subpage: fake.subpage,
                size: found.size,
                oob: 0,
            }),
            None => nand::sysfs_geometry(&mtd.sysfs, nand::index_of(&self.path)?),
        }
    }

    /// The target opened as a NAND chip: the character device, or a stand-in with its bad blocks.
    fn open_chip(&self, found: &Found, write: bool) -> std::result::Result<Box<dyn Chip>, String> {
        let file = target::open(found, write)
            .map_err(|error| format!("the target can't be opened: {error}"))?;
        let mtd = self
            .mtd
            .as_ref()
            .ok_or("the target is not a NAND partition")?;
        match &mtd.fake {
            None => Ok(Box::new(nand::Device::new(file))),
            Some(fake) => {
                let bad = match &fake.bad {
                    None => Vec::new(),
                    Some(path) => {
                        let text = fs::read_to_string(path)
                            .map_err(|e| format!("the bad blocks file can't be read: {e}"))?;
                        text.split_whitespace()
                            .map(|t| t.parse::<u64>())
                            .collect::<std::result::Result<Vec<_>, _>>()
                            .map_err(|_| {
                                "the bad blocks file is not erase-block indices".to_string()
                            })?
                    }
                };
                Ok(Box::new(nand::Fake::new(file, fake.erase, bad)))
            }
        }
    }

    /// The NAND writer for a write of `image_size` bytes: the plan is made here, before any erase,
    /// and refuses by name.
    fn nand_out(
        &self,
        found: &Found,
        image_size: u64,
    ) -> std::result::Result<Out, (Status, String)> {
        let mtd = self.mtd.as_ref().ok_or((
            Status::INVALID_ARGUMENT,
            "the target is not a NAND partition".to_string(),
        ))?;
        let geometry = self
            .mtd_geometry(found)
            .map_err(|why| (Status::INVALID_ARGUMENT, why))?;
        let mut chip = self
            .open_chip(found, true)
            .map_err(|why| (Status::IO, why))?;
        let plan = nand::Plan::new(chip.as_mut(), geometry, mtd.image, image_size, mtd.mark_bad)?;
        Ok(Out::Nand(Box::new(nand::Sink::new(chip, plan))))
    }

    /// Ends the running job with `end` and records the state.
    fn end_job(&mut self, end: std::result::Result<u64, (Status, String)>, state: &'static str) {
        let shown = if end.is_ok() { "done" } else { "failed" };
        if let Some(job) = self.job.as_mut() {
            if let Err((_, sentence)) = &end {
                self.error = sentence.clone();
            }
            job.end = Some(end);
        }
        self.state = state;
        self.show(shown, NO_PROGRESS);
    }

    /// Sends `state` and its progress to the indicator, unless it is what was sent last. A refused
    /// indication changes nothing else.
    /// Refuses a manifest: the accepted one is dropped, and the outcome and reason are kept for
    /// `info` and the log.
    fn reject(
        &mut self,
        status: Status,
        sentence: impl Into<String>,
        signed: &'static str,
    ) -> Status {
        self.manifest = None;
        let sentence = sentence.into();
        self.last_signed = signed;
        self.last_reason = sentence.clone();
        self.error = sentence;
        status
    }

    fn show(&mut self, state: &'static str, progress: u16) {
        if self.shown == Some((state, progress)) {
            return;
        }
        self.shown = Some((state, progress));
        if let Some(indicator) = self.indicator.as_mut() {
            let _ = indicator.set(state, progress);
        }
    }

    /// The verify's progress, sent to the indicator on each tick while it reads.
    fn show_verify_progress(&mut self) {
        let progress = match self.job.as_ref() {
            Some(job) if job.end.is_none() && matches!(job.work, Work::Verify(_)) => {
                permille(job.got, job.size)
            }
            _ => return,
        };
        self.show("verifying", progress);
    }

    /// Takes a piece of a write's stream: decodes it into the target, or writes it as it is when
    /// the image is raw. A refusal is returned with the sentence to show.
    fn feed(writer: &mut Writer, bytes: &[u8]) -> std::result::Result<(), (Status, String)> {
        let Some(mut decoder) = writer.decoder.take() else {
            return writer
                .emit(bytes)
                .map_err(|status| (status, writer.refused.take().unwrap_or_default()));
        };
        let result = decoder.push(bytes, &mut |stream, b| writer.sink(stream, b));
        writer.decoder = Some(decoder);
        result.map_err(|error| {
            let sentence = match (&error, writer.refused.take()) {
                (xz::Error::Sink(_), Some(sentence)) => sentence,
                _ => error.sentence(),
            };
            (error.status(), sentence)
        })
    }

    /// The stream is all taken: the decoder must have ended cleanly, the image must be the size and
    /// hash the manifest gives, the file's record must be the one given, and the target is finished
    /// (synced, or its last erase blocks written and checked).
    fn finish_write(writer: &mut Writer) -> std::result::Result<Finished, (Status, String)> {
        if let Some(mut decoder) = writer.decoder.take() {
            decoder.finish().map_err(|e| (e.status(), e.sentence()))?;
            writer.streams = decoder.streams();
        }
        writer.check_record()?;
        if writer.written != writer.manifest.image_size {
            return Err((
                Status::INVALID_ARGUMENT,
                format!(
                    "the image is {} bytes; its manifest says {}",
                    writer.written, writer.manifest.image_size
                ),
            ));
        }
        let digest = writer.hasher.clone().finalize();
        if digest.as_slice() != writer.manifest.sha256 {
            return Err((
                Status::INVALID_ARGUMENT,
                "the image's SHA-256 is not the manifest's".to_string(),
            ));
        }
        let bytes = writer.written;
        match &mut writer.out {
            Out::File(file) => {
                file.sync_all()
                    .map_err(|e| io_failure(&e, "the target can't be synced"))?;
                Ok(Finished {
                    bytes,
                    expect: writer.manifest.sha256,
                    corrected: 0,
                })
            }
            Out::Nand(sink) => {
                let (expect, corrected) = sink.finish()?;
                Ok(Finished {
                    bytes,
                    expect,
                    corrected,
                })
            }
        }
    }

    /// The write's last piece is in: the stream and the image are checked (`finish_write`).
    fn finish_job_write(
        &mut self,
    ) -> (
        std::result::Result<Finished, (Status, String)>,
        Option<Manifest>,
    ) {
        let Some(job) = self.job.as_mut() else {
            return (Err((Status::NOT_FOUND, String::new())), None);
        };
        let Work::Write(writer) = &mut job.work else {
            return (Err((Status::NOT_FOUND, String::new())), None);
        };
        let manifest = writer.manifest.clone();
        (Self::finish_write(writer), Some(manifest))
    }

    /// Ends the write with what `finish_job_write` found.
    fn conclude_write(
        &mut self,
        end: std::result::Result<Finished, (Status, String)>,
        manifest: Option<Manifest>,
    ) -> Result<()> {
        match (end, manifest) {
            (Ok(done), Some(manifest)) => {
                self.corrected = done.corrected;
                self.written = Some(Written {
                    manifest,
                    expect: done.expect,
                });
                self.error.clear();
                self.note_partition_table();
                self.end_job(Ok(done.bytes), "written");
                Ok(())
            }
            (Err((status, sentence)), _) => {
                self.end_job(Err((status, sentence)), "failed");
                Err(status)
            }
            (Ok(_), None) => Err(Status::IO),
        }
    }

    /// Advances a verify job by one chunk (or one erase block of a NAND target). True while it has
    /// more to read.
    fn verify_step(&mut self) -> bool {
        // The step is decided under one borrow of the job; how it ends is decided after it, when the
        // job is no longer borrowed.
        enum Step {
            More,
            Done { bytes: u64, corrected: u64 },
            Failed(Status, String),
        }
        let step = {
            let Some(job) = self.job.as_mut() else {
                return false;
            };
            if job.end.is_some() {
                return false;
            }
            let size = job.size;
            match &mut job.work {
                Work::Write(_) => return false,
                Work::Nand(verifier) => match verifier.step() {
                    Ok(true) => {
                        job.got = verifier.progress().0;
                        Step::More
                    }
                    Ok(false) => {
                        job.got = size;
                        Step::Done {
                            bytes: size,
                            corrected: verifier.corrected,
                        }
                    }
                    Err((status, sentence)) => Step::Failed(status, sentence),
                },
                Work::Verify(checker) => {
                    let want = (checker.image_size - checker.read).min(CHUNK as u64) as usize;
                    match checker.file.read_at(&mut checker.buf[..want], checker.read) {
                        Ok(0) => Step::Failed(
                            Status::IO,
                            "the target ends before the image does".to_string(),
                        ),
                        Ok(n) => {
                            checker.hasher.update(&checker.buf[..n]);
                            checker.read += n as u64;
                            job.got = checker.read;
                            if checker.read < checker.image_size {
                                Step::More
                            } else {
                                let digest = checker.hasher.clone().finalize();
                                if digest.as_slice() == checker.expect {
                                    Step::Done {
                                        bytes: checker.image_size,
                                        corrected: 0,
                                    }
                                } else {
                                    Step::Failed(
                                        Status::INVALID_ARGUMENT,
                                        "the target does not hold the image: its SHA-256 is not the manifest's"
                                            .to_string(),
                                    )
                                }
                            }
                        }
                        Err(error) => {
                            let (status, sentence) = io_failure(&error, "the target can't be read");
                            Step::Failed(status, sentence)
                        }
                    }
                }
            }
        };
        match step {
            Step::More => true,
            Step::Done { bytes, corrected } => {
                self.corrected = corrected;
                self.end_job(Ok(bytes), "verified");
                false
            }
            Step::Failed(status, sentence) => {
                self.end_job(Err((status, sentence)), "failed");
                false
            }
        }
    }
}

/// `done` of `total` in per mille, at most 1000; the whole of it when `total` is 0.
fn permille(done: u64, total: u64) -> u16 {
    if total == 0 {
        return 1000;
    }
    (done.saturating_mul(1000) / total).min(1000) as u16
}

/// The status and sentence for an I/O failure: a full device is `ENOSPC`, anything else `IO`.
fn io_failure(error: &io::Error, what: &str) -> (Status, String) {
    if error.raw_os_error() == Some(28) {
        return (NO_SPACE, format!("{what}: the target is full"));
    }
    (Status::IO, format!("{what}: {error}"))
}

/// Every setting the driver reads. `path` is required; the rest have defaults.
const KEYS: &[Key] = &[
    Key::required("path", SettingKind::Str),
    Key::optional("kind", SettingKind::Str),
    Key::optional("image", SettingKind::Str),
    Key::optional("erase_size", SettingKind::Int),
    Key::optional("write_size", SettingKind::Int),
    Key::optional("subpage_size", SettingKind::Int),
    Key::optional("bad_blocks", SettingKind::Str),
    Key::optional("mtd_index", SettingKind::Int),
    Key::optional("mark_bad", SettingKind::Bool),
    Key::optional("sysfs", SettingKind::Str),
    Key::optional("max_dict_mb", SettingKind::Int),
    Key::optional("keys", SettingKind::Str),
    Key::optional("allow_first_flash", SettingKind::Bool),
    Key::optional("require_signed", SettingKind::Bool),
    Key::optional("mountinfo", SettingKind::Str),
];

/// The target kind and, for `mtd`, its NAND settings. Refused with the key named.
fn kind_and_mtd<'a>(
    cfg: &Config<'a>,
) -> core::result::Result<(Kind, Option<Mtd>), ConfigError<'a>> {
    let kind = Kind::parse(cfg.str("kind").unwrap_or("block"))
        .ok_or(ConfigError::new("kind", "must be block, file or mtd"))?;
    let mtd = match kind {
        Kind::Mtd => Some(Mtd::from_config(cfg)?),
        _ if cfg.str("image").is_some() => {
            return Err(ConfigError::new("image", "is only for kind = mtd"));
        }
        _ => None,
    };
    Ok((kind, mtd))
}

/// The `max_dict_mb` setting: the largest dictionary a decoder may take, in MiB (1 to 8).
fn max_dict_mb<'a>(cfg: &Config<'a>) -> core::result::Result<i64, ConfigError<'a>> {
    match cfg.int("max_dict_mb").unwrap_or(8) {
        mb @ 1..=8 => Ok(mb),
        _ => Err(ConfigError::new("max_dict_mb", "must be 1 to 8")),
    }
}

impl Mtd {
    /// The NAND settings of a `kind = "mtd"` board file. The test overrides (`erase_size` and the
    /// rest) make the target a regular file standing in for the partition.
    fn from_config<'a>(cfg: &Config<'a>) -> core::result::Result<Self, ConfigError<'a>> {
        let image = match cfg.str("image") {
            None => Image::Raw,
            Some(text) => {
                Image::parse(text).ok_or(ConfigError::new("image", "must be ubi or raw"))?
            }
        };
        let positive = |key: &'static str, n: i64| {
            u64::try_from(n)
                .ok()
                .filter(|&n| n > 0)
                .ok_or(ConfigError::new(key, "must be a positive number"))
        };
        let fake = match cfg.int("erase_size") {
            None => None,
            Some(erase) => {
                let write = positive("write_size", cfg.int("write_size").unwrap_or(2048))?;
                let subpage = match cfg.int("subpage_size") {
                    None => write,
                    Some(sub) => positive("subpage_size", sub)?,
                };
                Some(FakeNand {
                    erase: positive("erase_size", erase)?,
                    write,
                    subpage,
                    bad: cfg.str("bad_blocks").map(Into::into),
                    index: match cfg.int("mtd_index") {
                        None => None,
                        Some(n) => Some(u32::try_from(n).map_err(|_| {
                            ConfigError::new("mtd_index", "must be 0 to 4294967295")
                        })?),
                    },
                })
            }
        };
        Ok(Self {
            image,
            mark_bad: cfg.bool("mark_bad").unwrap_or(false),
            sysfs: cfg.str("sysfs").unwrap_or("/sys").into(),
            fake,
        })
    }
}

impl Lifecycle for SystemFlash {
    type Needs = SystemFlashNeeds;
    const TRACKS_CALLERS: bool = true;
    const JOBS: bool = true;
    const TICKS: bool = true;

    fn check_config<'a>(cfg: &Config<'a>) -> core::result::Result<(), ConfigError<'a>> {
        cfg.check_keys(KEYS)?;
        kind_and_mtd(cfg)?;
        max_dict_mb(cfg)?;
        Ok(())
    }

    fn start(needs: SystemFlashNeeds, cfg: &Config<'_>) -> Result<Self> {
        // The per-device key is removed: the board's security switch replaces it.
        if cfg.get("require_signed").is_some() || cfg.get("security_view").is_some() {
            return Err(Status::INVALID_ARGUMENT);
        }
        let path = cfg.str("path").ok_or(Status::INVALID_ARGUMENT)?;
        let (kind, mtd) = kind_and_mtd(cfg).map_err(|_| Status::INVALID_ARGUMENT)?;
        let format = mtd.as_ref().map_or(manifest::DISK, |m| m.image.format());
        let max_mb = max_dict_mb(cfg).map_err(|_| Status::INVALID_ARGUMENT)?;
        // A key file that can't be read leaves the board trusting no manifest, as slots do.
        let keys = cfg
            .str("keys")
            .and_then(|path| fs::read_to_string(path).ok())
            .and_then(|text| manifest::parse_keys(&text).ok())
            .unwrap_or_default();
        Ok(Self {
            indicator: needs.indicator,
            shown: None,
            path: path.into(),
            kind,
            mtd,
            format,
            allow_first_flash: cfg.bool("allow_first_flash").unwrap_or(false),
            max_dict: (max_mb as u64) << 20,
            board_id: cfg.board().to_string(),
            keys,
            mountinfo: cfg
                .str("mountinfo")
                .unwrap_or("/proc/self/mountinfo")
                .into(),
            manifest: None,
            written: None,
            corrected: 0,
            last_signed: "",
            last_reason: String::new(),
            state: "idle",
            error: String::new(),
            job: None,
            next_job: 0,
            calling: CallerId(0),
        })
    }

    fn caller(&mut self, caller: CallerId) {
        self.calling = caller;
    }

    fn health(&self) -> Health {
        if self.state == "failed" {
            return Health::Failed("the last write or verify failed; see info");
        }
        Health::Ok
    }

    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        let more = self.verify_step();
        self.show_verify_progress();
        more.then_some(now_ns)
    }

    fn job(&mut self, job: JobId, result: &mut Reply<'_>) -> JobState {
        let Some(running) = self.job.as_ref().filter(|j| j.id == job) else {
            return JobState::Finished(Status::NOT_FOUND);
        };
        let state = match &running.end {
            None => {
                return JobState::Running {
                    done: running.got,
                    total: running.size,
                };
            }
            Some(Ok(number)) => {
                let is_write = matches!(running.work, Work::Write(_));
                let sent = result.write(&number.to_le_bytes()).and_then(|()| {
                    if is_write {
                        result.write(&[running.code])
                    } else {
                        Ok(())
                    }
                });
                JobState::Finished(match sent {
                    Ok(()) => Status::OK,
                    Err(status) => status,
                })
            }
            Some(Err((status, _))) => JobState::Finished(*status),
        };
        self.job = None;
        state
    }

    fn job_cancel(&mut self, job: JobId) {
        let Some(running) = self.job.as_mut().filter(|j| j.id == job && j.end.is_none()) else {
            return;
        };
        let is_write = matches!(running.work, Work::Write(_));
        running.end = Some(Err((Status::CANCELLED, String::new())));
        self.show("failed", NO_PROGRESS);
        if is_write {
            self.error = "the write was cancelled: the target holds a partial image; write it again before booting it".into();
            self.state = "cancelled";
        } else {
            self.state = "written";
        }
    }
}

impl SystemFlash {
    /// The NAND fields of `info`: the image kind, the erase block, the bad blocks, the good blocks
    /// the image needs and the ones the target has, and the ECC corrections counted.
    fn nand_fields(&self, found: &std::result::Result<Found, String>) -> String {
        let (mut erase, mut bad, mut available) = (0, 0, 0);
        if let Ok(found) = found
            && self.mtd.is_some()
            && let Ok(geometry) = self.mtd_geometry(found)
        {
            erase = geometry.erase;
            if let Ok(mut chip) = self.open_chip(found, false)
                && let Ok(good) = nand::good_blocks(chip.as_mut(), &geometry)
            {
                available = good.len() as u64;
                bad = geometry.blocks() - available;
            }
        }
        let image_size = self.manifest.as_ref().map_or(0, |a| a.manifest.image_size);
        let needed = if erase == 0 {
            0
        } else {
            image_size.div_ceil(erase)
        };
        let image = self.mtd.as_ref().map_or("raw", |m| m.image.word());
        format!(
            ",\"image\":\"{image}\",\"erase_size\":{erase},\"bad_blocks\":{bad},\"needed\":{needed},\"available\":{available},\"corrected_bits\":{}",
            self.corrected
        )
    }
}

impl Flash for SystemFlash {
    fn info(&mut self, into: &mut [u8]) -> Result<usize> {
        let found = self.target();
        let (size, reason, holds) = match &found {
            Ok(found) => {
                let holds = target::open(found, false)
                    .map(|file| target::holds_something(&file))
                    .unwrap_or(false);
                (
                    found.size,
                    self.unwritable(found).unwrap_or_default(),
                    holds,
                )
            }
            Err(sentence) => (0, sentence.clone(), false),
        };
        let mut out = String::from("{\"kind\":");
        quoted(self.kind.word(), &mut out);
        out.push_str(&format!(
            ",\"size\":{size},\"block_size\":{},\"holds_data\":{holds},\"writable\":{},\"reason\":",
            target::SECTOR,
            reason.is_empty()
        ));
        quoted(&reason, &mut out);
        out.push_str(",\"state\":");
        quoted(self.state, &mut out);
        out.push_str(",\"error\":");
        quoted(&self.error, &mut out);
        out.push_str(&format!(
            ",\"manifest\":{{\"present\":{}",
            self.manifest.is_some()
        ));
        if let Some(accepted) = &self.manifest {
            out.push_str(",\"board\":");
            quoted(&accepted.manifest.board, &mut out);
            out.push_str(",\"format\":");
            quoted(&accepted.manifest.format, &mut out);
            out.push_str(&format!(",\"image_size\":{}", accepted.manifest.image_size));
        }
        out.push_str(",\"signed\":");
        quoted(self.last_signed, &mut out);
        out.push_str(",\"signer\":");
        quoted(
            self.manifest.as_ref().map_or("", |a| a.outcome.signer()),
            &mut out,
        );
        out.push_str(",\"reason\":");
        quoted(&self.last_reason, &mut out);
        out.push('}');
        out.push_str(&format!(
            ",\"allow_first_flash\":{},\"security\":{},\"max_dict_mb\":{}",
            self.allow_first_flash,
            lemnos_slot_store::security::enforced(),
            self.max_dict >> 20
        ));
        if self.kind == Kind::Mtd {
            out.push_str(&self.nand_fields(&found));
        }
        out.push('}');
        let bytes = out.as_bytes();
        let slot = into
            .get_mut(..bytes.len())
            .ok_or(Status::BUFFER_TOO_SMALL)?;
        slot.copy_from_slice(bytes);
        Ok(bytes.len())
    }

    fn manifest(&mut self, manifest: &[u8], record: &[u8]) -> Result<()> {
        if self.job.is_some() {
            return Err(self.refuse(Status::BUSY, "a write or verify is running"));
        }
        let text = match std::str::from_utf8(manifest) {
            Ok(text) => text,
            Err(_) => {
                return Err(self.reject(Status::INVALID_ARGUMENT, "the manifest is not text", ""));
            }
        };
        // The board is checked first, and nothing about the signature is looked at for another one.
        let (parsed, field) = match manifest::parse_for(text, &self.board_id, self.format) {
            Ok(parsed) => parsed,
            Err((status, sentence)) => return Err(self.reject(status, sentence, "")),
        };
        if let Some(field) = field
            && field != record
        {
            return Err(self.reject(
                Status::INVALID_ARGUMENT,
                "the manifest's signature is not the record given",
                "",
            ));
        }
        let decided = manifest::decide(
            lemnos_slot_store::security::enforced(),
            &parsed.board,
            &parsed.format,
            parsed.image_size,
            &parsed.sha256,
            record,
            &self.keys,
        );
        match decided {
            Ok(outcome) => {
                self.last_signed = outcome.word();
                self.last_reason = outcome.reason();
                self.error.clear();
                self.manifest = Some(Accepted {
                    manifest: parsed,
                    record: record.to_vec(),
                    outcome,
                });
                Ok(())
            }
            Err((status, sentence)) => Err(self.reject(status, sentence, "refused")),
        }
    }

    fn write(&mut self, size: u64, format: u8) -> Result<JobId> {
        if self.job.is_some() {
            return Err(self.refuse(Status::BUSY, "a write or verify is running"));
        }
        let Some(accepted) = self.manifest.clone() else {
            return Err(self.refuse(
                Status::NOT_FOUND,
                "there is no manifest: send the image's transport JSON first",
            ));
        };
        let manifest = accepted.manifest;
        if !self.allow_first_flash {
            return Err(self.refuse(
                Status::NOT_PERMITTED,
                "allow_first_flash is not set: only a flasher's board file sets it",
            ));
        }
        if format == FORMAT_RAW && size != manifest.image_size {
            return Err(self.refuse(Status::INVALID_ARGUMENT, "a raw stream is the image's size"));
        }
        let found = match self.target() {
            Ok(found) => found,
            Err(sentence) => return Err(self.refuse(Status::NO_DEVICE, sentence)),
        };
        if let Some(reason) = self.unwritable(&found) {
            return Err(self.refuse(Status::NOT_PERMITTED, reason));
        }
        if manifest.image_size > found.size {
            let sentence = format!(
                "the image is {} bytes and the target is {}",
                manifest.image_size, found.size
            );
            return Err(self.refuse(NO_SPACE, sentence));
        }
        let out = if self.kind == Kind::Mtd {
            match self.nand_out(&found, manifest.image_size) {
                Ok(out) => out,
                Err((status, sentence)) => return Err(self.refuse(status, sentence)),
            }
        } else {
            match target::open(&found, true) {
                Ok(file) => Out::File(file),
                Err(error) => {
                    let (status, sentence) =
                        io_failure(&error, "the target can't be opened for writing");
                    return Err(self.refuse(status, sentence));
                }
            }
        };
        self.next_job += 1;
        let id = JobId(self.next_job);
        let decoder = (format == FORMAT_XZ).then(|| Decoder::new(self.max_dict));
        self.job = Some(Job {
            id,
            starter: self.calling,
            work: Work::Write(Box::new(Writer {
                decoder,
                out,
                manifest,
                written: 0,
                hasher: Sha256::new(),
                refused: None,
                expect: accepted.record,
                record: Vec::with_capacity(RECORD_LEN),
                streams: 0,
            })),
            code: accepted.outcome.code(),
            size,
            got: 0,
            end: None,
        });
        self.state = "writing";
        self.error.clear();
        self.show("receiving", 0);
        Ok(id)
    }

    fn data(&mut self, offset: u64, bytes: &[u8]) -> Result<()> {
        let caller = self.calling;
        let len = bytes.len() as u64;
        // What the call does to the write, decided under one borrow of the job.
        enum Step {
            Refuse(Status, &'static str),
            /// A piece taken; the progress, and whether the image is being written yet.
            Taken(u16, bool),
            /// The stream's last piece: the image is checked and the write ends.
            Last,
            Failed(Status, String),
        }
        let step = match self.job.as_mut() {
            Some(job) if matches!(job.work, Work::Write(_)) && job.end.is_none() => {
                if job.starter != caller {
                    Step::Refuse(
                        Status::NOT_PERMITTED,
                        "only the write's starter may send its stream",
                    )
                } else if offset != job.got {
                    Step::Refuse(
                        Status::INVALID_ARGUMENT,
                        "the stream's offset is not where the last data ended; nothing changed",
                    )
                } else if job.got + len > job.size {
                    Step::Refuse(
                        Status::INVALID_ARGUMENT,
                        "the data goes past the stream's size",
                    )
                } else {
                    let Work::Write(writer) = &mut job.work else {
                        return Err(Status::NOT_FOUND);
                    };
                    match Self::feed(writer, bytes) {
                        Err((status, sentence)) => {
                            job.got += len;
                            Step::Failed(status, sentence)
                        }
                        Ok(()) => {
                            job.got += len;
                            if job.got < job.size {
                                Step::Taken(permille(job.got, job.size), writer.written > 0)
                            } else {
                                Step::Last
                            }
                        }
                    }
                }
            }
            _ => Step::Refuse(Status::NOT_FOUND, "no write is running"),
        };
        match step {
            Step::Refuse(status, sentence) => Err(self.refuse(status, sentence)),
            Step::Taken(progress, writing) => {
                self.show(if writing { "writing" } else { "receiving" }, progress);
                Ok(())
            }
            Step::Failed(status, sentence) => {
                self.end_job(Err((status, sentence)), "failed");
                Err(status)
            }
            Step::Last => {
                self.show("verifying", NO_PROGRESS);
                let (end, manifest) = self.finish_job_write();
                self.conclude_write(end, manifest)
            }
        }
    }

    fn verify(&mut self) -> Result<JobId> {
        if self.job.is_some() {
            return Err(self.refuse(Status::BUSY, "a write or verify is running"));
        }
        let Some(written) = self.written.clone() else {
            return Err(self.refuse(
                Status::NOT_FOUND,
                "no finished image: a write must have finished before a verify",
            ));
        };
        let found = match self.target() {
            Ok(found) => found,
            Err(sentence) => return Err(self.refuse(Status::NO_DEVICE, sentence)),
        };
        let image_size = written.manifest.image_size;
        let work = if self.kind == Kind::Mtd {
            let made = self
                .mtd_geometry(&found)
                .map_err(|why| (Status::INVALID_ARGUMENT, why))
                .and_then(|geometry| {
                    let chip = self
                        .open_chip(&found, false)
                        .map_err(|why| (Status::IO, why))?;
                    nand::Verifier::new(chip, geometry, image_size, written.expect)
                });
            match made {
                Ok(verifier) => Work::Nand(Box::new(verifier)),
                Err((status, sentence)) => return Err(self.refuse(status, sentence)),
            }
        } else {
            let file = match target::open(&found, false) {
                Ok(file) => file,
                Err(error) => {
                    let (status, sentence) = io_failure(&error, "the target can't be opened");
                    return Err(self.refuse(status, sentence));
                }
            };
            Work::Verify(Checker {
                file,
                buf: vec![0; CHUNK],
                read: 0,
                hasher: Sha256::new(),
                expect: written.expect,
                image_size,
            })
        };
        self.next_job += 1;
        let id = JobId(self.next_job);
        self.job = Some(Job {
            id,
            starter: self.calling,
            work,
            code: 0,
            size: image_size,
            got: 0,
            end: None,
        });
        self.state = "verifying";
        self.error.clear();
        self.show("verifying", 0);
        Ok(id)
    }
}

impl SystemFlash {
    /// After a write the kernel is asked to read the disk's partition table again. A kernel that
    /// holds a partition open refuses; that is reported, not failed: the restart the flasher does
    /// next makes the new table the one in use.
    fn note_partition_table(&mut self) {
        let Ok(found) = self.target() else {
            return;
        };
        if found.kind != Kind::Block {
            return;
        }
        let Ok(file) = target::open(&found, false) else {
            return;
        };
        if let Err(error) =
            lemnos_driver_system_flash_sys::reread_partitions(std::os::fd::AsFd::as_fd(&file))
        {
            self.error = format!(
                "the image is written, but the kernel did not re-read its partition table ({error}): restart the board before using it"
            );
        }
    }
}

/// What stops the security switch changing (`docs/slots.md`, "The security switch"): a write or a
/// verify running, in either direction; and to on, a trust root on this device's key list.
impl SecurityPrecondition for SystemFlash {
    fn apply(&mut self, _on: bool) -> Result<()> {
        // First flash keeps no boot-side mark: nothing to make match the switch.
        Ok(())
    }

    fn precondition(&mut self, on: bool, into: &mut [u8]) -> Result<usize> {
        let mut lines = Vec::new();
        if self.job.is_some() {
            lines.push("busy: a write or verify is running".to_string());
        }
        if on && self.keys.is_empty() {
            lines.push("missing: no trust root: the keys list (keys in the system-flash config) has no valid line".to_string());
        }
        let mut text = lines.join("\n");
        text.push('\n');
        let bytes = text.as_bytes();
        into.get_mut(..bytes.len())
            .ok_or(Status::BUFFER_TOO_SMALL)?
            .copy_from_slice(bytes);
        Ok(bytes.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_device_is_no_space_and_anything_else_is_io() {
        let full = io::Error::from_raw_os_error(28);
        assert_eq!(io_failure(&full, "x").0, NO_SPACE);
        let other = io::Error::from_raw_os_error(5);
        assert_eq!(io_failure(&other, "x").0, Status::IO);
    }

    #[test]
    fn check_config_names_the_bad_setting() {
        use lemnos_core::{Config, ConfigValue};
        fn check<'a>(
            entries: &'a [(&'a str, ConfigValue<'a>)],
        ) -> core::result::Result<(), ConfigError<'a>> {
            <SystemFlash as Lifecycle>::check_config(&Config::new(entries))
        }
        let mtd = [
            ("path", ConfigValue::Str("/dev/mtd0")),
            ("kind", ConfigValue::Str("mtd")),
            ("image", ConfigValue::Str("ubi")),
            ("erase_size", ConfigValue::Int(131_072)),
            ("subpage_size", ConfigValue::Int(2048)),
            ("mtd_index", ConfigValue::Int(0)),
            ("max_dict_mb", ConfigValue::Int(1)),
        ];
        assert!(<SystemFlash as Lifecycle>::check_config(&Config::new(&mtd)).is_ok());
        assert!(
            check(&[
                ("path", ConfigValue::Str("/dev/x")),
                ("max_dict_mb", ConfigValue::Int(8))
            ])
            .is_ok()
        );

        assert_eq!(
            check(&[
                ("bogus", ConfigValue::Int(1)),
                ("path", ConfigValue::Str("/x"))
            ])
            .unwrap_err()
            .key,
            "bogus"
        );
        let near = check(&[
            ("path", ConfigValue::Str("/x")),
            ("max_dict_m", ConfigValue::Int(8)),
        ])
        .unwrap_err();
        assert_eq!(
            (near.key, near.suggestion),
            ("max_dict_m", Some("max_dict_mb"))
        );
        assert_eq!(
            check(&[("path", ConfigValue::Int(1))]).unwrap_err().key,
            "path"
        );
        assert_eq!(check(&[]).unwrap_err().key, "path");
        assert_eq!(
            check(&[
                ("path", ConfigValue::Str("/x")),
                ("kind", ConfigValue::Str("tape"))
            ])
            .unwrap_err()
            .key,
            "kind"
        );
        assert_eq!(
            check(&[
                ("path", ConfigValue::Str("/x")),
                ("image", ConfigValue::Str("ubi"))
            ])
            .unwrap_err()
            .key,
            "image"
        );
        assert_eq!(
            check(&[
                ("path", ConfigValue::Str("/x")),
                ("kind", ConfigValue::Str("mtd")),
                ("image", ConfigValue::Str("xfs"))
            ])
            .unwrap_err()
            .key,
            "image"
        );
        assert_eq!(
            check(&[
                ("path", ConfigValue::Str("/x")),
                ("max_dict_mb", ConfigValue::Int(0))
            ])
            .unwrap_err()
            .key,
            "max_dict_mb"
        );
        assert_eq!(
            check(&[
                ("path", ConfigValue::Str("/x")),
                ("max_dict_mb", ConfigValue::Int(9))
            ])
            .unwrap_err()
            .key,
            "max_dict_mb"
        );
        let mtd_base = [
            ("path", ConfigValue::Str("/x")),
            ("kind", ConfigValue::Str("mtd")),
            ("erase_size", ConfigValue::Int(1)),
        ];
        let mut write_zero = mtd_base.to_vec();
        write_zero.push(("write_size", ConfigValue::Int(0)));
        assert_eq!(check(&write_zero).unwrap_err().key, "write_size");
        let mut subpage_zero = mtd_base.to_vec();
        subpage_zero.push(("subpage_size", ConfigValue::Int(0)));
        assert_eq!(check(&subpage_zero).unwrap_err().key, "subpage_size");
        let mut index_negative = mtd_base.to_vec();
        index_negative.push(("mtd_index", ConfigValue::Int(-1)));
        assert_eq!(check(&index_negative).unwrap_err().key, "mtd_index");
        assert_eq!(
            check(&[
                ("path", ConfigValue::Str("/x")),
                ("kind", ConfigValue::Str("mtd")),
                ("erase_size", ConfigValue::Int(0))
            ])
            .unwrap_err()
            .key,
            "erase_size"
        );
    }
}
