//! The private requests of the SDK kernel's `mvx` codec driver (`VIDIOC_G_MVX_*` and
//! `VIDIOC_S_MVX_*` on the codec's video node), as far as they are sound to issue. This crate
//! holds the `unsafe`; `lemnos-driver-k230-vpu` holds the structures and what they are for.
//!
//! # The call
//!
//! Every private request is `_IOWR('V', 192 + n, T)` on a plain structure `T` with no pointer
//! in it. The V4L2 core copies exactly the size encoded in the request number in from the
//! argument, hands the driver that kernel copy, and copies it back out. [`private`] passes a
//! pointer to the caller's byte slice only when the slice is exactly that size, so the kernel
//! reads and writes the slice and nothing else.
//!
//! # Which are issued
//!
//! Only the requests in [`ALLOWED`]. For each, both layers of the kernel driver were read to
//! see what becomes of the bytes: `mvx_v4l2_vidioc.c` (`mvx_v4l2_vidioc_default`),
//! `mvx_v4l2_session.c` (`mvx_v4l2_session_set_*`) and `mvx_session.c` (`mvx_session_set_*`).
//! For these the kernel copies fields into the session, with fixed sizes, and stores them or
//! queues them as a firmware option. No field is an index, a count or a length the kernel
//! trusts, with one exception that is bounded here:
//!
//! - `VIDIOC_S_MVX_ROI_REGIONS` (1): `mvx_v4l2_session_set_roi_regions` loops over `num_roi`
//!   regions with no bound of its own, reading the argument and writing a kernel structure of
//!   the same fixed array. [`private`] refuses a `num_roi` over the array's length
//!   ([`MAX_ROI`]).
//!
//! What the settings make the block do is the firmware's, and is confined by the block's own
//! page table, which holds only the pages of the buffers queued on the session.
//!
//! # Which are not, and why
//!
//! - `VIDIOC_S_MVX_OSD_INFO` (20): the kernel looks each of the caller's pixel formats up and
//!   dereferences the answer without checking it, so an unknown format makes it follow an
//!   error pointer. `VIDIOC_S_MVX_OSD_CONFIG` (19) goes with it.
//! - `VIDIOC_S_MVX_DSL_FRAME` (6), `_DSL_RATIO` (7), `_DSL_MODE` (9),
//!   `_MINI_FRAME_HEIGHT` (10), `_SEAMLESS_TARGET` (14), `_ENC_SRC_CROP` (16) and
//!   `_DEC_DST_CROP` (17): they change how large a picture the block writes or reads against
//!   buffers sized for the queue's format, and the kernel checks none of them against that
//!   format.
//! - `VIDIOC_S_MVX_SEI_USERDATA` (4) and `VIDIOC_S_MVX_CHR_CFG` (12): each carries a length or
//!   a count (`user_data_len`, `num_chr`) that the kernel passes on unchecked, and where it
//!   is used was not followed to the end.
//!
//! # Source
//!
//! Interface facts only, no code copied, from `github.com/ruyisdk/linux-xuantie-kernel` at
//! `7d4e1f444f461dbe3833bd99a4640e7b6c2cd529` (the kernel the Kendryte K230 Linux SDK builds
//! at its commit `fc6cc2151d54cb307be47f81815023f7ffb0eae6`), in
//! `drivers/media/platform/canaan/vpu/`: `mvx-v4l2-controls.h` (the request numbers and
//! `struct v4l2_mvx_roi_regions`), `mvx_v4l2_vidioc.c`, `mvx_v4l2_session.c` and
//! `mvx_session.c` (what each request does with its argument).
//!
//! Hardware status: untested on board. The check is `testing/board-runs/E37.md`.

#![cfg(target_os = "linux")]

use std::io;
use std::os::fd::{AsRawFd, BorrowedFd};

/// `BASE_VIDIOC_PRIVATE`: the first private request number.
pub const BASE_PRIVATE: u32 = 192;

/// The private requests that are issued, as their number after [`BASE_PRIVATE`]:
/// `G_MVX_COLORDESC`, `S_MVX_ROI_REGIONS`, `S_MVX_QP_EPR`, `S_MVX_COLORDESC`,
/// `S_MVX_RATE_CONTROL`, `S_MVX_LONG_TERM_REF`, `S_MVX_STATS_MODE`, `S_MVX_HUFF_TABLE`,
/// `S_MVX_COLOR_CONV_COEF`, `S_MVX_RGB2YUV_COLOR_CONV_COEF`.
pub const ALLOWED: [u32; 10] = [0, 1, 2, 3, 5, 8, 11, 13, 15, 18];

/// `VIDIOC_S_MVX_ROI_REGIONS`'s number after [`BASE_PRIVATE`].
pub const ROI_REGIONS: u32 = 1;
/// Bytes of `struct v4l2_mvx_roi_regions` before its array of regions.
pub const ROI_HEADER_BYTES: usize = 8;
/// Where `num_roi` is in it.
pub const ROI_COUNT_AT: usize = 7;
/// Bytes of one `struct v4l2_buffer_param_region`.
pub const ROI_REGION_BYTES: usize = 14;
/// `V4L2_MVX_MAX_FRAME_REGIONS`: the regions the array holds.
pub const MAX_ROI: usize = 16;

/// The size a request number encodes (`_IOC_SIZE`).
#[must_use]
pub const fn size_of_request(request: u32) -> usize {
    ((request >> 16) & 0x3fff) as usize
}

/// The private request number `request` encodes, when it is `_IOWR('V', 192 + n, ..)`.
#[must_use]
pub const fn private_number(request: u32) -> Option<u32> {
    let direction = request >> 30;
    let kind = (request >> 8) & 0xff;
    let number = request & 0xff;
    if direction == 3 && kind == b'V' as u32 && number >= BASE_PRIVATE {
        Some(number - BASE_PRIVATE)
    } else {
        None
    }
}

/// Whether [`private`] issues `request` with `payload`: one of [`ALLOWED`], with the payload
/// exactly the size the request encodes, and for the regions of interest a count the
/// structure's array holds.
#[must_use]
pub fn takes(request: u32, payload: &[u8]) -> bool {
    let Some(number) = private_number(request) else {
        return false;
    };
    if !ALLOWED.contains(&number) || payload.len() != size_of_request(request) {
        return false;
    }
    if number == ROI_REGIONS {
        let holds = payload.len() == ROI_HEADER_BYTES + MAX_ROI * ROI_REGION_BYTES;
        let count = payload.get(ROI_COUNT_AT).copied().map(usize::from);
        return holds && count.is_some_and(|count| count <= MAX_ROI);
    }
    true
}

/// Issues the private request `request` on the codec's video node `node`, with `payload` as
/// its structure: the kernel reads it and writes its answer back into it. `InvalidInput`,
/// and no call, unless [`takes`] says the request is issued with this payload.
pub fn private(node: BorrowedFd<'_>, request: u32, payload: &mut [u8]) -> io::Result<()> {
    if !takes(request, payload) {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let pointer = payload.as_mut_ptr();
    // SAFETY: the request is `_IOWR` with a size equal to `payload`'s length (checked by
    // `takes`), so the V4L2 core copies exactly that many bytes in from `pointer` and back
    // out to it; `payload` is a live, exclusively borrowed slice of that length for the whole
    // call, and the structures of the allowed requests hold no pointer the kernel follows.
    let ret = unsafe { libc::ioctl(node.as_raw_fd(), request as _, pointer) };
    if ret < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::fd::AsFd;

    use super::*;

    /// `_IOWR('V', 192 + n, size)`.
    fn request(n: u32, size: usize) -> u32 {
        (3 << 30) | ((size as u32) << 16) | (u32::from(b'V') << 8) | (BASE_PRIVATE + n)
    }

    const ROI_BYTES: usize = ROI_HEADER_BYTES + MAX_ROI * ROI_REGION_BYTES;

    #[test]
    fn a_request_number_says_what_it_is() {
        let roi = request(ROI_REGIONS, ROI_BYTES);
        assert_eq!(size_of_request(roi), ROI_BYTES);
        assert_eq!(private_number(roi), Some(1));
        assert_eq!(private_number(request(20, 4)), Some(20));
        // Not read-write, not a video request, not a private one.
        assert_eq!(private_number(roi & !(1 << 30)), None);
        assert_eq!(private_number(roi & !(1 << 31)), None);
        assert_eq!(
            private_number((roi & !0xff00) | (u32::from(b'M') << 8)),
            None
        );
        assert_eq!(private_number((roi & !0xff) | 191), None);
        assert_eq!((ROI_BYTES, ROI_COUNT_AT), (232, 7));
    }

    #[test]
    fn only_the_requests_read_to_be_sound_are_taken_with_exactly_their_size() {
        for n in 0..=20 {
            let allowed = ALLOWED.contains(&n);
            let size = if n == ROI_REGIONS { ROI_BYTES } else { 24 };
            let payload = vec![0u8; size];
            assert_eq!(takes(request(n, size), &payload), allowed, "request {n}");
            // A slice shorter or longer than the kernel will copy is never passed.
            assert!(
                !takes(request(n, size), &payload[..size - 1]),
                "request {n}"
            );
            assert!(!takes(request(n, size - 1), &payload), "request {n}");
        }
        for refused in [4, 6, 7, 9, 10, 12, 14, 16, 17, 19, 20] {
            assert!(!ALLOWED.contains(&refused), "request {refused}");
        }
        assert!(!takes(request(21, 8), &[0; 8]), "a request nobody knows");
    }

    #[test]
    fn a_region_count_past_the_array_never_reaches_the_kernel() {
        let roi = request(ROI_REGIONS, ROI_BYTES);
        let mut payload = vec![0u8; ROI_BYTES];
        for count in 0..=255u8 {
            payload[ROI_COUNT_AT] = count;
            assert_eq!(
                takes(roi, &payload),
                usize::from(count) <= MAX_ROI,
                "{count}"
            );
        }
        // A structure of another size is not this kernel's.
        let other = request(ROI_REGIONS, ROI_BYTES + ROI_REGION_BYTES);
        assert!(!takes(other, &vec![0u8; ROI_BYTES + ROI_REGION_BYTES]));
    }

    #[test]
    fn a_refused_request_is_not_issued_and_an_allowed_one_is() {
        // A pipe is no video node: the kernel answers that it has no such request, which
        // shows the call was made. No device node is opened.
        let (reader, _writer) = std::io::pipe().unwrap();
        let node = reader.as_fd();
        let mut payload = vec![0u8; ROI_BYTES];
        let error = private(node, request(ROI_REGIONS, ROI_BYTES), &mut payload).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::ENOTTY));
        payload[ROI_COUNT_AT] = 17;
        let error = private(node, request(ROI_REGIONS, ROI_BYTES), &mut payload).unwrap_err();
        assert_eq!(
            (error.kind(), error.raw_os_error()),
            (io::ErrorKind::InvalidInput, None)
        );
        let error = private(node, request(20, 8), &mut [0; 8]).unwrap_err();
        assert_eq!(
            (error.kind(), error.raw_os_error()),
            (io::ErrorKind::InvalidInput, None)
        );
    }
}
