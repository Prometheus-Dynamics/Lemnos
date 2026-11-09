//! The V4L2 calls whose arguments hold pointers the kernel follows or descriptors
//! it creates, so a plain [`Ioctl`](crate::ioctl::Ioctl) cannot stand for them:
//! buffers with their plane arrays, extended controls with their payloads, and
//! exported dma-bufs. Each function sets the pointers itself from borrowed slices,
//! and the lengths and counts the kernel writes back are checked or kept apart
//! from the ones the next call hands it.

use std::io;
use std::marker::PhantomData;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd};

use super::uapi::{
    MEMORY_USERPTR, VIDEO_MAX_PLANES, VIDIOC_EXPBUF, v4l2_buffer, v4l2_exportbuffer,
    v4l2_ext_control, v4l2_ext_control_value, v4l2_ext_controls, v4l2_m, v4l2_plane,
};
use crate::ioctl::{ioctl_ptr, iowr, zeroed};
use crate::v4l2::CTRL_WHICH_REQUEST_VAL;

fn einval() -> io::Error {
    io::Error::from_raw_os_error(libc::EINVAL)
}

/// A request that takes a `struct v4l2_buffer`. Crate-private: the safe calls in
/// [`crate::v4l2`] build the structure, so a caller cannot put a stale descriptor or
/// an address in its `m` member.
#[derive(Clone, Copy, Debug)]
pub(crate) struct BufferIoctl {
    nr: u32,
}

/// `VIDIOC_QUERYBUF`.
pub(crate) const VIDIOC_QUERYBUF: BufferIoctl = BufferIoctl {
    nr: iowr::<v4l2_buffer>(b'V', 9),
};
/// `VIDIOC_QBUF`.
pub(crate) const VIDIOC_QBUF: BufferIoctl = BufferIoctl {
    nr: iowr::<v4l2_buffer>(b'V', 15),
};
/// `VIDIOC_DQBUF`.
pub(crate) const VIDIOC_DQBUF: BufferIoctl = BufferIoctl {
    nr: iowr::<v4l2_buffer>(b'V', 17),
};
/// `VIDIOC_PREPARE_BUF`.
pub(crate) const VIDIOC_PREPARE_BUF: BufferIoctl = BufferIoctl {
    nr: iowr::<v4l2_buffer>(b'V', 93),
};

/// How many planes the kernel says it filled, refused when more than the
/// `capacity` the buffer was queried with.
pub(crate) fn returned_planes(length: u32, capacity: usize) -> io::Result<usize> {
    match usize::try_from(length) {
        Ok(n) if n <= capacity => Ok(n),
        _ => Err(io::Error::from_raw_os_error(libc::EIO)),
    }
}

impl BufferIoctl {
    /// The request number.
    #[cfg(test)]
    pub(crate) const fn nr(self) -> u32 {
        self.nr
    }

    /// Issues the request on `buf`. On a multi-planar queue `planes` are the
    /// buffer's planes, read and written by the kernel, at least one and at most
    /// [`VIDEO_MAX_PLANES`]; on a single-planar queue `planes` must be empty.
    /// User-pointer memory is refused (`EINVAL`): nothing here hands the kernel an
    /// address to write to. Returns how many planes the kernel filled (`EIO` when it
    /// claims more than `planes` holds); one on a single-planar queue.
    pub(crate) fn call(
        self,
        fd: BorrowedFd<'_>,
        buf: &mut v4l2_buffer,
        planes: &mut [v4l2_plane],
    ) -> io::Result<usize> {
        let multi = buf.is_multiplanar();
        if buf.memory == MEMORY_USERPTR
            || planes.len() > VIDEO_MAX_PLANES
            || (multi && planes.is_empty())
            || (!multi && !planes.is_empty())
        {
            return Err(einval());
        }
        let capacity = planes.len();
        if multi {
            buf.m = v4l2_m {
                planes: planes.as_mut_ptr(),
            };
            // At most `VIDEO_MAX_PLANES` (checked above), so this fits.
            buf.length = capacity as u32;
        }
        // SAFETY: the request takes a `v4l2_buffer`, which `buf` is. On a
        // multi-planar queue `m` points at `length` planes in `planes`, alive for
        // the call; their `m` members can only be offsets and descriptors. On a
        // single-planar queue `m` is an offset or a descriptor. User pointers were
        // refused above.
        let result = unsafe { ioctl_ptr(fd, self.nr, std::ptr::from_mut(buf)) };
        if multi {
            // `planes` is about to go back to the caller: keep no pointer to it.
            buf.m = v4l2_m { offset: 0 };
        }
        result?;
        if multi {
            returned_planes(buf.length, capacity)
        } else {
            Ok(1)
        }
    }
}

/// Exports one plane of an MMAP buffer as a dma-buf (`VIDIOC_EXPBUF`); `raw` says
/// which plane and with which open flags.
pub fn export_buffer(fd: BorrowedFd<'_>, raw: &mut v4l2_exportbuffer) -> io::Result<OwnedFd> {
    VIDIOC_EXPBUF.call(fd, raw)?;
    if raw.fd < 0 {
        return Err(io::Error::other("VIDIOC_EXPBUF returned no descriptor"));
    }
    // SAFETY: a successful export returns a new dma-buf descriptor, which no other
    // owner has.
    Ok(unsafe { OwnedFd::from_raw_fd(raw.fd) })
}

/// A request that takes a `struct v4l2_ext_controls`.
#[derive(Clone, Copy, Debug)]
pub struct ExtCtrlsIoctl {
    nr: u32,
    name: &'static str,
}

/// `VIDIOC_G_EXT_CTRLS`.
pub const VIDIOC_G_EXT_CTRLS: ExtCtrlsIoctl = ExtCtrlsIoctl {
    nr: iowr::<v4l2_ext_controls>(b'V', 71),
    name: "VIDIOC_G_EXT_CTRLS",
};
/// `VIDIOC_S_EXT_CTRLS`.
pub const VIDIOC_S_EXT_CTRLS: ExtCtrlsIoctl = ExtCtrlsIoctl {
    nr: iowr::<v4l2_ext_controls>(b'V', 72),
    name: "VIDIOC_S_EXT_CTRLS",
};
/// `VIDIOC_TRY_EXT_CTRLS`.
pub const VIDIOC_TRY_EXT_CTRLS: ExtCtrlsIoctl = ExtCtrlsIoctl {
    nr: iowr::<v4l2_ext_controls>(b'V', 73),
    name: "VIDIOC_TRY_EXT_CTRLS",
};

/// `V4L2_CID_MAX_CTRLS`: the most controls one extended-controls call may carry.
pub const MAX_EXT_CONTROLS: usize = 1024;

/// One control of an extended-controls call: `struct v4l2_ext_control`, with the
/// payload it points at borrowed for `'a`. The payload is exclusive because the
/// kernel may write it on any of the calls, a set included.
#[derive(Debug)]
pub struct ExtControl<'a> {
    /// The control as the next call hands it to the kernel. For a payload,
    /// `size` and the pointer are the borrowed slice's, and stay so whatever the
    /// kernel writes back.
    raw: v4l2_ext_control,
    /// Whether the union holds a pointer the kernel follows.
    has_payload: bool,
    /// What the kernel left in `size` after the last call. Untrusted.
    reported_size: u32,
    payload: PhantomData<&'a mut [u8]>,
}

impl std::fmt::Debug for v4l2_ext_control {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (id, size) = (self.id, self.size);
        write!(f, "v4l2_ext_control({id:#x}, {size} bytes)")
    }
}

impl ExtControl<'static> {
    fn of(id: u32, u: v4l2_ext_control_value) -> Self {
        Self {
            raw: v4l2_ext_control {
                id,
                size: 0,
                reserved2: [0],
                u,
            },
            has_payload: false,
            reported_size: 0,
            payload: PhantomData,
        }
    }

    /// A 32-bit control (an integer, boolean, menu index, bitmask or button).
    pub fn value(id: u32, value: i32) -> Self {
        // Through the 64-bit member, so the union's other bytes are zero.
        Self::of(
            id,
            v4l2_ext_control_value {
                value64: i64::from(value as u32),
            },
        )
    }

    /// A 64-bit control.
    pub fn value64(id: u32, value64: i64) -> Self {
        Self::of(id, v4l2_ext_control_value { value64 })
    }
}

impl<'a> ExtControl<'a> {
    /// A string, array or compound control over `payload`: the kernel reads it on
    /// a set, and fills it on a get. A slice past `u32::MAX` bytes is described as
    /// its first `u32::MAX` bytes, never as more than it holds.
    pub fn payload(id: u32, payload: &'a mut [u8]) -> Self {
        let size = u32::try_from(payload.len()).unwrap_or(u32::MAX);
        let ptr = if payload.is_empty() {
            std::ptr::null_mut()
        } else {
            payload.as_mut_ptr().cast()
        };
        Self {
            raw: v4l2_ext_control {
                id,
                size,
                reserved2: [0],
                u: v4l2_ext_control_value { ptr },
            },
            has_payload: true,
            reported_size: 0,
            payload: PhantomData,
        }
    }

    /// The control's id.
    pub fn id(&self) -> u32 {
        self.raw.id
    }

    /// The payload's size in bytes, as the borrowed slice set it (zero for a value).
    pub fn size(&self) -> u32 {
        self.raw.size
    }

    /// The size the kernel left after the last call: a dynamic array's used
    /// size, or on `ENOSPC` the size it wanted. Untrusted, and possibly larger than
    /// [`Self::size`].
    pub fn reported_size(&self) -> u32 {
        self.reported_size
    }

    /// The 32-bit value after a get.
    pub fn get_value(&self) -> i32 {
        // SAFETY: the union's first four bytes are a plain integer.
        unsafe { self.raw.u.value }
    }

    /// The 64-bit value after a get.
    pub fn get_value64(&self) -> i64 {
        // SAFETY: the union's eight bytes are a plain integer.
        unsafe { self.raw.u.value64 }
    }

    /// Takes what the kernel wrote into the entry it was given. Only the reported
    /// size and a value control's value come back: this control's `size` and
    /// payload pointer stay the borrowed slice's, so a control used again cannot
    /// hand the kernel a length or an address the kernel made up.
    pub(crate) fn absorb(&mut self, kernel: &v4l2_ext_control) {
        self.reported_size = kernel.size;
        if !self.has_payload {
            self.raw.u = kernel.u;
        }
    }
}

/// A refused extended-controls call: the error, and the kernel's `error_idx`.
#[derive(Debug)]
pub struct ExtCtrlsError {
    /// What the call failed with.
    pub source: io::Error,
    /// The index of the control the kernel blames; `controls.len()` when it blames
    /// none of them. Untrusted: look it up with `get`.
    pub error_idx: u32,
}

/// Up to this many controls go to the kernel from the stack, so a call of that
/// size allocates nothing.
const INLINE_EXT_CONTROLS: usize = 16;

impl ExtCtrlsIoctl {
    /// The request's uAPI name, for error messages.
    pub const fn name(self) -> &'static str {
        self.name
    }

    /// The request number.
    pub const fn nr(self) -> u32 {
        self.nr
    }

    /// Issues the request on `controls`, with `which` and (for request values) the
    /// media request in `request`. More than [`MAX_EXT_CONTROLS`] controls is refused
    /// (`E2BIG`), and `request` must be given exactly when `which` names a request
    /// (`EINVAL`). What the kernel wrote back is taken with
    /// [`ExtControl::reported_size`] and the value getters; it never changes what a
    /// later call passes.
    pub fn call(
        self,
        fd: BorrowedFd<'_>,
        which: u32,
        request: Option<BorrowedFd<'_>>,
        controls: &mut [ExtControl<'_>],
    ) -> Result<(), ExtCtrlsError> {
        if controls.len() > MAX_EXT_CONTROLS {
            return Err(refused(libc::E2BIG));
        }
        if (which == CTRL_WHICH_REQUEST_VAL) != request.is_some() {
            return Err(refused(libc::EINVAL));
        }
        // The kernel gets a copy of each entry, so it can scribble on the copy's
        // size and union without reaching `controls`.
        let mut inline: [v4l2_ext_control; INLINE_EXT_CONTROLS] = zeroed();
        let mut heap: Vec<v4l2_ext_control> = Vec::new();
        let kernel: &mut [v4l2_ext_control] = if controls.len() <= INLINE_EXT_CONTROLS {
            &mut inline[..controls.len()]
        } else {
            heap.resize(controls.len(), zeroed());
            &mut heap
        };
        for (dst, src) in kernel.iter_mut().zip(controls.iter()) {
            *dst = src.raw;
        }
        let mut raw = v4l2_ext_controls {
            which,
            // At most `MAX_EXT_CONTROLS` (checked above), so this fits.
            count: kernel.len() as u32,
            error_idx: 0,
            request_fd: request.map_or(0, |r| r.as_raw_fd()),
            reserved: [0],
            controls: kernel.as_mut_ptr(),
        };
        // SAFETY: the request takes a `v4l2_ext_controls`, which `raw` is.
        // `controls` points at `count` entries in `kernel`, alive for the call; each
        // entry's payload pointer comes from a `&mut [u8]` of `size` bytes that
        // `ExtControl`'s lifetime keeps borrowed, and a value control has `size` zero,
        // so nothing is followed through its union.
        let result = unsafe { ioctl_ptr(fd, self.nr, &raw mut raw) };
        for (control, entry) in controls.iter_mut().zip(kernel.iter()) {
            control.absorb(entry);
        }
        match result {
            Ok(_) => Ok(()),
            Err(source) => Err(ExtCtrlsError {
                source,
                error_idx: raw.error_idx,
            }),
        }
    }
}

fn refused(errno: i32) -> ExtCtrlsError {
    ExtCtrlsError {
        source: io::Error::from_raw_os_error(errno),
        error_idx: 0,
    }
}

/// Sets or clears `O_NONBLOCK` on a descriptor. Left alone when already so.
pub fn set_nonblocking(fd: BorrowedFd<'_>, nonblocking: bool) -> io::Result<()> {
    // SAFETY: `fcntl` with `F_GETFL` takes no pointer.
    let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    let wanted = if nonblocking {
        flags | libc::O_NONBLOCK
    } else {
        flags & !libc::O_NONBLOCK
    };
    if wanted == flags {
        return Ok(());
    }
    // SAFETY: `fcntl` with `F_SETFL` takes an integer flag set.
    if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, wanted) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
