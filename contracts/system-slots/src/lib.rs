//! The `system.slots/1` contract: the operating system images ("slots") a board has installed,
//! side by side, and which one it boots.
//!
//! A slot is a root filesystem image with the kernel, device tree and initramfs that boot it. Any
//! number may be installed, as many as fit. One is the default, the one the board boots. Another
//! can be booted once on trial: it becomes the default only when it is confirmed, and a board
//! that restarts before then boots the default again. A board may have a single slot; a trial
//! then falls back to it.
//!
//! A slot arrives as a bundle: a tar of `slot.json` (its manifest), `slot.sig` (a 108-byte
//! signature record, the shared container first flash uses: the key id and an Ed25519 signature
//! over the domain-tagged slot bundle message of the manifest's bytes), `rootfs.erofs` and `boot/`.
//! `slot.json` is the first member and `slot.sig`, when present, the second (`docs/slots.md`). The record names its key, and the
//! signature is checked against that key when the board holds it. The board's
//! policy (`require_signed`) is the table in `lemnos_slot_store::signing`, the same one first
//! flash applies: an unsigned bundle or one under a key the board does not hold installs and is
//! reported as not checked, unless the board requires signing; a bad signature under a key the
//! board holds is always refused. The manifest
//! names the image's size and SHA-256, every boot file's SHA-256 and the image's dm-verity root
//! hash. The driver carries the bytes and checks them against the manifest; it never reads what
//! the image holds.
//!
//! The JSON `list` writes:
//!
//! ```text
//! {"slot_active":"<id>","slot_default":"<id>","slot_previous":"<id>","slot_trial":"<id>",
//!  "state":"idle","error":"","free_bytes":N,"reserve_bytes":N,"keep":N,
//!  "slots":[{"id":"<id>","product":"..","board":"..","version":"..","variant":"..","kernel":"..",
//!            "display_name":"..","size":N,"verity":true,"intact":true,
//!            "flags":["running","default","previous","trial","staged"],"pinned":false,
//!            "good":"yes|no|untried","signed":"trusted|untrusted|unsigned","signer":"<owner>",
//!            "installed":"<UTC time>","last_boot":"<UTC time>",
//!            "failures":N,"failed":false,
//!            "signature":"verified|not_checked_unknown_key|not_checked_unsigned"}]}
//! ```
//!
//! `signed` is the older label of the same fact (`trusted` is `verified`, `untrusted` is
//! `not_checked_unknown_key`, `unsigned` is `not_checked_unsigned`); a slot installed before the
//! `signature` word existed reports it from its `signed` label. `info` writes the policy and the
//! last install's signature result:
//!
//! ```text
//! {"require_signed":false,"last_install":{"slot":"<id, or \"\" when refused>",
//!  "signature":"verified|not_checked_unknown_key|not_checked_unsigned|refused","signer":"<owner>",
//!  "reason":"<the reason in words>"},"default_signature":"<the default's signature word>",
//!  "boot_unverified":"<the slot the boot side started unverified, or \"\">",
//!  "key_ids":[{"id":"<64 hex: SHA-256 of a held public key>","owner":"<owner>"}],
//!  "last_attempt":{"state":"none|running|installed|refused|cancelled|interrupted|failed",
//!   "slot":"<the slot id the attempt was for, or \"\">","reason":"<the reason the starter was given, at most 192 bytes>",
//!   "received":N,"total":N,"boot":"<boot id>","started_ms":N,"ended_ms":N,"text":"last install: ..."}}
//! ```
//!
//! An empty id is `""`. `last_attempt` is the last install attempt, appended after `key_ids` (so the
//! fields before it are unchanged): any session reads how the last install ended, though a job belongs
//! to its starter's connection. `received` of `total` bytes are the job's own figures; `started_ms` and
//! `ended_ms` are the host's monotonic clock in milliseconds, counting from the boot `boot` names
//! (`ended_ms` is `null` while it runs). `none` is `none since the service started` and carries `boot` too: it never claims an install did not end before the daemon started. An attempt a daemon restart cut short is `interrupted`
//! with `null` for the bytes and times. `text` is the same record as one line. It is kept until the
//! next attempt starts, in memory for the daemon's life (`docs/slots.md`, "The last install attempt").
//!
//! `list_page` writes the same object with `slots` holding one page, then `"more":<bool>` and
//! `"next":"<seq>:<id>"` (`""` when no page follows) before `trial` and `resolved`. Pages run
//! over the slots' install numbers, newest first, ties by id, so a slot installed between two
//! pages is never in a later one and none is repeated.

#![no_std]

use lemnos_core::{JobId, Result, contract};

#[contract("system.slots", version = 1, kind = "system")]
pub trait Slots {
    /// The installed slots, the pointers and the free space, as JSON (see the crate's
    /// documentation). Returns its length; `BUFFER_TOO_SMALL` when it doesn't fit.
    #[method(1)]
    fn list(&mut self, #[out] into: &mut [u8]) -> Result<usize>;
    /// Starts installing a bundle of `size` bytes, which [`Slots::write`] then delivers in
    /// order. The job's progress is the bytes taken of `size`, and its result the new slot's id
    /// (UTF-8). `pin` keeps the slot from housekeeping; `keep_old` has confirming it remove no
    /// old slots. One install at a time: `BUSY` while another runs. Never over the running or
    /// the default slot. `NOT_PERMITTED` when the signature policy refuses the bundle (`info`
    /// then says why); `INVALID_ARGUMENT` for a malformed signature.
    #[method(2, job)]
    fn install(&mut self, size: u64, pin: bool, keep_old: bool) -> Result<JobId>;
    /// The running install's bytes from `offset`, which must be where the last write ended.
    /// Only its starter may write.
    #[method(3)]
    fn write(&mut self, offset: u64, bytes: &[u8]) -> Result<()>;
    /// Makes a slot that has booted well and confirmed the default from the next boot. `force`
    /// takes one that failed since its last confirm.
    #[method(4)]
    fn set_default(&mut self, id: &str, force: bool) -> Result<()>;
    /// Restarts the board into `id` once, on trial. Confirmed, it becomes the default, unless
    /// `keep_default`; `keep_old` has that confirm remove no old slots. Not confirmed, the next
    /// restart boots the default again. `force` tries a slot that failed since its last confirm.
    #[method(5)]
    fn trial(&mut self, id: &str, keep_default: bool, keep_old: bool, force: bool) -> Result<()>;
    /// Called once the running system is healthy: a trial that is running becomes the default
    /// (and housekeeping removes the oldest slots past the number kept). A trial that isn't
    /// running is recorded as failed.
    #[method(6)]
    fn confirm(&mut self) -> Result<()>;
    /// Removes a slot. Never the running, the default or the trial slot, nor a pinned one.
    #[method(7)]
    fn remove(&mut self, id: &str) -> Result<()>;
    /// A pinned slot is never removed by housekeeping, nor by `remove` until it is unpinned.
    #[method(8)]
    fn pin(&mut self, id: &str, pinned: bool) -> Result<()>;
    /// How many slots housekeeping keeps, pinned ones not counted. 0 keeps every slot.
    #[method(9)]
    fn set_keep(&mut self, keep: u32) -> Result<()>;
    /// Whether a bundle of `size` bytes would fit, asked before sending one as a file:
    /// `ENOSPC` when it and the reserve don't, `BUSY` while an install runs.
    #[method(10)]
    fn accept(&mut self, size: u64) -> Result<()>;
    /// Installs the bundle a sender put in the data partition's `slots/.incoming/<name>`, as
    /// [`Slots::install`] does: the job's progress is the bytes of the file checked and
    /// unpacked, its result the new slot's id. `name` is one path component of
    /// `[A-Za-z0-9._-]`, and the file must be a regular file there, not a link. The driver owns
    /// the file: it is deleted when the job ends, however it ends, and `.incoming` is emptied
    /// when the driver starts. `BUSY` while another install runs.
    #[method(11, job)]
    fn install_file(&mut self, name: &str, pin: bool, keep_old: bool) -> Result<JobId>;
    /// The boot history, one line per boot kept (`n utc slot outcome reset requested boot`,
    /// oldest first; see `lemnos-slot-store`'s `boots`). Returns its length; `BUFFER_TOO_SMALL`
    /// when it doesn't fit.
    #[method(12)]
    fn boots(&mut self, #[out] into: &mut [u8]) -> Result<usize>;
    /// One page of `list`, for a board whose slots don't fit one reply. `after` is `-` for the
    /// first page, and then the `next` of the page before (`<seq>:<id>`). The page is the slots
    /// that come after the cursor, newest first, as many as fit the buffer; `more` says whether
    /// another page follows, and `next` is the cursor for it. Returns the length, as `list` does.
    /// `INVALID_ARGUMENT` for a cursor that is not one; `BUFFER_TOO_SMALL` when a slot does not fit.
    #[method(13)]
    fn list_page(&mut self, after: &str, #[out] into: &mut [u8]) -> Result<usize>;

    /// The signature policy and the last install's signature result, as JSON (see the crate's
    /// documentation). Returns its length; `BUFFER_TOO_SMALL` when it doesn't fit.
    #[method(14)]
    fn info(&mut self, #[out] into: &mut [u8]) -> Result<usize>;
}
