//! V4L2 for Lemnos: memory-to-memory sessions ([`m2m`]). The device calls are
//! in `lemnos_linux_sys::v4l2`; these types are re-exported for consumers.
//!
//! Generic only: a session knows queues, formats, buffers, events and drains.
//! What a device does with the frames (which codec, which conversion) belongs
//! to the consumer.

pub mod m2m;

pub use lemnos_linux_sys::v4l2::{
    BUF_CAP_SUPPORTS_DMABUF, BUF_FLAG_BFRAME, BUF_FLAG_ERROR, BUF_FLAG_KEYFRAME, BUF_FLAG_LAST,
    BUF_FLAG_PFRAME, BufType, BufferInfo, CAP_VIDEO_M2M, CAP_VIDEO_M2M_MPLANE, Capability,
    DEC_CMD_STOP, Dequeued, ENC_CMD_STOP, EVENT_EOS, EVENT_SOURCE_CHANGE, Event, Format,
    FormatDesc, MAX_PLANES, Memory, PixFormat, PixFormatMplane, PlaneFormat, PlaneInfo,
    PlaneMemory, QueuePlane, Requested, fourcc,
};
