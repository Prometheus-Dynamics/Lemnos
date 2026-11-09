//! V4L2 (`/dev/videoN`, `/dev/v4l-subdevN`): the uAPI structures and request
//! numbers of `linux/videodev2.h` and `linux/v4l2-subdev.h`, and the calls on them.
//! Generic device access only. The API of the parent module is built on these calls,
//! so this is the one definition of each layout and number.
//!
//! A plain request is an [`Ioctl`] paired with its argument type, called with
//! [`Ioctl::call`]. A request whose argument the kernel follows as a pointer
//! ([`ExtCtrlsIoctl`], [`subdev::g_routing`], and the buffer calls of
//! [`crate::v4l2`]) takes borrowed slices and sets the pointers itself, so everything
//! public here is safe to call.

mod calls;
mod geometry;
mod queue;
pub mod subdev;
mod uapi;

pub use crate::ioctl::{Ioctl, Pod, from_bytes, zeroed};
pub(crate) use calls::{
    BufferIoctl, VIDIOC_DQBUF, VIDIOC_PREPARE_BUF, VIDIOC_QBUF, VIDIOC_QUERYBUF,
};
pub use calls::{
    ExtControl, ExtCtrlsError, ExtCtrlsIoctl, MAX_EXT_CONTROLS, VIDIOC_G_EXT_CTRLS,
    VIDIOC_S_EXT_CTRLS, VIDIOC_TRY_EXT_CTRLS, export_buffer, set_nonblocking,
};
pub use geometry::{Fraction, Rect};
pub use queue::{BufType, Memory};
pub use uapi::*;

#[cfg(all(test, target_pointer_width = "64"))]
mod tests;
