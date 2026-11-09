//! The queue kinds (`enum v4l2_buf_type`) and the memory kinds (`enum v4l2_memory`).

/// The kind of buffer queue an operation applies to (`enum v4l2_buf_type`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum BufType {
    /// Single-planar video capture.
    VideoCapture = 1,
    /// Single-planar video output.
    VideoOutput = 2,
    /// Video overlay.
    VideoOverlay = 3,
    /// Raw VBI capture.
    VbiCapture = 4,
    /// Raw VBI output.
    VbiOutput = 5,
    /// Sliced VBI capture.
    SlicedVbiCapture = 6,
    /// Sliced VBI output.
    SlicedVbiOutput = 7,
    /// Video output overlay.
    VideoOutputOverlay = 8,
    /// Multi-planar video capture.
    VideoCaptureMplane = 9,
    /// Multi-planar video output.
    VideoOutputMplane = 10,
    /// Software-defined radio capture.
    SdrCapture = 11,
    /// Software-defined radio output.
    SdrOutput = 12,
    /// Metadata capture (statistics, embedded data).
    MetaCapture = 13,
    /// Metadata output (ISP parameters).
    MetaOutput = 14,
}

impl BufType {
    /// Converts a raw `v4l2_buf_type`.
    pub fn from_raw(v: u32) -> Option<Self> {
        use BufType::*;
        Some(match v {
            1 => VideoCapture,
            2 => VideoOutput,
            3 => VideoOverlay,
            4 => VbiCapture,
            5 => VbiOutput,
            6 => SlicedVbiCapture,
            7 => SlicedVbiOutput,
            8 => VideoOutputOverlay,
            9 => VideoCaptureMplane,
            10 => VideoOutputMplane,
            11 => SdrCapture,
            12 => SdrOutput,
            13 => MetaCapture,
            14 => MetaOutput,
            _ => return None,
        })
    }

    /// The raw value.
    pub fn to_raw(self) -> u32 {
        self as u32
    }

    /// True for the multi-planar video queue types.
    pub fn is_multiplanar(self) -> bool {
        matches!(
            self,
            BufType::VideoCaptureMplane | BufType::VideoOutputMplane
        )
    }

    /// True for metadata queues.
    pub fn is_meta(self) -> bool {
        matches!(self, BufType::MetaCapture | BufType::MetaOutput)
    }

    /// True for queues userspace fills (output, as seen from the application).
    pub fn is_output(self) -> bool {
        matches!(
            self,
            BufType::VideoOutput
                | BufType::VbiOutput
                | BufType::SlicedVbiOutput
                | BufType::VideoOutputOverlay
                | BufType::VideoOutputMplane
                | BufType::SdrOutput
                | BufType::MetaOutput
        )
    }
}

/// How buffer memory is provided (`enum v4l2_memory`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum Memory {
    /// Driver-allocated buffers, mapped with `mmap` (and exportable as dma-bufs).
    Mmap = 1,
    /// User pointers. A known value only: nothing in this crate queues such a buffer,
    /// because the kernel would keep the address after the call returned.
    UserPtr = 2,
    /// Imported dma-buf file descriptors.
    DmaBuf = 4,
}

impl Memory {
    /// Converts a raw `v4l2_memory`.
    pub fn from_raw(v: u32) -> Option<Self> {
        match v {
            1 => Some(Memory::Mmap),
            2 => Some(Memory::UserPtr),
            4 => Some(Memory::DmaBuf),
            _ => None,
        }
    }
}
