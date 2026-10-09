//! Frame encoding: the length-prefixed little-endian layout and stable enum
//! codes.

use super::{MAX_FRAME, WireError, bad};
use lemnos_device::{Axis, DeviceClass, DeviceStatus, Quantity};
use lemnos_hal::ErrorKind;

/// Builds one frame.
pub(super) struct Encoder {
    buf: Vec<u8>,
}

impl Encoder {
    pub(super) fn new(kind: u16) -> Self {
        let mut buf = Vec::with_capacity(64);
        buf.extend_from_slice(&[0; 4]);
        buf.extend_from_slice(&kind.to_le_bytes());
        Self { buf }
    }

    pub(super) fn u8(&mut self, v: u8) -> &mut Self {
        self.buf.push(v);
        self
    }
    pub(super) fn u16(&mut self, v: u16) -> &mut Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub(super) fn u32(&mut self, v: u32) -> &mut Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub(super) fn u64(&mut self, v: u64) -> &mut Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub(super) fn i32(&mut self, v: i32) -> &mut Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub(super) fn i8(&mut self, v: i8) -> &mut Self {
        self.buf.push(v as u8);
        self
    }
    pub(super) fn f64(&mut self, v: f64) -> &mut Self {
        self.u64(v.to_bits())
    }
    pub(super) fn str(&mut self, v: &str) -> &mut Self {
        let len = v.len().min(u16::MAX as usize);
        self.u16(len as u16);
        self.buf.extend_from_slice(&v.as_bytes()[..len]);
        self
    }
    /// `u32 length` + bytes.
    pub(super) fn bytes(&mut self, v: &[u8]) -> &mut Self {
        self.u32(v.len() as u32);
        self.buf.extend_from_slice(v);
        self
    }
    pub(super) fn opt_u32(&mut self, v: Option<u32>) -> &mut Self {
        self.u32(v.unwrap_or(u32::MAX))
    }
    pub(super) fn opt_u16(&mut self, v: Option<u16>) -> &mut Self {
        self.u16(v.unwrap_or(u16::MAX))
    }
    pub(super) fn opt_color(&mut self, v: Option<u32>) -> &mut Self {
        match v {
            Some(c) => self.u8(1).u32(c),
            None => self.u8(0).u32(0),
        }
    }

    pub(super) fn finish(mut self) -> Vec<u8> {
        let len = (self.buf.len() - 4) as u32;
        self.buf[..4].copy_from_slice(&len.to_le_bytes());
        self.buf
    }
}

/// Reads one frame's payload.
pub(super) struct Decoder<'a> {
    pub(super) buf: &'a [u8],
}

impl<'a> Decoder<'a> {
    pub(super) fn take(&mut self, n: usize) -> Result<&'a [u8], WireError> {
        if self.buf.len() < n {
            return Err(bad("truncated frame"));
        }
        let (head, rest) = self.buf.split_at(n);
        self.buf = rest;
        Ok(head)
    }
    pub(super) fn u8(&mut self) -> Result<u8, WireError> {
        Ok(self.take(1)?[0])
    }
    pub(super) fn u16(&mut self) -> Result<u16, WireError> {
        Ok(u16::from_le_bytes(
            self.take(2)?.try_into().expect("2 bytes"),
        ))
    }
    pub(super) fn u32(&mut self) -> Result<u32, WireError> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().expect("4 bytes"),
        ))
    }
    pub(super) fn u64(&mut self) -> Result<u64, WireError> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().expect("8 bytes"),
        ))
    }
    pub(super) fn i32(&mut self) -> Result<i32, WireError> {
        Ok(i32::from_le_bytes(
            self.take(4)?.try_into().expect("4 bytes"),
        ))
    }
    pub(super) fn i8(&mut self) -> Result<i8, WireError> {
        Ok(self.u8()? as i8)
    }
    pub(super) fn f64(&mut self) -> Result<f64, WireError> {
        Ok(f64::from_bits(self.u64()?))
    }
    pub(super) fn str(&mut self) -> Result<String, WireError> {
        let len = usize::from(self.u16()?);
        String::from_utf8(self.take(len)?.to_vec()).map_err(|_| bad("string is not UTF-8"))
    }
    pub(super) fn bytes(&mut self) -> Result<Vec<u8>, WireError> {
        let n = self.u32()? as usize;
        Ok(self.take(n)?.to_vec())
    }
    pub(super) fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }
    pub(super) fn opt_u32(&mut self) -> Result<Option<u32>, WireError> {
        Ok(Some(self.u32()?).filter(|v| *v != u32::MAX))
    }
    pub(super) fn opt_u16(&mut self) -> Result<Option<u16>, WireError> {
        Ok(Some(self.u16()?).filter(|v| *v != u16::MAX))
    }
    pub(super) fn opt_color(&mut self) -> Result<Option<u32>, WireError> {
        let present = self.u8()? != 0;
        let value = self.u32()?;
        Ok(present.then_some(value))
    }
}

/// Splits complete frames off the front of `buf`: `(kind, payload, frame
/// length)` for the first one, `None` until one is complete.
/// A frame's kind, payload, and total length.
pub(super) type FrameParts<'a> = (u16, &'a [u8], usize);

pub(super) fn frame(buf: &[u8]) -> Result<Option<FrameParts<'_>>, WireError> {
    if buf.len() < 4 {
        return Ok(None);
    }
    let len = u32::from_le_bytes(buf[..4].try_into().expect("4 bytes")) as usize;
    if !(2..=MAX_FRAME).contains(&len) {
        return Err(bad(format!("frame length {len}")));
    }
    if buf.len() < 4 + len {
        return Ok(None);
    }
    let kind = u16::from_le_bytes([buf[4], buf[5]]);
    Ok(Some((kind, &buf[6..4 + len], 4 + len)))
}

// --- enum codes -------------------------------------------------------------

macro_rules! code_enum {
    ($ty:ty, $fallback:expr, [$($variant:path => $code:literal),+ $(,)?]) => {
        impl Code for $ty {
            fn code(self) -> u8 {
                match self {
                    $($variant => $code,)+
                    #[allow(unreachable_patterns)]
                    _ => 255,
                }
            }

            fn from_code(code: u8) -> Self {
                match code {
                    $($code => $variant,)+
                    _ => $fallback,
                }
            }
        }
    };
}

/// A stable wire number for an enum.
pub(super) trait Code: Sized {
    fn code(self) -> u8;
    fn from_code(code: u8) -> Self;
}

code_enum!(DeviceClass, DeviceClass::Other, [
    DeviceClass::Imu => 0, DeviceClass::Accelerometer => 1, DeviceClass::Gyroscope => 2,
    DeviceClass::Magnetometer => 3, DeviceClass::PowerMonitor => 4, DeviceClass::Temperature => 5,
    DeviceClass::Fan => 6, DeviceClass::Lens => 7, DeviceClass::Light => 8, DeviceClass::Gpio => 9,
    DeviceClass::Orientation => 10, DeviceClass::Other => 11,
]);
code_enum!(Quantity, Quantity::Mode, [
    Quantity::Acceleration => 0, Quantity::AngularRate => 1, Quantity::MagneticField => 2,
    Quantity::Voltage => 3, Quantity::Current => 4, Quantity::Power => 5, Quantity::Temperature => 6,
    Quantity::RotationalSpeed => 7, Quantity::Ratio => 8, Quantity::Position => 9, Quantity::Level => 10,
    Quantity::Mode => 11, Quantity::Color => 12, Quantity::Angle => 13, Quantity::Pressure => 14,
    Quantity::Frequency => 15,
]);
code_enum!(Axis, Axis::None, [Axis::None => 0, Axis::X => 1, Axis::Y => 2, Axis::Z => 3]);
code_enum!(DeviceStatus, DeviceStatus::Missing, [
    DeviceStatus::Available => 0, DeviceStatus::Degraded => 1, DeviceStatus::Faulted => 2,
    DeviceStatus::Missing => 3,
]);
code_enum!(ErrorKind, ErrorKind::Failed, [
    ErrorKind::NotFound => 0, ErrorKind::Unavailable => 1, ErrorKind::Busy => 2,
    ErrorKind::PermissionDenied => 3, ErrorKind::Timeout => 4, ErrorKind::InvalidInput => 5,
    ErrorKind::Unsupported => 6, ErrorKind::Configuration => 7, ErrorKind::Nack => 8,
    ErrorKind::Overrun => 9, ErrorKind::Failed => 10,
]);
