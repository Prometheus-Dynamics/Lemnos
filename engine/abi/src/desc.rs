use crate::Status;

/// Where a buffer's bytes are: `size` bytes at `offset` in the dma-buf `fd`.
///
/// It is a memory location and nothing else. What the bytes are (an image,
/// its planes and their layout, a tensor, a bitstream) is the business of the
/// contract and driver that use them, never of the descriptor: movers in
/// Lemnos carry opaque bytes.
///
/// Ownership of the descriptor follows the call: borrowed for its duration
/// unless [`BufferDesc::OWNED`] is set, in which case the receiver closes it.
/// `struct_size` is the size the sender was built with, so a newer sender's
/// longer struct is read up to the fields this one knows.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BufferDesc {
    pub struct_size: u32,
    pub flags: u32,
    /// The dma-buf holding the bytes.
    pub fd: i32,
    pub reserved: u32,
    /// Byte offset of the buffer in `fd`.
    pub offset: u64,
    /// Bytes of the buffer from `offset`.
    pub size: u64,
}

impl BufferDesc {
    /// The receiver owns the descriptor and closes it.
    pub const OWNED: u32 = 1 << 0;

    /// `size` bytes at `offset` in `fd`.
    pub const fn new(fd: i32, offset: u64, size: u64) -> Self {
        Self {
            struct_size: size_of::<Self>() as u32,
            flags: 0,
            fd,
            reserved: 0,
            offset,
            size,
        }
    }

    /// `len` bytes at the start of `fd`.
    pub const fn bytes(fd: i32, len: u64) -> Self {
        Self::new(fd, 0, len)
    }

    /// Checks what a receiver relies on: a size it can read, a descriptor,
    /// and a range that does not overflow.
    pub const fn validate(&self) -> Status {
        if (self.struct_size as usize) < size_of::<Self>()
            || self.fd < 0
            || self.offset.checked_add(self.size).is_none()
        {
            return Status::INVALID_ARGUMENT;
        }
        Status::OK
    }
}

/// A fence by file descriptor. Ownership follows the call as for
/// [`BufferDesc`].
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FenceDesc {
    /// [`FenceDesc::NONE`], [`FenceDesc::SYNC_FILE`] or
    /// [`FenceDesc::SYNCOBJ`]; a kind the receiver does not know is refused.
    pub kind: u32,
    pub fd: i32,
    /// A syncobj's timeline point (0: a binary syncobj). 0 for a sync file.
    pub point: u64,
}

impl FenceDesc {
    /// No fence: the work is already complete.
    pub const NONE: u32 = 0;
    /// A sync file (`sync_file(5)`): poll it for `POLLIN`.
    pub const SYNC_FILE: u32 = 1;
    /// A DRM syncobj exported as a file descriptor, at `point`.
    pub const SYNCOBJ: u32 = 2;

    pub const fn none() -> Self {
        Self {
            kind: Self::NONE,
            fd: -1,
            point: 0,
        }
    }

    pub const fn sync_file(fd: i32) -> Self {
        Self {
            kind: Self::SYNC_FILE,
            fd,
            point: 0,
        }
    }

    pub const fn syncobj(fd: i32, point: u64) -> Self {
        Self {
            kind: Self::SYNCOBJ,
            fd,
            point,
        }
    }

    pub fn validate(&self) -> Status {
        match self.kind {
            Self::NONE => Status::OK,
            Self::SYNC_FILE | Self::SYNCOBJ if self.fd >= 0 => Status::OK,
            Self::SYNC_FILE | Self::SYNCOBJ => Status::INVALID_ARGUMENT,
            _ => Status::NOT_SUPPORTED,
        }
    }
}
