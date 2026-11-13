//! Safe byte views of the parameter and statistics buffers.
//!
//! A caller fills a parameter buffer and reads a statistics buffer as bytes, at the offsets the
//! structures put their fields at. [`Bytes`] gives each buffer structure a safe conversion to and
//! from its bytes, so a caller needs no `unsafe` and no pointer arithmetic: it writes
//! `params.as_bytes_mut()[offset..]` or reads `StatBuf::from_bytes(buf)`.
//!
//! Mechanism: a reinterpretation of the structure's memory, not `drivers/pisp`'s field-by-field
//! encoding (its `Wire` trait, which needs no `unsafe`). That encoding is a hand-written list of
//! every field, and these bindgen structures run to thousands of fields across three generations;
//! a list written by hand would be a second copy of the generated layouts. The reinterpretation is
//! sound because each structure is `#[repr(C, packed)]` (alignment 1, no padding: [`layout`]
//! checks the sizes against the C compiler) and holds only integers and byte arrays, so every bit
//! pattern is a valid value. The module's tests check the alignment and the round trip.
//!
//! The `unsafe` blocks are the only ones in the view, each one operation with a `SAFETY` comment.

/// A buffer structure with a safe byte view. Every implementation is a packed `#[repr(C)]`
/// structure of integers and byte arrays, so [`Bytes::from_bytes`] accepts any `SIZE` bytes.
pub trait Bytes: Copy {
    /// Bytes the structure takes.
    const SIZE: usize;

    /// The structure's bytes, in memory order (little-endian fields, as the hardware reads them).
    fn as_bytes(&self) -> &[u8];

    /// The structure's bytes, writable. Any bytes written are a valid structure.
    fn as_bytes_mut(&mut self) -> &mut [u8];

    /// The structure read from the first [`Bytes::SIZE`] bytes of `bytes`; `None` if there are
    /// fewer.
    fn from_bytes(bytes: &[u8]) -> Option<Self>;
}

macro_rules! bytes_view {
    ($($t:ty),* $(,)?) => {$(
        // Alignment 1 means the structure is packed and has no padding, given that every field is
        // either such a structure or a fixed-width integer (`layout-check/field_types.py` checks
        // the fields of every generated structure). A regenerated binding that dropped `packed`
        // fails to compile here.
        const _: () = assert!(core::mem::align_of::<$t>() == 1);

        impl Bytes for $t {
            const SIZE: usize = core::mem::size_of::<$t>();

            fn as_bytes(&self) -> &[u8] {
                // SAFETY: `$t` is `repr(C, packed)` with integer and byte-array fields only, so its
                // `SIZE` bytes are all initialised and it has no padding; the slice borrows `self`.
                unsafe { core::slice::from_raw_parts((self as *const Self).cast::<u8>(), Self::SIZE) }
            }

            fn as_bytes_mut(&mut self) -> &mut [u8] {
                // SAFETY: as in `as_bytes`, and every bit pattern of `$t` is a valid value, so a
                // caller may write any bytes through the slice.
                unsafe { core::slice::from_raw_parts_mut((self as *mut Self).cast::<u8>(), Self::SIZE) }
            }

            fn from_bytes(bytes: &[u8]) -> Option<Self> {
                let src = bytes.get(..Self::SIZE)?;
                // SAFETY: `src` holds `SIZE` initialised bytes (the slice above is exactly that
                // long), every bit pattern is a valid `$t`, and `read_unaligned` needs no
                // alignment, so the read is in bounds and valid.
                Some(unsafe { core::ptr::read_unaligned(src.as_ptr().cast::<Self>()) })
            }
        }
    )*};
}

bytes_view!(
    crate::isp21::isp21_isp_params_cfg,
    crate::isp21::rkisp_isp21_stat_buffer,
    crate::isp3x::isp3x_isp_params_cfg,
    crate::isp3x::rkisp3x_isp_stat_buffer,
    crate::isp32::isp32_isp_params_cfg,
    crate::isp32::rkisp32_isp_stat_buffer,
    crate::isp32::rkisp32_lite_stat_buffer,
    crate::isp35::isp35_isp_params_cfg,
    crate::isp35::rkisp35_stat_buffer,
    crate::isp39::isp39_isp_params_cfg,
    crate::isp39::rkisp39_stat_buffer,
);
