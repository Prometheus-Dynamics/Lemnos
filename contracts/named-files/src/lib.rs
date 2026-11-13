//! The `lemnos.files/1` contract: the named files a node serves to a reader, a snapshot at a time.
//!
//! A file has a name (`lemnos/boots`, `lemnos/storage`, ...) and a bound in bytes. A reader asks
//! for a file with [`NamedFiles::open`], which takes the snapshot the transfer then reads: its
//! bytes are fixed for the transfer's life, so a resumed read and a digest over it agree. The
//! bytes come [`NamedFiles::read`] at a time, from any offset, and [`NamedFiles::close`] frees the
//! snapshot. A name the node does not serve is refused by name (`NOT_FOUND`).
//!
//! [`NamedFiles::index`] lists the names the node serves now, in the format `docs/daemon.md`
//! gives: `orion-index 1`, `total <bytes>`, then one `<name> <max_bytes> user-data|no-user-data`
//! line per name.

#![no_std]

use lemnos_core::{Result, contract};

/// The most snapshots one node holds at once. One more is `BUSY`.
pub const MAX_OPEN: usize = 4;

#[contract("lemnos.files", version = 1, kind = "system")]
pub trait NamedFiles {
    /// The index: every name the node serves now, with its bound and whether it may hold user
    /// data. Returns its length; `BUFFER_TOO_SMALL` when it doesn't fit.
    #[method(1)]
    fn index(&mut self, #[out] into: &mut [u8]) -> Result<usize>;
    /// Takes a snapshot of the named file for a transfer, and returns its handle. `NOT_FOUND`
    /// when the node serves no such name (or the file is absent now); `BUSY` at [`MAX_OPEN`]
    /// snapshots; `INVALID_ARGUMENT` when the file is over its bound.
    #[method(2)]
    fn open(&mut self, name: &str) -> Result<u64>;
    /// The size of the snapshot `handle`. `NOT_FOUND` for a handle that is not open.
    #[method(3)]
    fn size(&mut self, handle: u64) -> Result<u64>;
    /// Fills `into` from `from` in the snapshot: at most `into.len()` bytes, zero at the end.
    /// `NOT_FOUND` for a handle that is not open.
    #[method(4)]
    fn read(&mut self, handle: u64, from: u64, #[out] into: &mut [u8]) -> Result<usize>;
    /// Frees the snapshot `handle`. Closing a handle that is not open is `NOT_FOUND`.
    #[method(5)]
    fn close(&mut self, handle: u64) -> Result<()>;
}
