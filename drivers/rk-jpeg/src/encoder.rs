//! The JPEG encoder's job, as `lemnos-driver-rkjpege` builds it for `rk-mpp:jpeg-encoder`.
//!
//! [`RegisterSet`] takes the caller's values and buffers and makes the [`lemnos_driver_rk_mpp::Job`]
//! for [`lemnos_driver_rk_mpp::Block::JpegEncoder`] (the 79-word file of [`crate::regs::Variant::EncoderV1`]).
//! The start register is not written by the job: the vendor kernel sets its start bit itself, last.

pub use lemnos_driver_rkjpege::job::{MAX_SIDE, READ_BACK, WRITE_WORDS};
pub use lemnos_driver_rkjpege::{Buffer, JobError, RegisterSet, SetError, Source, vpu720};
