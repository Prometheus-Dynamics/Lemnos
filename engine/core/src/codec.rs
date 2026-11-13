//! How a call's arguments and result become bytes: little-endian, fixed
//! width, no padding, with a `u32` length before strings and byte slices.

use crate::{Result, Status};

/// What a value looks like on the wire, so a tool with no code for a device
/// can still read it and print it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Shape {
    Unit,
    Bool,
    U8,
    U16,
    U32,
    U64,
    I8,
    I16,
    I32,
    I64,
    F32,
    F64,
    Str,
    Bytes,
    Option(&'static Shape),
    Array(&'static Shape, u32),
    Record(&'static [Field]),
}

/// One field of a [`Shape::Record`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Field {
    pub name: &'static str,
    pub shape: Shape,
    /// An addition: a field after the base shape, which a reader from before it does not know.
    /// Set by `#[added]` on the field (and never by `#[breaking]`, which makes it base). Left out
    /// of the base hash; see [`layout`](crate::layout).
    pub added: bool,
}

/// Writes into a buffer the caller owns.
#[derive(Debug)]
pub struct Writer<'a> {
    buf: &'a mut [u8],
    len: usize,
}

impl<'a> Writer<'a> {
    pub fn new(buf: &'a mut [u8]) -> Self {
        Self { buf, len: 0 }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// What has been written so far.
    pub fn written(&self) -> &[u8] {
        &self.buf[..self.len]
    }

    /// Forgets what was written, so the buffer can be filled again from the start.
    pub const fn clear(&mut self) {
        self.len = 0;
    }

    /// The room left.
    pub fn remaining(&self) -> usize {
        self.buf.len() - self.len
    }

    pub fn write(&mut self, bytes: &[u8]) -> Result<()> {
        self.out(bytes.len())?.copy_from_slice(bytes);
        self.len += bytes.len();
        Ok(())
    }

    /// The next `capacity` bytes, for a callee to fill. Call
    /// [`advance`](Self::advance) with what was used.
    pub fn out(&mut self, capacity: usize) -> Result<&mut [u8]> {
        let end = self
            .len
            .checked_add(capacity)
            .ok_or(Status::BUFFER_TOO_SMALL)?;
        self.buf
            .get_mut(self.len..end)
            .ok_or(Status::BUFFER_TOO_SMALL)
    }

    pub fn advance(&mut self, used: usize) -> Result<()> {
        let end = self.len.checked_add(used).ok_or(Status::BUFFER_TOO_SMALL)?;
        if end > self.buf.len() {
            return Err(Status::BUFFER_TOO_SMALL);
        }
        self.len = end;
        Ok(())
    }

    /// Starts a length-prefixed block: writes a placeholder `u32` and returns where it is. Pair
    /// it with [`end_block`](Self::end_block), which writes the bytes that follow it.
    pub fn begin_block(&mut self) -> Result<usize> {
        let at = self.len;
        self.write(&[0; 4])?;
        Ok(at)
    }

    /// Ends the block [`begin_block`](Self::begin_block) started at `at`, writing its length.
    pub fn end_block(&mut self, at: usize) -> Result<()> {
        let body = self.len - at - 4;
        let body = u32::try_from(body).map_err(|_| Status::INVALID_ARGUMENT)?;
        self.buf[at..at + 4].copy_from_slice(&body.to_le_bytes());
        Ok(())
    }
}

/// Reads what a [`Writer`] wrote.
#[derive(Debug, Clone, Copy)]
pub struct Reader<'a> {
    buf: &'a [u8],
}

impl<'a> Reader<'a> {
    pub const fn new(buf: &'a [u8]) -> Self {
        Self { buf }
    }

    pub const fn remaining(&self) -> usize {
        self.buf.len()
    }

    pub fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if n > self.buf.len() {
            return Err(Status::INVALID_ARGUMENT);
        }
        let (head, tail) = self.buf.split_at(n);
        self.buf = tail;
        Ok(head)
    }

    /// Reads a block written by [`Writer::begin_block`] and [`Writer::end_block`]: its length,
    /// then exactly that many bytes, as a reader of their own.
    pub fn block(&mut self) -> Result<Reader<'a>> {
        let len = u32::from_le_bytes(self.array()?) as usize;
        self.take(len).map(Reader::new)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        let mut out = [0; N];
        out.copy_from_slice(self.take(N)?);
        Ok(out)
    }
}

/// A value that can be sent.
pub trait Encode {
    const SHAPE: Shape;
    /// True for a field added after the first form of its frame or struct: one a reader from
    /// before it does not know. Such a field is left out of the base-shape hash. See
    /// `docs/writing-a-driver.md`.
    const ADDED: bool = false;
    fn encode(&self, out: &mut Writer<'_>) -> Result<()>;
}

/// A value that can be received. Strings and byte slices borrow from the
/// message.
pub trait Decode<'a>: Sized {
    fn decode(from: &mut Reader<'a>) -> Result<Self>;
}

macro_rules! numbers {
    ($($t:ty => $shape:ident),* $(,)?) => {$(
        impl Encode for $t {
            const SHAPE: Shape = Shape::$shape;
            fn encode(&self, out: &mut Writer<'_>) -> Result<()> {
                out.write(&self.to_le_bytes())
            }
        }
        impl Decode<'_> for $t {
            fn decode(from: &mut Reader<'_>) -> Result<Self> {
                Ok(Self::from_le_bytes(from.array()?))
            }
        }
    )*};
}

numbers!(
    u8 => U8, u16 => U16, u32 => U32, u64 => U64,
    i8 => I8, i16 => I16, i32 => I32, i64 => I64,
    f32 => F32, f64 => F64,
);

impl Encode for () {
    const SHAPE: Shape = Shape::Unit;
    fn encode(&self, _out: &mut Writer<'_>) -> Result<()> {
        Ok(())
    }
}

impl Decode<'_> for () {
    fn decode(_from: &mut Reader<'_>) -> Result<Self> {
        Ok(())
    }
}

impl Encode for bool {
    const SHAPE: Shape = Shape::Bool;
    fn encode(&self, out: &mut Writer<'_>) -> Result<()> {
        out.write(&[u8::from(*self)])
    }
}

impl Decode<'_> for bool {
    fn decode(from: &mut Reader<'_>) -> Result<Self> {
        match u8::decode(from)? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(Status::INVALID_ARGUMENT),
        }
    }
}

impl Encode for [u8] {
    const SHAPE: Shape = Shape::Bytes;
    fn encode(&self, out: &mut Writer<'_>) -> Result<()> {
        let len = u32::try_from(self.len()).map_err(|_| Status::INVALID_ARGUMENT)?;
        len.encode(out)?;
        out.write(self)
    }
}

impl Encode for &[u8] {
    const SHAPE: Shape = Shape::Bytes;
    fn encode(&self, out: &mut Writer<'_>) -> Result<()> {
        (**self).encode(out)
    }
}

impl<'a> Decode<'a> for &'a [u8] {
    fn decode(from: &mut Reader<'a>) -> Result<Self> {
        let len = u32::decode(from)? as usize;
        from.take(len)
    }
}

impl Encode for &str {
    const SHAPE: Shape = Shape::Str;
    fn encode(&self, out: &mut Writer<'_>) -> Result<()> {
        self.as_bytes().encode(out)
    }
}

impl<'a> Decode<'a> for &'a str {
    fn decode(from: &mut Reader<'a>) -> Result<Self> {
        core::str::from_utf8(<&[u8]>::decode(from)?).map_err(|_| Status::INVALID_ARGUMENT)
    }
}

impl<T: Encode> Encode for Option<T> {
    const SHAPE: Shape = Shape::Option(&T::SHAPE);
    fn encode(&self, out: &mut Writer<'_>) -> Result<()> {
        match self {
            None => false.encode(out),
            Some(value) => {
                true.encode(out)?;
                value.encode(out)
            }
        }
    }
}

impl<'a, T: Decode<'a>> Decode<'a> for Option<T> {
    fn decode(from: &mut Reader<'a>) -> Result<Self> {
        if bool::decode(from)? {
            T::decode(from).map(Some)
        } else {
            Ok(None)
        }
    }
}

impl<T: Encode, const N: usize> Encode for [T; N] {
    const SHAPE: Shape = Shape::Array(&T::SHAPE, N as u32);
    fn encode(&self, out: &mut Writer<'_>) -> Result<()> {
        self.iter().try_for_each(|value| value.encode(out))
    }
}

impl<'a, T: Decode<'a> + Copy + Default, const N: usize> Decode<'a> for [T; N] {
    fn decode(from: &mut Reader<'a>) -> Result<Self> {
        let mut out = [T::default(); N];
        for slot in &mut out {
            *slot = T::decode(from)?;
        }
        Ok(out)
    }
}

/// Three values, as many devices report them.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Vector3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Encode for Vector3 {
    const SHAPE: Shape = Shape::Record(&[
        Field {
            name: "x",
            shape: Shape::F32,
            added: false,
        },
        Field {
            name: "y",
            shape: Shape::F32,
            added: false,
        },
        Field {
            name: "z",
            shape: Shape::F32,
            added: false,
        },
    ]);
    fn encode(&self, out: &mut Writer<'_>) -> Result<()> {
        let at = out.begin_block()?;
        self.x.encode(out)?;
        self.y.encode(out)?;
        self.z.encode(out)?;
        out.end_block(at)
    }
}

impl Decode<'_> for Vector3 {
    fn decode(from: &mut Reader<'_>) -> Result<Self> {
        let mut body = from.block()?;
        Ok(Self {
            x: f32::decode(&mut body)?,
            y: f32::decode(&mut body)?,
            z: f32::decode(&mut body)?,
        })
    }
}

/// A trailing value: one a frame or struct from before it does not carry. Absent on the wire
/// (nothing left to read), it decodes as `T::default()`. Its [`Encode::ADDED`] is true, so it is
/// outside the base shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Tail<T>(pub T);

impl<T: Encode> Encode for Tail<T> {
    const SHAPE: Shape = T::SHAPE;
    const ADDED: bool = true;
    fn encode(&self, out: &mut Writer<'_>) -> Result<()> {
        self.0.encode(out)
    }
}

impl<'a, T: Decode<'a> + Default> Decode<'a> for Tail<T> {
    fn decode(from: &mut Reader<'a>) -> Result<Self> {
        if from.remaining() == 0 {
            return Ok(Self(T::default()));
        }
        T::decode(from).map(Self)
    }
}
