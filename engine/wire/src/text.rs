//! Words into arguments and replies into text, using only the shapes a device reports.
//! `lemnos-ctl` and `lemnos-orion` share it, so a device is driven the same way from either.
//!
//! An argument is one or more words: a number, `true`/`on`/`1` or `false`/`off`/`0` for a
//! bool, a word for a string, hex for bytes, `none` for an absent option, and the words of
//! each element or field in order for an array or a record. A reply is printed the same way,
//! a record with its field names.
//!
//! A method with an `#[out]` buffer (`has_out_buffer`) takes one more word after its arguments:
//! the buffer's size in bytes. The device reads it before it writes, so it is sent always. When
//! the words leave it out, it is [`OUT_LIMIT`], the most a frame carries. Its reply is printed as
//! text when the bytes are valid UTF-8, and as hex otherwise. See [`encode_call`] and
//! [`render_reply`], which every client of the text path uses.

use core::fmt::Write;

use crate::{
    DeviceDescription, MAX_FRAME, OwnedItem, OwnedMethod, OwnedParam, OwnedShape, REPLY_OVERHEAD,
};
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
            // A record is a block: its u32 length, then its fields.
            let at = out.len();
            out.extend_from_slice(&[0; 4]);
            for (_, field) in fields {
                encode(field, words, out)?;
            }
            let body = u32::try_from(out.len() - at - 4)
                .map_err(|_| "the record is too long".to_string())?;
            out[at..at + 4].copy_from_slice(&body.to_le_bytes());
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

/// The parameters a method names when it reads that many bytes into its out buffer. The check
/// that the reply can carry them is made before the call (see [`encode_call_within`]).
pub const LENGTH_PARAMS: [&str; 2] = ["read_len", "max_len"];

/// Whether a method can be watched (subscribed to). The daemon serves a subscription only to a
/// method with no arguments and no out buffer, so `lemnos-ctl watch` says so before it asks.
///
/// # Errors
///
/// A method with arguments or an out buffer, named with what to do instead.
pub fn watchable(method: &OwnedMethod) -> Result<(), String> {
    if method.params.is_empty() && !method.has_out_buffer {
        Ok(())
    } else {
        Err(format!(
            "{} takes arguments or an out buffer, and the daemon serves watch only to a method with \
             neither: call it instead",
            method.name
        ))
    }
}

/// The most an out buffer can hold: what one frame carries beside the reply's own header. The
/// default size of a method's `#[out]` buffer, and the largest one a caller may ask for.
pub const OUT_LIMIT: u32 = (MAX_FRAME - REPLY_OVERHEAD) as u32;

/// A call ready to send: its arguments, and the size of the out buffer when the method has one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    pub args: Vec<u8>,
    /// The out buffer's capacity, sent as the last argument; `None` for a method without one.
    pub out_capacity: Option<u32>,
    /// The most the caller allows an out buffer: the size a word may ask for, and the default.
    pub largest: u32,
}

impl Call {
    /// The reply buffer the caller must give: an out-buffer reply can be as large as its
    /// capacity, so the buffer must hold that. Zero for a method without an out buffer.
    pub fn reply_len(&self) -> usize {
        self.out_capacity.map_or(0, |size| size as usize)
    }
}

/// The call for a method from `words`: its arguments, then for a method with an `#[out]`
/// buffer the buffer's size. A size word is optional and comes last; without it the capacity is
/// [`OUT_LIMIT`]. See [`encode_call_within`] for a caller whose replies must be smaller.
///
/// # Errors
///
/// As [`encode_call_within`].
pub fn encode_call<S: AsRef<str>>(method: &OwnedMethod, words: &[S]) -> Result<Call, String> {
    encode_call_within(method, words, OUT_LIMIT)
}

/// As [`encode_call`], for a caller that carries at most `largest` bytes of reply (the bridge,
/// whose transport frame is smaller than a lemnosd frame). The default capacity is `largest`,
/// and a size word above it is refused.
///
/// # Errors
///
/// A word that is not what its shape wants, too few words, too many, or a size that is not a
/// number of at most `largest` bytes.
pub fn encode_call_within<S: AsRef<str>>(
    method: &OwnedMethod,
    words: &[S],
    largest: u32,
) -> Result<Call, String> {
    let largest = largest.min(OUT_LIMIT);
    let mut words = words.iter().peekable();
    let mut args = Vec::new();
    for param in &method.params {
        // A length the reply must carry is checked before anything is sent: a device call can
        // have side effects (a bus transfer), so a length the reply cannot hold is refused here.
        if param.shape == OwnedShape::U32
            && LENGTH_PARAMS.contains(&param.name.as_str())
            && let Some(length) = words.peek().and_then(|w| w.as_ref().parse::<u32>().ok())
            && length > largest
        {
            return Err(format!(
                "{} asks for {length} bytes, more than the {largest} one reply carries: \
                 ask for less and repeat the call",
                param.name
            ));
        }
        encode(&param.shape, &mut words, &mut args)?;
    }
    let out_capacity = if method.has_out_buffer {
        let size = match words.next() {
            None => largest,
            Some(word) => word
                .as_ref()
                .parse::<u32>()
                .ok()
                .filter(|size| *size <= largest)
                .ok_or_else(|| {
                    format!(
                        "{:?} isn't a buffer size: a number of bytes, at most {largest}",
                        word.as_ref()
                    )
                })?,
        };
        args.extend(size.to_le_bytes());
        Some(size)
    } else {
        None
    };
    if words.next().is_some() {
        return Err(if method.has_out_buffer {
            "too many values: the buffer's size is the last one".to_string()
        } else {
            "too many values".to_string()
        });
    }
    Ok(Call {
        args,
        out_capacity,
        largest,
    })
}

/// An out buffer's bytes as text: as they are when they are valid UTF-8, else as hex.
fn out_text(bytes: &[u8]) -> String {
    match core::str::from_utf8(bytes) {
        Ok(text) => text.to_string(),
        Err(_) => {
            let mut hex = String::new();
            push_hex(bytes, &mut hex);
            hex
        }
    }
}

/// What a caller says when a call's reply did not fit its out buffer (`BUFFER_TOO_SMALL`): how
/// to ask for more.
pub fn too_small(call: &Call) -> String {
    let largest = call.largest;
    match call.out_capacity {
        Some(size) if size < largest => format!(
            "the reply is larger than its {size}-byte buffer; ask for more by giving a larger \
             size as the last value, up to {largest}"
        ),
        Some(_) if largest >= OUT_LIMIT => {
            format!("the reply is larger than {largest} bytes, the most a frame carries")
        }
        Some(_) => format!(
            "the reply is larger than {largest} bytes, the most this caller carries in one reply; \
             the method has no paged form"
        ),
        None => "the reply is larger than the buffer".to_string(),
    }
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
            // A record is a block: its length, then its fields, read from the block alone.
            let n = u32::from_le_bytes(from.array()?) as usize;
            let mut body = Cursor(from.take(n)?);
            into.push('{');
            for (i, (name, field)) in fields.iter().enumerate() {
                if i > 0 {
                    into.push_str(", ");
                }
                into.push_str(name);
                into.push_str(": ");
                decode(field, &mut body, into, scale)?;
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
    // The record's block length comes first.
    from.array::<4>()?;
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
    if has_out_buffer {
        return Ok(out_text(reply));
    }
    let mut text = String::new();
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
        unit_state: lemnos_core::UnitState::Unknown,
        name: String::new(),
        unit: String::new(),
        exponent: 0,
        min: None,
        max: None,
        contract: 0,
        method: 0,
        field: None,
        reading: None,
        step: None,
        warn_low: None,
        warn_high: None,
        group: None,
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
            unit_state: lemnos_core::UnitState::from_unit(unit),
            name: "i".to_string(),
            unit: unit.to_string(),
            exponent,
            min: None,
            max: None,
            contract: 0,
            method: 0,
            field: field.map(str::to_string),
            reading: None,
            step: None,
            warn_low: None,
            warn_high: None,
            group: None,
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
        // The record's block: its length (5), then x and y.
        let mut reply = 5u32.to_le_bytes().to_vec();
        reply.push(3);
        reply.extend(1.5f32.to_le_bytes());
        assert_eq!(
            render(&record(), false, &reply, &item("", 0, None)).unwrap(),
            "{x: 3, y: 1.5}"
        );
    }

    #[test]
    fn an_item_can_show_one_field_scaled_with_its_unit() {
        // The record's block: its length (5), then x and y.
        let mut reply = 5u32.to_le_bytes().to_vec();
        reply.push(3);
        reply.extend(1.5f32.to_le_bytes());
        assert_eq!(
            render(&record(), false, &reply, &item("V", 1, Some("y"))).unwrap(),
            "15 V"
        );
    }

    #[test]
    fn an_out_buffer_prints_as_text_when_it_is_utf8_and_as_hex_otherwise() {
        assert_eq!(
            render(
                &OwnedShape::Bytes,
                true,
                b"{\"slots\":[]}",
                &item("", 0, None)
            )
            .unwrap(),
            "{\"slots\":[]}"
        );
        assert_eq!(
            render(&OwnedShape::Bytes, true, &[1, 0xab], &item("", 0, None)).unwrap(),
            "01ab"
        );
    }

    fn method(params: Vec<OwnedParam>, has_out_buffer: bool) -> OwnedMethod {
        OwnedMethod {
            id: 1,
            name: "m".to_string(),
            params,
            returns: OwnedShape::Bytes,
            has_out_buffer,
            job: false,
            hash: 0,
            added: false,
            added_fields: Vec::new(),
        }
    }

    #[test]
    fn a_length_the_reply_cannot_carry_is_refused_before_the_call() {
        let read = OwnedMethod {
            id: 2,
            name: "i2c_transfer".to_string(),
            params: vec![
                OwnedParam {
                    name: "address".to_string(),
                    shape: OwnedShape::U16,
                },
                OwnedParam {
                    name: "read_len".to_string(),
                    shape: OwnedShape::U32,
                },
            ],
            returns: OwnedShape::Bytes,
            has_out_buffer: true,
            job: false,
            hash: 0,
            added: false,
            added_fields: Vec::new(),
        };
        assert!(encode_call_within(&read, &words(&["7", "60"]), 60 * 1024).is_ok());
        let err = encode_call_within(&read, &words(&["7", "61441"]), 60 * 1024).unwrap_err();
        assert!(err.contains("read_len asks for 61441 bytes"), "{err}");
        assert!(err.contains("ask for less and repeat"), "{err}");
    }

    #[test]
    fn watch_refuses_a_method_with_arguments_or_an_out_buffer_by_name() {
        assert!(watchable(&method(vec![], false)).is_ok());
        let with_args = method(param(OwnedShape::U8), false);
        assert!(
            watchable(&with_args)
                .unwrap_err()
                .contains("takes arguments or an out buffer")
        );
        assert!(watchable(&method(vec![], true)).is_err());
    }

    #[test]
    fn an_out_buffer_method_sends_its_size_last_and_the_default_is_the_frame_limit() {
        let out = method(param(OwnedShape::U8), true);
        let call = encode_call(&out, &words(&["7"])).unwrap();
        assert_eq!(call.args, [7, 0xe0, 0xff, 0x0f, 0x00]);
        assert_eq!(call.out_capacity, Some(OUT_LIMIT));
        assert_eq!(call.reply_len(), OUT_LIMIT as usize);
        // A size given as the last word replaces the default.
        let call = encode_call(&out, &words(&["7", "2048"])).unwrap();
        assert_eq!(call.args, [7, 0, 8, 0, 0]);
        assert_eq!(call.reply_len(), 2048);
        assert!(encode_call(&out, &words(&["7", "2048", "1"])).is_err());
        let too_big = (OUT_LIMIT + 1).to_string();
        assert!(encode_call(&out, &words(&["7", too_big.as_str()])).is_err());
        assert!(encode_call(&out, &words(&["7", "big"])).is_err());
        // A method with no out buffer takes no size.
        let plain = method(param(OwnedShape::U8), false);
        assert_eq!(encode_call(&plain, &words(&["7"])).unwrap().args, [7]);
        assert!(encode_call(&plain, &words(&["7", "2048"])).is_err());
        // An out buffer with no arguments: the size is the only word.
        let bare = encode_call(&method(vec![], true), &words(&[])).unwrap();
        assert_eq!(bare.args, OUT_LIMIT.to_le_bytes());
        assert!(
            too_small(&encode_call(&out, &words(&["7", "8"])).unwrap()).contains("ask for more")
        );
        assert!(too_small(&bare).contains("most a frame carries"));
        // A caller with a smaller reply limit defaults to it and refuses more.
        let within = encode_call_within(&out, &words(&["7"]), 4096).unwrap();
        assert_eq!(within.out_capacity, Some(4096));
        assert!(encode_call_within(&out, &words(&["7", "4097"]), 4096).is_err());
        assert!(too_small(&within).contains("no paged form"));
    }

    #[test]
    fn a_short_reply_is_an_error() {
        assert!(render(&OwnedShape::U32, false, &[1], &item("", 0, None)).is_err());
    }
}
