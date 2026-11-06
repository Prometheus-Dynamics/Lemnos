use crate::Status;

/// The most planes a [`BufferDesc`] carries: three colour planes plus one
/// for a modifier's metadata (compression, for example), as DRM allows.
pub const MAX_PLANES: usize = 4;

/// One memory plane of a buffer.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PlaneDesc {
    /// The dma-buf holding this plane; planes may share one. -1 for a plane
    /// that is not used.
    pub fd: i32,
    pub reserved: u32,
    /// Byte offset of the plane in `fd`.
    pub offset: u64,
    /// Bytes per row (the pitch); 0 for a buffer that is not an image.
    pub stride: u64,
    /// Bytes of the plane from `offset`.
    pub size: u64,
}

/// A buffer by file descriptor: a dma-buf image (`fourcc`, `modifier`, one
/// plane per memory plane) or a plain block of bytes (one plane, `fourcc` 0).
///
/// Ownership of the descriptors follows the call: borrowed for its duration
/// unless [`BufferDesc::OWNED`] is set, in which case the receiver closes
/// them. `struct_size` is the size the sender was built with, so a newer
/// sender's longer struct is read up to the fields this one knows.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BufferDesc {
    pub struct_size: u32,
    pub flags: u32,
    pub width: u32,
    pub height: u32,
    /// The DRM fourcc (`DRM_FORMAT_*`); 0 for bytes.
    pub fourcc: u32,
    pub plane_count: u32,
    /// The DRM format modifier; 0 is linear.
    pub modifier: u64,
    pub planes: [PlaneDesc; MAX_PLANES],
}

const UNUSED_PLANE: PlaneDesc = PlaneDesc {
    fd: -1,
    reserved: 0,
    offset: 0,
    stride: 0,
    size: 0,
};

impl BufferDesc {
    /// The receiver owns the descriptors and closes them.
    pub const OWNED: u32 = 1 << 0;

    /// `len` bytes at the start of `fd`.
    pub const fn bytes(fd: i32, len: u64) -> Self {
        let mut planes = [UNUSED_PLANE; MAX_PLANES];
        planes[0] = PlaneDesc {
            fd,
            reserved: 0,
            offset: 0,
            stride: 0,
            size: len,
        };
        Self {
            struct_size: size_of::<Self>() as u32,
            flags: 0,
            width: 0,
            height: 0,
            fourcc: 0,
            plane_count: 1,
            modifier: 0,
            planes,
        }
    }

    /// An image with no planes yet; add them with [`Self::with_plane`].
    pub const fn image(width: u32, height: u32, fourcc: u32, modifier: u64) -> Self {
        Self {
            struct_size: size_of::<Self>() as u32,
            flags: 0,
            width,
            height,
            fourcc,
            plane_count: 0,
            modifier,
            planes: [UNUSED_PLANE; MAX_PLANES],
        }
    }

    /// Adds the next plane. [`Status::INVALID_ARGUMENT`] past [`MAX_PLANES`].
    pub const fn with_plane(mut self, plane: PlaneDesc) -> Result<Self, Status> {
        if self.plane_count as usize >= MAX_PLANES {
            return Err(Status::INVALID_ARGUMENT);
        }
        self.planes[self.plane_count as usize] = plane;
        self.plane_count += 1;
        Ok(self)
    }

    /// The planes in use.
    pub fn planes(&self) -> &[PlaneDesc] {
        &self.planes[..(self.plane_count as usize).min(MAX_PLANES)]
    }

    /// Checks what a receiver relies on: a size it can read, 1 to
    /// [`MAX_PLANES`] planes, each with a descriptor and a range that does
    /// not overflow, and a stride on every plane of an image.
    pub fn validate(&self) -> Status {
        if (self.struct_size as usize) < size_of::<Self>() {
            return Status::INVALID_ARGUMENT;
        }
        let count = self.plane_count as usize;
        if count == 0 || count > MAX_PLANES {
            return Status::INVALID_ARGUMENT;
        }
        let image = self.fourcc != 0;
        for plane in self.planes() {
            if plane.fd < 0
                || plane.offset.checked_add(plane.size).is_none()
                || (image && plane.stride == 0)
            {
                return Status::INVALID_ARGUMENT;
            }
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
