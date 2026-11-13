//! The `system.slots/1` contract: the operating system images ("slots") a board has installed,
//! side by side, and which one it boots.
//!
//! A slot is a root filesystem image with the kernel, device tree and initramfs that boot it. Any
//! number may be installed, as many as fit. One is the default, the one the board boots. Another
//! can be booted once on trial: it becomes the default only when it is confirmed, and a board
//! that restarts before then boots the default again. A board may have a single slot; a trial
//! then falls back to it.
//!
//! A slot arrives as a bundle: a tar of `slot.json` (its manifest), `slot.sig` (an Ed25519
//! signature over the manifest's bytes), `rootfs.erofs` and `boot/`. A signature is always
//! checked against the keys the board trusts and recorded; a board may refuse a bundle that no
//! trusted key signed, and by default takes it and shows it as unverified. The manifest
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
//!            "failures":N,"failed":false}]}
//! ```
//!
//! An empty id is `""`.

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
    /// the default slot. `NOT_PERMITTED` when the board takes only signed bundles and trusts no
    /// key.
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
}
