use core::mem::{align_of, offset_of, size_of};

use crate::{AbiVersion as V, BufferDesc, FenceDesc, Handle, PlaneDesc, Status};

/// A surface's version, exchanged before any other call: the caller passes
/// the one it was built against, the surface checks it ([`Self::accepts`]).
///
/// - `major`: changes that break callers. Must be equal.
/// - `minor`: additions. The surface must be at least the caller's.
/// - `layout`: a hash of the layouts of the types crossing the boundary
///   ([`layout_hash`]). Must be equal: a struct that changed shape under the
///   same version is caught here, not in a misread field.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AbiVersion {
    pub struct_size: u32,
    pub major: u16,
    pub minor: u16,
    pub layout: u64,
}

impl AbiVersion {
    pub const fn new(major: u16, minor: u16, layout: u64) -> Self {
        Self {
            struct_size: size_of::<Self>() as u32,
            major,
            minor,
            layout,
        }
    }

    /// Whether this surface serves a caller built against `caller`:
    /// [`Status::OK`] or [`Status::VERSION_MISMATCH`].
    pub const fn accepts(&self, caller: &Self) -> Status {
        if (caller.struct_size as usize) < size_of::<Self>()
            || caller.major != self.major
            || caller.minor > self.minor
            || caller.layout != self.layout
        {
            Status::VERSION_MISMATCH
        } else {
            Status::OK
        }
    }
}

/// FNV-1a over `words`: a surface hashes the size, alignment and field
/// offsets of every type it passes, in a fixed order, and puts the result in
/// its [`AbiVersion::layout`]. Computed at compile time.
pub const fn layout_hash(words: &[u64]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut i = 0;
    while i < words.len() {
        let mut word = words[i];
        let mut byte = 0;
        while byte < 8 {
            hash ^= word & 0xff;
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            word >>= 8;
            byte += 1;
        }
        i += 1;
    }
    hash
}

macro_rules! shape {
    ($t:ty $(, $field:ident)*) => {
        [size_of::<$t>() as u64, align_of::<$t>() as u64 $(, offset_of!($t, $field) as u64)*]
    };
}

const fn concat<const N: usize>(parts: &[&[u64]]) -> [u64; N] {
    let mut out = [0; N];
    let (mut n, mut p) = (0, 0);
    while p < parts.len() {
        let mut i = 0;
        while i < parts[p].len() {
            out[n] = parts[p][i];
            n += 1;
            i += 1;
        }
        p += 1;
    }
    out
}

const SHAPES: [u64; 32] = concat(&[
    &shape!(Status),
    &shape!(Handle),
    &shape!(PlaneDesc, fd, reserved, offset, stride, size),
    &shape!(
        BufferDesc,
        struct_size,
        flags,
        width,
        height,
        fourcc,
        plane_count,
        modifier,
        planes
    ),
    &shape!(FenceDesc, kind, fd, point),
    &shape!(V, struct_size, major, minor, layout),
]);

/// The layout hash of this crate's own types. Types holding pointers
/// (`MessageSink`) are left out: their layout follows the target's pointer
/// width, which both sides of a call in one process share, and the hash is
/// then the same on every target, so a C header can carry it as a constant.
pub const LAYOUT_HASH: u64 = layout_hash(&SHAPES);

/// Version 1.0 is frozen: this is its layout hash, as `include/lemnos/abi.h`
/// declares it. A change to any hashed layout fails to compile here, and is a
/// new major version (see the README's stability rules).
const FROZEN_1_0: u64 = 0xa02c_fb4a_8dd4_545b;
const _: () = assert!(
    LAYOUT_HASH == FROZEN_1_0,
    "a layout of version 1.0 changed: that is a new major"
);

/// This crate's version. A surface that passes its types embeds it in its own
/// handshake (or checks it alongside).
pub const VERSION: AbiVersion = AbiVersion::new(1, 0, LAYOUT_HASH);
