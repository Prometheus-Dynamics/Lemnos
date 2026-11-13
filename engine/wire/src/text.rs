//! Words into arguments and replies into text, using only the shapes a device reports.
//! `lemnos-ctl` and `lemnos-orion` share it, so a device is driven the same way from either.
//!
//! An argument is one or more words: a number, `true`/`on`/`1` or `false`/`off`/`0` for a
//! bool, a word for a string, hex for bytes, `none` for an absent option, and the words of
//! each element or field in order for an array or a record. A reply is printed the same way,
//! a record with its field names.

use core::fmt::Write;

use crate::{DeviceDescription, OwnedItem, OwnedMethod, OwnedParam, OwnedShape};
use lemnos_core::ItemKind;

/// A shape as a person reads it: `u8`, `str?`, `[f32; 3]`, `{x: f32, y: f32}`.
pub fn shape_name(shape: &OwnedShape) -> String {
    match shape {
        OwnedShape::Unit => "()".to_string(),
        OwnedShape::Bool => "bool".to_string(),
        OwnedShape::U8 => "u8".to_string(),
        OwnedShape::U16 => "u16".to_string(),
        OwnedShape::U32 => "u32".to_string(),
        OwnedShape::U64 => "u64".to_string(),
        OwnedShape::I8 => "i8".to_string(),
        OwnedShape::I16 => "i16".to_string(),
        OwnedShape::I32 => "i32".to_string(),
        OwnedShape::I64 => "i64".to_string(),
        OwnedShape::F32 => "f32".to_string(),
        OwnedShape::F64 => "f64".to_string(),
        OwnedShape::Str => "str".to_string(),
        OwnedShape::Bytes => "bytes".to_string(),
        OwnedShape::Option(inner) => format!("{}?", shape_name(inner)),
        OwnedShape::Array(inner, n) => format!("[{}; {n}]", shape_name(inner)),
        OwnedShape::Record(fields) => {
            let inner = fields
                .iter()
                .map(|(name, shape)| format!("{name}: {}", shape_name(shape)))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{{{inner}}}")
        }
    }
}

fn parse<T: core::str::FromStr>(word: &str, shape: &OwnedShape) -> Result<T, String> {
    word.parse()
        .map_err(|_| format!("{word:?} isn't a {}", shape_name(shape)))
}

fn hex(word: &str) -> Result<Vec<u8>, String> {
    let bad = || format!("{word:?} isn't hex bytes");
    if !word.len().is_multiple_of(2) {
        return Err(bad());
    }
    (0..word.len())
        .step_by(2)
        .map(|i| {
            word.get(i..i + 2)
                .and_then(|b| u8::from_str_radix(b, 16).ok())
                .ok_or_else(bad)
        })
        .collect()
}

/// Encodes one shape, taking as many words as it needs.
fn encode<'w, S: AsRef<str> + 'w>(
    shape: &OwnedShape,
    words: &mut core::iter::Peekable<impl Iterator<Item = &'w S>>,
    out: &mut Vec<u8>,
) -> Result<(), String> {
    let mut next = || {
        words
            .next()
            .map(AsRef::as_ref)
            .ok_or_else(|| "not enough values".to_string())
    };
    match shape {
        OwnedShape::Unit => {}
        OwnedShape::Bool => {
            let value = match next()? {
                "true" | "on" | "1" => 1,
                "false" | "off" | "0" => 0,
                other => return Err(format!("{other:?} isn't a bool")),
            };
            out.push(value);
        }
        OwnedShape::U8 => out.push(parse::<u8>(next()?, shape)?),
        OwnedShape::U16 => out.extend(parse::<u16>(next()?, shape)?.to_le_bytes()),
        OwnedShape::U32 => out.extend(parse::<u32>(next()?, shape)?.to_le_bytes()),
        OwnedShape::U64 => out.extend(parse::<u64>(next()?, shape)?.to_le_bytes()),
        OwnedShape::I8 => out.extend(parse::<i8>(next()?, shape)?.to_le_bytes()),
        OwnedShape::I16 => out.extend(parse::<i16>(next()?, shape)?.to_le_bytes()),
        OwnedShape::I32 => out.extend(parse::<i32>(next()?, shape)?.to_le_bytes()),
        OwnedShape::I64 => out.extend(parse::<i64>(next()?, shape)?.to_le_bytes()),
        OwnedShape::F32 => out.extend(parse::<f32>(next()?, shape)?.to_le_bytes()),
        OwnedShape::F64 => out.extend(parse::<f64>(next()?, shape)?.to_le_bytes()),
        OwnedShape::Str => {
            let word = next()?;
            out.extend((word.len() as u32).to_le_bytes());
            out.extend(word.as_bytes());
        }
        OwnedShape::Bytes => {
            let bytes = hex(next()?)?;
            out.extend((bytes.len() as u32).to_le_bytes());
            out.extend(bytes);
        }
        OwnedShape::Option(inner) => match words.peek().map(|word| word.as_ref()) {
            Some("none") => {
                words.next();
                out.push(0);
            }
            _ => {
                out.push(1);
                encode(inner, words, out)?;
            }
        },
        OwnedShape::Array(inner, n) => {
            for _ in 0..*n {
                encode(inner, words, out)?;
            }
        }
        OwnedShape::Record(fields) => {
            for (_, field) in fields {
                encode(field, words, out)?;
            }
        }
    }
    Ok(())
}

/// The arguments of a method with `params`, from `words`. Every word must be used.
///
/// # Errors
///
/// A word that is not what its shape wants, too few words, or too many.
pub fn encode_args<S: AsRef<str>>(params: &[OwnedParam], words: &[S]) -> Result<Vec<u8>, String> {
    let mut words = words.iter().peekable();
    let mut out = Vec::new();
    for param in params {
        encode(&param.shape, &mut words, &mut out)?;
    }
    if words.next().is_some() {
        return Err("too many values".to_string());
    }
    Ok(out)
}

struct Cursor<'a>(&'a [u8]);

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        if self.0.len() < n {
            return Err("the reply is shorter than its shape".to_string());
        }
        let (head, tail) = self.0.split_at(n);
        self.0 = tail;
        Ok(head)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], String> {
        self.take(N)?
            .try_into()
            .map_err(|_| "the reply is shorter than its shape".to_string())
    }
}

/// A decoded value that can be scaled.
fn decode(
    shape: &OwnedShape,
    from: &mut Cursor<'_>,
    into: &mut String,
    scale: Scale,
) -> Result<(), String> {
    let number = |value: f64, into: &mut String| {
        let _ = write!(into, "{}", value * scale.0);
    };
    macro_rules! whole {
        ($ty:ty) => {{
            let value = <$ty>::from_le_bytes(from.array()?);
            if scale.0 == 1.0 {
                let _ = write!(into, "{value}");
            } else {
                number(value as f64, into);
            }
        }};
    }
    match shape {
        OwnedShape::Unit => into.push_str("ok"),
        OwnedShape::Bool => match from.array::<1>()?[0] {
            0 => into.push_str("false"),
            1 => into.push_str("true"),
            other => return Err(format!("{other} isn't a bool")),
        },
        OwnedShape::U8 => whole!(u8),
        OwnedShape::U16 => whole!(u16),
        OwnedShape::U32 => whole!(u32),
        OwnedShape::U64 => whole!(u64),
        OwnedShape::I8 => whole!(i8),
        OwnedShape::I16 => whole!(i16),
        OwnedShape::I32 => whole!(i32),
        OwnedShape::I64 => whole!(i64),
        OwnedShape::F32 => number(f64::from(f32::from_le_bytes(from.array()?)), into),
        OwnedShape::F64 => number(f64::from_le_bytes(from.array()?), into),
        OwnedShape::Str => {
            let n = u32::from_le_bytes(from.array()?) as usize;
            let text = core::str::from_utf8(from.take(n)?).map_err(|e| e.to_string())?;
            into.push_str(text);
        }
        OwnedShape::Bytes => {
            let n = u32::from_le_bytes(from.array()?) as usize;
            push_hex(from.take(n)?, into);
        }
        OwnedShape::Option(inner) => {
            if from.array::<1>()?[0] == 0 {
                into.push_str("none");
            } else {
                decode(inner, from, into, scale)?;
            }
        }
        OwnedShape::Array(inner, n) => {
            into.push('[');
            for i in 0..*n {
                if i > 0 {
                    into.push_str(", ");
                }
                decode(inner, from, into, scale)?;
            }
            into.push(']');
        }
        OwnedShape::Record(fields) => {
            into.push('{');
            for (i, (name, field)) in fields.iter().enumerate() {
                if i > 0 {
                    into.push_str(", ");
                }
                into.push_str(name);
                into.push_str(": ");
                decode(field, from, into, scale)?;
            }
            into.push('}');
        }
    }
    Ok(())
}

fn push_hex(bytes: &[u8], into: &mut String) {
    for byte in bytes {
        let _ = write!(into, "{byte:02x}");
    }
}

#[derive(Clone, Copy)]
struct Scale(f64);

/// Skips to one field of a record, for items that show just that field.
fn skip_to<'s>(
    shape: &'s OwnedShape,
    field: Option<&str>,
    from: &mut Cursor<'_>,
) -> Result<&'s OwnedShape, String> {
    let (Some(field), OwnedShape::Record(fields)) = (field, shape) else {
        return Ok(shape);
    };
    let mut discard = String::new();
    for (name, inner) in fields {
        if name == field {
            return Ok(inner);
        }
        decode(inner, from, &mut discard, Scale(1.0))?;
    }
    Err(format!("the reply has no field {field:?}"))
}

/// A reply as text, scaled and labelled as its item says. A method's whole reply, with no
/// item, is [`render_reply`].
///
/// # Errors
///
/// A reply shorter than its shape, or with bytes its shape does not allow.
pub fn render(
    shape: &OwnedShape,
    has_out_buffer: bool,
    reply: &[u8],
    item: &OwnedItem,
) -> Result<String, String> {
    let mut text = String::new();
    if has_out_buffer {
        push_hex(reply, &mut text);
        return Ok(text);
    }
    let mut from = Cursor(reply);
    let shape = skip_to(shape, item.field.as_deref(), &mut from)?;
    let scale = Scale(10f64.powi(i32::from(item.exponent)));
    decode(shape, &mut from, &mut text, scale)?;
    if !item.unit.is_empty() {
        text.push(' ');
        text.push_str(&item.unit);
    }
    Ok(text)
}

/// A method's whole reply as text, with no item's scale or unit.
///
/// # Errors
///
/// As [`render`].
pub fn render_reply(method: &OwnedMethod, reply: &[u8]) -> Result<String, String> {
    let bare = OwnedItem {
        kind: ItemKind::Value,
        name: String::new(),
        unit: String::new(),
        exponent: 0,
        min: None,
        max: None,
        contract: 0,
        method: 0,
        field: None,
    };
    render(&method.returns, method.has_out_buffer, reply, &bare)
}

/// The item named `wanted`.
///
/// # Errors
///
/// When the device has no such item.
pub fn find_item<'a>(
    description: &'a DeviceDescription,
    wanted: &str,
) -> Result<&'a OwnedItem, String> {
    description
        .items
        .iter()
        .find(|i| i.name == wanted)
        .ok_or_else(|| format!("no item {wanted:?}"))
}

/// The contract position and the method an item reads or drives.
///
/// # Errors
///
/// When the item names a method the device does not have.
pub fn method_of<'a>(
    description: &'a DeviceDescription,
    item: &OwnedItem,
) -> Result<(usize, &'a OwnedMethod), String> {
    let contract = usize::from(item.contract);
    description
        .contracts
        .get(contract)
        .and_then(|c| c.methods.iter().find(|m| m.id == item.method))
        .map(|m| (contract, m))
        .ok_or_else(|| format!("{} names a method the device doesn't have", item.name))
}

/// A method by `NAME`, or by `TAG:NAME` when more than one contract has that name.
///
/// # Errors
///
/// When no contract has it, or more than one does and no tag was given.
pub fn find_method<'a>(
    description: &'a DeviceDescription,
    wanted: &str,
) -> Result<(usize, &'a OwnedMethod), String> {
    let (tag, name) = wanted
        .rsplit_once(':')
        .map_or((None, wanted), |(tag, name)| (Some(tag), name));
    let mut found = description.contracts.iter().enumerate().flat_map(|(i, c)| {
        c.methods
            .iter()
            .filter(move |m| m.name == name && tag.is_none_or(|t| t == c.tag))
            .map(move |m| (i, m))
    });
    let first = found
        .next()
        .ok_or_else(|| format!("no method {wanted:?}"))?;
    if found.next().is_some() {
        return Err(format!(
            "{name:?} is in more than one contract; use TAG:{name}"
        ));
    }
    Ok(first)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(list: &[&str]) -> Vec<String> {
        list.iter().map(ToString::to_string).collect()
    }

    fn param(shape: OwnedShape) -> Vec<OwnedParam> {
        vec![OwnedParam {
            name: "p".to_string(),
            shape,
        }]
    }

    fn item(unit: &str, exponent: i8, field: Option<&str>) -> OwnedItem {
        OwnedItem {
            kind: ItemKind::Value,
            name: "i".to_string(),
            unit: unit.to_string(),
            exponent,
            min: None,
            max: None,
            contract: 0,
            method: 0,
            field: field.map(str::to_string),
        }
    }

    fn record() -> OwnedShape {
        OwnedShape::Record(vec![
            ("x".to_string(), OwnedShape::U8),
            ("y".to_string(), OwnedShape::F32),
        ])
    }

    #[test]
    fn words_become_the_bytes_a_shape_wants() {
        assert_eq!(
            encode_args(&param(OwnedShape::U16), &words(&["513"])).unwrap(),
            [1, 2]
        );
        assert_eq!(
            encode_args(&param(OwnedShape::Bool), &words(&["on"])).unwrap(),
            [1]
        );
        assert_eq!(
            encode_args(&param(OwnedShape::Str), &words(&["ab"])).unwrap(),
            [2, 0, 0, 0, b'a', b'b']
        );
        assert_eq!(
            encode_args(&param(OwnedShape::Bytes), &words(&["0aff"])).unwrap(),
            [2, 0, 0, 0, 0x0a, 0xff]
        );
    }

    #[test]
    fn nested_shapes_take_their_words_in_order() {
        let shape = OwnedShape::Array(Box::new(OwnedShape::U8), 2);
        assert_eq!(
            encode_args(&param(shape), &words(&["1", "2"])).unwrap(),
            [1, 2]
        );
        let optional = OwnedShape::Option(Box::new(OwnedShape::U8));
        assert_eq!(
            encode_args(&param(optional.clone()), &words(&["none"])).unwrap(),
            [0]
        );
        assert_eq!(
            encode_args(&param(optional), &words(&["7"])).unwrap(),
            [1, 7]
        );
    }

    #[test]
    fn wrong_words_are_explained() {
        assert!(encode_args(&param(OwnedShape::U8), &words(&["300"])).is_err());
        assert!(encode_args(&param(OwnedShape::U8), &words(&[])).is_err());
        assert!(encode_args(&param(OwnedShape::U8), &words(&["1", "2"])).is_err());
        assert!(encode_args(&param(OwnedShape::Bytes), &words(&["abc"])).is_err());
        assert!(encode_args(&param(OwnedShape::Bool), &words(&["maybe"])).is_err());
    }

    #[test]
    fn a_record_prints_with_its_field_names() {
        let mut reply = vec![3];
        reply.extend(1.5f32.to_le_bytes());
        assert_eq!(
            render(&record(), false, &reply, &item("", 0, None)).unwrap(),
            "{x: 3, y: 1.5}"
        );
    }

    #[test]
    fn an_item_can_show_one_field_scaled_with_its_unit() {
        let mut reply = vec![3];
        reply.extend(1.5f32.to_le_bytes());
        assert_eq!(
            render(&record(), false, &reply, &item("V", 1, Some("y"))).unwrap(),
            "15 V"
        );
    }

    #[test]
    fn an_out_buffer_prints_as_hex() {
        assert_eq!(
            render(&OwnedShape::Bytes, true, &[1, 0xab], &item("", 0, None)).unwrap(),
            "01ab"
        );
    }

    #[test]
    fn a_short_reply_is_an_error() {
        assert!(render(&OwnedShape::U32, false, &[1], &item("", 0, None)).is_err());
    }
}
