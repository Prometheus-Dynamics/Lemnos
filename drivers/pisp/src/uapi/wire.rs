//! Field-by-field serialisation of the uAPI structures, little-endian as the hardware reads
//! them. This replaces a byte-view cast, so the crate needs no `unsafe`; the cost is a loop over
//! the fields instead of a pointer cast.
//!
//! [`wire_struct!`] declares a structure and generates the code. It also asserts at compile time
//! that the fields add up to the structure's size, which rules out implicit padding: the bytes
//! written are exactly the C layout, which `layout.rs` checks against the headers.

/// Writes consecutive little-endian values into a buffer. Writing past the end is dropped;
/// [`Wire::write_to`] checks the length first, so it doesn't happen.
pub struct Writer<'a> {
    buf: &'a mut [u8],
    pos: usize,
}

impl Writer<'_> {
    fn put(&mut self, bytes: &[u8]) {
        let end = self.pos + bytes.len();
        if let Some(dst) = self.buf.get_mut(self.pos..end) {
            dst.copy_from_slice(bytes);
        }
        self.pos = end;
    }
}

/// Reads consecutive little-endian values from a buffer. Reading past the end gives zeros;
/// [`Wire::read_from`] checks the length first, so it doesn't happen.
pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> [u8; N] {
        let mut out = [0; N];
        if let Some(src) = self.buf.get(self.pos..self.pos + N) {
            out.copy_from_slice(src);
        }
        self.pos += N;
        out
    }
}

/// The buffer given is shorter than the structure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BufferTooSmall {
    /// Bytes the structure takes.
    pub needed: usize,
    /// Bytes given.
    pub got: usize,
}

impl core::fmt::Display for BufferTooSmall {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "buffer of {} bytes, {} needed", self.got, self.needed)
    }
}

impl core::error::Error for BufferTooSmall {}

/// A value with the kernel's layout, readable from and writable to bytes.
pub trait Wire: Copy {
    /// The all-zero value.
    const ZERO: Self;
    /// Size in bytes.
    const SIZE: usize;

    /// Appends the value to `w`.
    fn encode(&self, w: &mut Writer<'_>);

    /// Overwrites the value in place from `r`, so a large one need not be built on the stack.
    fn decode(&mut self, r: &mut Reader<'_>);

    /// Writes the value at the start of `out`; returns its size.
    fn write_to(&self, out: &mut [u8]) -> Result<usize, BufferTooSmall> {
        if out.len() < Self::SIZE {
            return Err(BufferTooSmall {
                needed: Self::SIZE,
                got: out.len(),
            });
        }
        self.encode(&mut Writer { buf: out, pos: 0 });
        Ok(Self::SIZE)
    }

    /// Replaces the value with the one at the start of `bytes`.
    fn read_from(&mut self, bytes: &[u8]) -> Result<(), BufferTooSmall> {
        if bytes.len() < Self::SIZE {
            return Err(BufferTooSmall {
                needed: Self::SIZE,
                got: bytes.len(),
            });
        }
        self.decode(&mut Reader { buf: bytes, pos: 0 });
        Ok(())
    }
}

macro_rules! wire_int {
    ($($t:ty),*) => {$(
        impl Wire for $t {
            const ZERO: Self = 0;
            const SIZE: usize = size_of::<$t>();

            fn encode(&self, w: &mut Writer<'_>) {
                w.put(&self.to_le_bytes());
            }

            fn decode(&mut self, r: &mut Reader<'_>) {
                *self = <$t>::from_le_bytes(r.take());
            }
        }
    )*};
}

wire_int!(u8, i8, u16, i16, u32, i32, u64);

impl<T: Wire, const N: usize> Wire for [T; N] {
    const ZERO: Self = [T::ZERO; N];
    const SIZE: usize = T::SIZE * N;

    fn encode(&self, w: &mut Writer<'_>) {
        for v in self {
            v.encode(w);
        }
    }

    fn decode(&mut self, r: &mut Reader<'_>) {
        for v in self {
            v.decode(r);
        }
    }
}

/// Declares a `#[repr(C)]` uAPI structure with [`Wire`] and `Default` (all zero).
macro_rules! wire_struct {
    (
        $(#[$meta:meta])*
        pub struct $name:ident {
            $($(#[$fmeta:meta])* pub $field:ident: $ty:ty,)*
        }
    ) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        #[repr(C)]
        pub struct $name {
            $($(#[$fmeta])* pub $field: $ty,)*
        }

        impl $crate::uapi::Wire for $name {
            const ZERO: Self = Self {
                $($field: <$ty as $crate::uapi::Wire>::ZERO,)*
            };
            const SIZE: usize = 0 $(+ <$ty as $crate::uapi::Wire>::SIZE)*;

            fn encode(&self, w: &mut $crate::uapi::Writer<'_>) {
                $($crate::uapi::Wire::encode(&self.$field, w);)*
            }

            fn decode(&mut self, r: &mut $crate::uapi::Reader<'_>) {
                $($crate::uapi::Wire::decode(&mut self.$field, r);)*
            }
        }

        impl Default for $name {
            fn default() -> Self {
                <Self as $crate::uapi::Wire>::ZERO
            }
        }

        const _: () = assert!(
            size_of::<$name>() == <$name as $crate::uapi::Wire>::SIZE,
            concat!(stringify!($name), " has implicit padding"),
        );
    };
}

pub(crate) use wire_struct;

#[cfg(test)]
mod tests {
    use core::mem::offset_of;

    use super::super::*;
    use super::*;

    /// Serialises `value` and checks `bytes` appear at `offset` and nothing else is set.
    fn lands_at<T: Wire>(value: T, offset: usize, bytes: &[u8]) {
        let mut out = vec![0u8; T::SIZE];
        assert_eq!(value.write_to(&mut out), Ok(T::SIZE));
        assert_eq!(&out[offset..offset + bytes.len()], bytes, "at {offset}");
        let others = out
            .iter()
            .enumerate()
            .filter(|(i, b)| !(offset..offset + bytes.len()).contains(i) && **b != 0)
            .count();
        assert_eq!(others, 0, "stray bytes for field at {offset}");
    }

    #[test]
    fn front_end_fields_land_at_their_c_offsets() {
        let c = FeConfig {
            dirty_flags_extra: 0x0403_0201,
            ..FeConfig::default()
        };
        lands_at(c, offset_of!(FeConfig, dirty_flags_extra), &[1, 2, 3, 4]);
        let mut c = FeConfig::default();
        c.global.enables = 0x0403_0201;
        lands_at(c, offset_of!(FeConfig, global), &[1, 2, 3, 4]);
        let mut c = FeConfig::default();
        c.ch[1].output.format.stride = 0x0403_0201;
        // ch[1] + output (24) + format.stride (8).
        lands_at(c, offset_of!(FeConfig, ch) + 48 + 24 + 8, &[1, 2, 3, 4]);
        let mut c = FeConfig::default();
        c.agc_stats.row_shift = 9;
        lands_at(c, offset_of!(FeConfig, agc_stats) + 144, &[9]);
    }

    #[test]
    fn back_end_fields_land_at_their_c_offsets() {
        let t = Tile {
            output_hog_addr_offset: 0x0403_0201,
            ..Tile::default()
        };
        lands_at(t, offset_of!(Tile, output_hog_addr_offset), &[1, 2, 3, 4]);
        let c = BeTilesConfig {
            num_tiles: 0x0403_0201,
            ..BeTilesConfig::default()
        };
        lands_at(c, 16716, &[1, 2, 3, 4]);
        let mut c = BeTilesConfig::default();
        c.tiles[63].edge = 7;
        lands_at(c, 6476 + 63 * 160, &[7]);
        let mut c = BeConfig::default();
        c.sharpen.negative_limit = 0x0201;
        lands_at(c, 5324 + 226, &[1, 2]);
        let mut c = BeConfig::default();
        c.lsc.lut_packed[32][32] = 0x0403_0201;
        lands_at(c, 280 + 4 + 33 * 33 * 4 - 4, &[1, 2, 3, 4]);
        let mut c = BeConfig::default();
        c.output_format[1].hi2 = 0x0201;
        lands_at(c, 6324 + 28 + 26, &[1, 2]);
    }

    #[test]
    fn statistics_fields_land_at_their_c_offsets() {
        let mut s = RawStatistics::ZERO;
        s.cdaf.floating[3] = 0x0807_0605_0403_0201;
        lands_at(s, 23200 - 8, &[1, 2, 3, 4, 5, 6, 7, 8]);
        let mut s = RawStatistics::ZERO;
        s.agc.histogram[0] = 0x0403_0201;
        lands_at(s, 16448 + 2048, &[1, 2, 3, 4]);
    }

    #[test]
    fn a_value_round_trips() {
        let mut c = BeConfig::default();
        c.global.rgb_enables = 0x0012_3456;
        c.resample[1].coef[95] = -1234;
        c.ccm.offsets[2] = -5;
        let mut bytes = [0u8; BeConfig::SIZE];
        c.write_to(&mut bytes).unwrap();
        let mut back = BeConfig::default();
        back.read_from(&bytes).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn short_buffers_are_refused() {
        let c = FeConfig::default();
        let mut bytes = [0u8; 10];
        assert_eq!(
            c.write_to(&mut bytes),
            Err(BufferTooSmall {
                needed: FeConfig::SIZE,
                got: 10
            })
        );
        let mut r = FeConfig::default();
        assert_eq!(
            r.read_from(&bytes),
            Err(BufferTooSmall {
                needed: FeConfig::SIZE,
                got: 10
            })
        );
    }
}
