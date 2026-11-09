//! Writing a value into a buffer the caller supplies, with the length it
//! needs: the caller may ask with no buffer, then call again with one large
//! enough. The surface turns its raw pointer and capacity into the slice (a
//! null pointer is `None`), so this crate stays free of unsafe code.
//!
//! All or nothing: a value that does not fit is not copied at all, so a caller
//! that ignores the status never reads a truncated value as a whole one.

use crate::Status;

/// What a write did: its status and the bytes the value needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Written {
    /// [`Status::OK`], or [`Status::BUFFER_TOO_SMALL`] when nothing was copied.
    pub status: Status,
    /// The bytes the value needs, written whether or not it fit. For text,
    /// without its terminating NUL.
    pub needed: usize,
}

/// Copies `value` into the start of `out` when it fits.
pub fn write_out(value: &[u8], out: Option<&mut [u8]>) -> Written {
    let needed = value.len();
    let status = match out {
        Some(out) if out.len() >= needed => {
            out[..needed].copy_from_slice(value);
            Status::OK
        }
        _ => Status::BUFFER_TOO_SMALL,
    };
    Written { status, needed }
}

/// Copies `text` and a terminating NUL into `out` when both fit; `needed` is
/// the text's length without the NUL.
pub fn write_text(text: &str, out: Option<&mut [u8]>) -> Written {
    let needed = text.len();
    let status = match out {
        Some(out) if out.len() > needed => {
            out[..needed].copy_from_slice(text.as_bytes());
            out[needed] = 0;
            Status::OK
        }
        _ => Status::BUFFER_TOO_SMALL,
    };
    Written { status, needed }
}
