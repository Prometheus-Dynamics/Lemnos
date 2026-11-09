//! Reads one value item from a method's reply, from its shape alone. Nothing here knows
//! what the value means: a value is a number (scaled by its item's exponent) or a flag.
//! Layout follows `lemnos-ctl`: little-endian, one byte for a bool or an option tag,
//! a `u32` length before a string or a byte run.

use core::fmt;

use lemnos_wire::OwnedShape;

/// A value as Orion carries it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Reading {
    Number(f64),
    Flag(bool),
}

/// What an item publishes as, found once from its method's shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plain {
    Number,
    Flag,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadError {
    /// The reply is shorter than its shape.
    Short,
    /// The field the item names is not in the reply's record.
    NoField,
    /// A bool byte that is neither 0 nor 1.
    NotBool(u8),
    /// The shape has no plain value (see [`plain_of`]).
    Unsupported,
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Short => f.write_str("the reply is shorter than its shape"),
            Self::NoField => f.write_str("the reply has no such field"),
            Self::NotBool(byte) => write!(f, "{byte} isn't a bool"),
            Self::Unsupported => f.write_str("the shape has no plain value"),
        }
    }
}

/// The publishable kind of an item's reply, or `None` when it has none (a string, a byte run,
/// a list, a record with no such field, a unit).
pub fn plain_of(shape: &OwnedShape, field: Option<&str>) -> Option<Plain> {
    let shape = select(shape, field)?;
    match shape {
        OwnedShape::Bool => Some(Plain::Flag),
        OwnedShape::U8
        | OwnedShape::U16
        | OwnedShape::U32
        | OwnedShape::U64
        | OwnedShape::I8
        | OwnedShape::I16
        | OwnedShape::I32
        | OwnedShape::I64
        | OwnedShape::F32
        | OwnedShape::F64 => Some(Plain::Number),
        _ => None,
    }
}

/// The shape of the named field, or of the whole reply when there is no field.
fn select<'a>(shape: &'a OwnedShape, field: Option<&str>) -> Option<&'a OwnedShape> {
    let Some(field) = field else {
        return Some(shape);
    };
    let OwnedShape::Record(fields) = shape else {
        return None;
    };
    fields
        .iter()
        .find(|(name, _)| name == field)
        .map(|(_, inner)| inner)
}

/// Reads a reply. A number is scaled by `10^exponent`, as the item's unit says.
pub fn read(
    shape: &OwnedShape,
    field: Option<&str>,
    exponent: i8,
    reply: &[u8],
) -> Result<Reading, ReadError> {
    let mut cursor = Cursor(reply);
    let shape = match field {
        None => shape,
        Some(wanted) => {
            let OwnedShape::Record(fields) = shape else {
                return Err(ReadError::NoField);
            };
            let mut found = None;
            for (name, inner) in fields {
                if name == wanted {
                    found = Some(inner);
                    break;
                }
                skip(inner, &mut cursor)?;
            }
            found.ok_or(ReadError::NoField)?
        }
    };
    let scale = 10f64.powi(i32::from(exponent));
    Ok(match shape {
        OwnedShape::Bool => match cursor.take::<1>()?[0] {
            0 => Reading::Flag(false),
            1 => Reading::Flag(true),
            other => return Err(ReadError::NotBool(other)),
        },
        OwnedShape::U8 => Reading::Number(f64::from(u8::from_le_bytes(cursor.take()?)) * scale),
        OwnedShape::I8 => Reading::Number(f64::from(i8::from_le_bytes(cursor.take()?)) * scale),
        OwnedShape::U16 => Reading::Number(f64::from(u16::from_le_bytes(cursor.take()?)) * scale),
        OwnedShape::I16 => Reading::Number(f64::from(i16::from_le_bytes(cursor.take()?)) * scale),
        OwnedShape::U32 => Reading::Number(f64::from(u32::from_le_bytes(cursor.take()?)) * scale),
        OwnedShape::I32 => Reading::Number(f64::from(i32::from_le_bytes(cursor.take()?)) * scale),
        OwnedShape::F32 => Reading::Number(f64::from(f32::from_le_bytes(cursor.take()?)) * scale),
        OwnedShape::U64 => Reading::Number(u64::from_le_bytes(cursor.take()?) as f64 * scale),
        OwnedShape::I64 => Reading::Number(i64::from_le_bytes(cursor.take()?) as f64 * scale),
        OwnedShape::F64 => Reading::Number(f64::from_le_bytes(cursor.take()?) * scale),
        _ => return Err(ReadError::Unsupported),
    })
}

/// Steps over one whole value of `shape`.
fn skip(shape: &OwnedShape, cursor: &mut Cursor<'_>) -> Result<(), ReadError> {
    match shape {
        OwnedShape::Unit => Ok(()),
        OwnedShape::Bool | OwnedShape::U8 | OwnedShape::I8 => cursor.take::<1>().map(drop),
        OwnedShape::U16 | OwnedShape::I16 => cursor.take::<2>().map(drop),
        OwnedShape::U32 | OwnedShape::I32 | OwnedShape::F32 => cursor.take::<4>().map(drop),
        OwnedShape::U64 | OwnedShape::I64 | OwnedShape::F64 => cursor.take::<8>().map(drop),
        OwnedShape::Str | OwnedShape::Bytes => {
            let len = u32::from_le_bytes(cursor.take()?) as usize;
            cursor.run(len).map(drop)
        }
        OwnedShape::Option(inner) => {
            if cursor.take::<1>()?[0] == 0 {
                Ok(())
            } else {
                skip(inner, cursor)
            }
        }
        OwnedShape::Array(inner, count) => {
            for _ in 0..*count {
                skip(inner, cursor)?;
            }
            Ok(())
        }
        OwnedShape::Record(fields) => {
            for (_, inner) in fields {
                skip(inner, cursor)?;
            }
            Ok(())
        }
    }
}

struct Cursor<'a>(&'a [u8]);

impl<'a> Cursor<'a> {
    fn run(&mut self, n: usize) -> Result<&'a [u8], ReadError> {
        if self.0.len() < n {
            return Err(ReadError::Short);
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(head)
    }

    fn take<const N: usize>(&mut self) -> Result<[u8; N], ReadError> {
        self.run(N)?.try_into().map_err(|_| ReadError::Short)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(fields: &[(&str, OwnedShape)]) -> OwnedShape {
        OwnedShape::Record(
            fields
                .iter()
                .map(|(name, shape)| ((*name).to_owned(), shape.clone()))
                .collect(),
        )
    }

    #[test]
    fn a_scalar_reads_scaled_by_its_exponent() {
        let reply = 1234i32.to_le_bytes();
        assert_eq!(
            read(&OwnedShape::I32, None, -3, &reply),
            Ok(Reading::Number(1.234))
        );
        assert_eq!(
            read(&OwnedShape::I32, None, 0, &reply),
            Ok(Reading::Number(1234.0))
        );
    }

    #[test]
    fn a_bool_reads_as_a_flag_and_other_bytes_are_refused() {
        assert_eq!(
            read(&OwnedShape::Bool, None, 0, &[1]),
            Ok(Reading::Flag(true))
        );
        assert_eq!(
            read(&OwnedShape::Bool, None, 0, &[7]),
            Err(ReadError::NotBool(7))
        );
    }

    #[test]
    fn a_named_field_is_found_past_the_fields_before_it() {
        let shape = record(&[
            ("tag", OwnedShape::Str),
            ("count", OwnedShape::U16),
            ("ok", OwnedShape::Bool),
        ]);
        let mut reply = Vec::new();
        reply.extend_from_slice(&3u32.to_le_bytes());
        reply.extend_from_slice(b"abc");
        reply.extend_from_slice(&500u16.to_le_bytes());
        reply.push(1);
        assert_eq!(
            read(&shape, Some("count"), -1, &reply),
            Ok(Reading::Number(50.0))
        );
        assert_eq!(read(&shape, Some("ok"), 0, &reply), Ok(Reading::Flag(true)));
        assert_eq!(
            read(&shape, Some("nope"), 0, &reply),
            Err(ReadError::NoField)
        );
    }

    #[test]
    fn only_numbers_and_flags_are_plain() {
        assert_eq!(plain_of(&OwnedShape::F32, None), Some(Plain::Number));
        assert_eq!(plain_of(&OwnedShape::Bool, None), Some(Plain::Flag));
        assert_eq!(plain_of(&OwnedShape::Str, None), None);
        assert_eq!(plain_of(&OwnedShape::Unit, None), None);
        let shape = record(&[("x", OwnedShape::F64), ("name", OwnedShape::Str)]);
        assert_eq!(plain_of(&shape, Some("x")), Some(Plain::Number));
        assert_eq!(plain_of(&shape, Some("name")), None);
        assert_eq!(plain_of(&shape, None), None);
    }

    #[test]
    fn a_short_reply_is_an_error_not_a_panic() {
        assert_eq!(
            read(&OwnedShape::U32, None, 0, &[1, 2]),
            Err(ReadError::Short)
        );
    }
}
