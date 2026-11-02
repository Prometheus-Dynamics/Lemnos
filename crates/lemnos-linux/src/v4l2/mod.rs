//! V4L2 for Lemnos: memory-to-memory sessions ([`m2m`]). The device calls are
//! in `lemnos_linux_sys::v4l2`; these types are re-exported for consumers.
//!
//! Generic only: a session knows queues, formats, buffers, events and drains.
//! What a device does with the frames (which codec, which conversion) belongs
//! to the consumer.

pub mod m2m;

use std::io;
use std::path::{Path, PathBuf};

use lemnos_linux_sys::v4l2 as sys;

/// Sets control `id` to `value` on an open V4L2 descriptor (`VIDIOC_S_CTRL`) and
/// returns the value the driver kept (a driver may clamp or adjust it). Works on
/// any descriptor of a device, so a caller can set a codec control (bitrate, GOP)
/// before the session is built on the same descriptor. Plain integer controls
/// only (boolean flips, rotation, colours, bitrate); a control with a payload
/// needs the extended form.
pub fn set_control(fd: std::os::fd::BorrowedFd<'_>, id: u32, value: i32) -> io::Result<i32> {
    sys::s_ctrl(fd, id, value)
}

/// Reads control `id` from an open V4L2 descriptor (`VIDIOC_G_CTRL`).
pub fn control(fd: std::os::fd::BorrowedFd<'_>, id: u32) -> io::Result<i32> {
    sys::g_ctrl(fd, id)
}

/// Reads a node's capabilities (`VIDIOC_QUERYCAP`) without opening a session:
/// its driver name, and whether it is a memory-to-memory device and which
/// queue layout it uses ([`Capability::is_m2m`], [`Capability::is_m2m_multiplanar`]).
/// The node is opened read-only, so this does not claim the device.
pub fn query(path: impl AsRef<Path>) -> io::Result<Capability> {
    let file = std::fs::File::open(path.as_ref())?;
    sys::querycap(std::os::fd::AsFd::as_fd(&file))
}

/// Finds the first memory-to-memory node whose driver is `driver` (e.g.
/// `"rockchip-rga"`), scanning `/dev/video*` in numeric order. Nodes that
/// cannot be queried are skipped. `None` when no node matches.
pub fn find_m2m(driver: &str) -> io::Result<Option<(PathBuf, Capability)>> {
    let mut nodes: Vec<(u32, PathBuf)> = std::fs::read_dir("/dev")?
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let name = entry.file_name();
            let index = name.to_str()?.strip_prefix("video")?.parse::<u32>().ok()?;
            Some((index, entry.path()))
        })
        .collect();
    nodes.sort_by_key(|(index, _)| *index);
    for (_, path) in nodes {
        if let Some(cap) = query(&path)
            .ok()
            .filter(|cap| cap.is_m2m() && cap.driver == driver)
        {
            return Ok(Some((path, cap)));
        }
    }
    Ok(None)
}

pub use lemnos_linux_sys::v4l2::{
    BUF_CAP_SUPPORTS_DMABUF, BUF_FLAG_BFRAME, BUF_FLAG_ERROR, BUF_FLAG_KEYFRAME, BUF_FLAG_LAST,
    BUF_FLAG_PFRAME, BufType, BufferInfo, CAP_VIDEO_M2M, CAP_VIDEO_M2M_MPLANE, CID_BG_COLOR,
    CID_HFLIP, CID_ROTATE, CID_VFLIP, Capability, DEC_CMD_STOP, Dequeued, ENC_CMD_STOP, EVENT_EOS,
    EVENT_SOURCE_CHANGE, Event, Format, FormatDesc, MAX_PLANES, Memory, PixFormat, PixFormatMplane,
    PlaneFormat, PlaneInfo, PlaneMemory, QueuePlane, Rect, Requested, fourcc,
};
