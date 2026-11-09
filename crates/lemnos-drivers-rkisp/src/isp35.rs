//! The ISP35 (RV1126B, the driver's `ISP_V35`): the video nodes the vendor kernel's `rkisp` driver
//! registers for it (`capture_v35.c`). The buffers' layouts are in [`crate::uapi::isp35`].
//! Written, untested on board.

use crate::isp32::nodes;

/// The video nodes an ISP35 exposes: the main path, the self path (the second, scaled output) and
/// the IQ tool's virtual output.
pub const OUTPUT_NODES: &[&str] = &[nodes::MAINPATH, nodes::SELFPATH, nodes::IQTOOL];
