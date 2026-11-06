//! V4L2 (`/dev/videoN`, `/dev/v4l-subdevN`): the uAPI structures and request
//! numbers of `linux/videodev2.h` and `linux/v4l2-subdev.h`, and the calls on them.
//! Generic device access only. The memory-to-memory API of the parent module is
//! built on these calls, so this is the one definition of each layout and number.
//!
//! A plain request is an [`Ioctl`] paired with its argument type, called with
//! [`Ioctl::call`]. A request whose argument the kernel follows as a pointer
//! ([`BufferIoctl`], [`ExtCtrlsIoctl`], [`subdev::g_routing`]) takes borrowed slices
//! and sets the pointers itself, so everything public here is safe to call.

mod calls;
mod geometry;
mod queue;
pub mod subdev;
mod uapi;

pub use crate::ioctl::{Ioctl, Pod, from_bytes, zeroed};
pub use calls::{
    BufferIoctl, ExtControl, ExtCtrlsError, ExtCtrlsIoctl, VIDIOC_DQBUF, VIDIOC_G_EXT_CTRLS,
    VIDIOC_PREPARE_BUF, VIDIOC_QBUF, VIDIOC_QUERYBUF, VIDIOC_S_EXT_CTRLS, VIDIOC_TRY_EXT_CTRLS,
    export_buffer, set_nonblocking,
};
pub use geometry::{Fraction, Rect};
pub use queue::{BufType, Memory};
pub use uapi::*;

#[cfg(all(test, target_pointer_width = "64"))]
mod tests;
