//! A device's description as the bridge's `.call` action sends it: `call describe` on any
//! resource, with no arguments, answers this text. It is the whole of the description lemnosd
//! serves (what `lemnos-client` `describe` returns), in records, one per line.
//!
//! **The grammar (version 1).** The text is lines, each ended by one `\n`, the last included. The
//! first line is exactly `describe 1`; a reader refuses any other first line, so a change of format
//! is a new number. Every other line is one record: a record word, then zero or more pairs, each
//! preceded by exactly one space, `key=value`.
//!
//! - A record word is `[a-z_][a-z0-9_]*`; a key is `[a-z0-9_]+`.
//! - A value is either a text value, in double quotes, or a bare token `[A-Za-z0-9_.:+-]+`.
//! - Inside the quotes, `\\` is a backslash, `\"` a quote, `\n`, `\r` and `\t` a newline, a carriage
//!   return and a tab. Those are the only escapes: any other backslash sequence is an error, and so is
//!   any raw control character. Every other character, non-ASCII text included, is written as it is.
//!   The closing quote is followed by a space or the end of the line.
//! - A line has no trailing space and no empty line, is at most 4096 bytes, and holds each key at most
//!   once, with at most 64 keys.
//! - A reader ignores a key it does not know and a record whose word it does not know. Anything else
//!   is refused by name, with its line number.
//!
//! **The values.** A text value is always quoted. A number is bare, in decimal, or in hex with `0x` (an
//! address and a hash). A real number is written by its shortest round-trip form (`1.5`, `-0.0`,
//! `1e300`). A number the device does not give is an absent key, never a placeholder, and a real
//! number that is NaN or infinite is absent too. An enum word is bare (see the records). A text
//! option the device does not set is `none`; the quoted text `"none"` is a value, not the option.
//!
//! **The records.**
//!
//! - `device generation lease_seconds label kind bus address location state reason live`: `generation`
//!   is the device's description generation, the number its `lemnos.<device>.description` topic
//!   carries (absent before the device has one); `lease_seconds` is the bridge's caller lease
//!   (the hello fact `lease.seconds`), absent when the lease is off; `address` is `0xNN` or `none`;
//!   `state` is `running`, `retrying`, `off`, or `unknown` when none is given; `reason` is why the
//!   device is not running, or its last failure, and is empty when it runs; `live` is `true` when the
//!   device's running driver gave the rest of the description, and `false` when the device is not
//!   running (off, retrying, failed, or never started): then the contracts and methods are the
//!   manifest's, and there are no items, no address and no location, since only a running driver
//!   knows them.
//! - `contract index tag kind hash methods`: `index` is the contract's position, which an item's
//!   `contract` names; `hash` is hex; `methods` is the COUNT of the method records that follow for
//!   this contract, and those records are the ones whose `contract` is `index`.
//! - `method contract id name params returns out_buffer job`: `params` is each `name:shape`, joined
//!   by a space, as one quoted text; `out_buffer` and `job` are `true` or `false`.
//! - `item kind name unit unit_state exponent min max real_min real_max real_step step warn_low
//!   warn_high contract method field reading group`: `min`, `max`, `real_min`, `real_max`,
//!   `real_step`, `step`, `warn_low` and `warn_high` are absent when the device gives no such bound;
//!   `field` and `group` are quoted or `none`; `reading` is one of the reading words, or `unknown`.
//!
//! **Bounds.** A reader takes at most 60 KiB of text, 4 KiB per line, 4096 records, 64 keys per
//! record, 256 `contract` records, 2048 `method` records and 2048 `item` records. [`render`] refuses a
//! description that would pass one of them, by name, and never cuts it short; [`parse`] refuses the
//! same. The bridge's reply is held to 60 KiB too.
//!
//! Nothing here knows a kind of device: every word comes from the description. [`parse`] is the
//! reader this format is held to; [`write_records`] writes records back in the same form, so a text
//! read and written again is the same text.

use std::fmt::Write as _;

use lemnos_core::{ItemKind, Reading, UnitState};
use lemnos_wire::text::shape_name;
use lemnos_wire::{DeviceDescription, OwnedItem, OwnedMethod};

/// The first line of every description: the format's version.
pub const VERSION_LINE: &str = "describe 1";
/// The most text a reader takes.
pub const MAX_TEXT: usize = 60 * 1024;
/// The longest line a reader takes, in bytes, without its newline.
pub const MAX_LINE: usize = 4 * 1024;
/// The most records a reader takes, the version line excluded.
pub const MAX_RECORDS: usize = 4096;
/// The most keys a record holds.
pub const MAX_KEYS: usize = 64;
/// The most `contract` records.
pub const MAX_CONTRACTS: usize = 256;
/// The most `method` records.
pub const MAX_METHODS: usize = 2048;
/// The most `item` records.
pub const MAX_ITEMS: usize = 2048;

/// The text of a description: its version line, then its records, each ended by a newline.
/// `generation` is the device's description generation, from the same place its topic takes it;
/// `lease_seconds` is the caller lease the bridge runs (see the hello fact `lease.seconds`).
///
/// # Errors
///
/// When the description cannot be written in the format: a text value holds a control character,
/// or the description is over one of the [bounds](self#bounds). It is refused whole, by name.
pub fn render(
    description: &DeviceDescription,
    generation: Option<u64>,
    lease_seconds: Option<u64>,
) -> Result<String, String> {
    let mut lines = vec![device_record(description, generation, lease_seconds)?];
    if description.contracts.len() > MAX_CONTRACTS {
        return Err(too_many(
            "contracts",
            description.contracts.len(),
            MAX_CONTRACTS,
        ));
    }
    for (index, contract) in description.contracts.iter().enumerate() {
        lines.push(format!(
            "contract index={index} tag={} kind={} hash={:#018x} methods={}",
            quote(&contract.tag)?,
            quote(&contract.kind)?,
            contract.hash,
            contract.methods.len(),
        ));
    }
    let methods: usize = description.contracts.iter().map(|c| c.methods.len()).sum();
    if methods > MAX_METHODS {
        return Err(too_many("methods", methods, MAX_METHODS));
    }
    for (index, contract) in description.contracts.iter().enumerate() {
        for method in &contract.methods {
            lines.push(method_record(index, method)?);
        }
    }
    if description.items.len() > MAX_ITEMS {
        return Err(too_many("items", description.items.len(), MAX_ITEMS));
    }
    for item in &description.items {
        lines.push(item_record(item)?);
    }
    if lines.len() > MAX_RECORDS {
        return Err(too_many("records", lines.len(), MAX_RECORDS));
    }
    let mut out = String::from(VERSION_LINE);
    out.push('\n');
    for line in &lines {
        if line.len() > MAX_LINE {
            let word = line.split(' ').next().unwrap_or_default();
            return Err(format!(
                "the {word} record is {} bytes; a line takes at most {MAX_LINE}, and the description is refused whole",
                line.len()
            ));
        }
        out.push_str(line);
        out.push('\n');
    }
    if out.len() > MAX_TEXT {
        return Err(too_many("bytes of text", out.len(), MAX_TEXT));
    }
    Ok(out)
}

fn too_many(what: &str, count: usize, max: usize) -> String {
    format!(
        "the description has {count} {what}; a reader takes at most {max}, and the description is refused whole, never cut"
    )
}

/// Text in the format's quotes: the escapes the module documents, nothing else changed.
///
/// # Errors
///
/// When the text holds a control character other than tab, newline and carriage return, which the
/// format has no escape for.
pub fn quote(text: &str) -> Result<String, String> {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                return Err(format!(
                    "a text value holds the control character {c:?}, which the format has no escape for"
                ));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    Ok(out)
}

fn option_text(text: Option<&str>) -> Result<String, String> {
    text.map_or_else(|| Ok("none".to_owned()), quote)
}

/// Appends ` key=value` when the value is given.
fn number<T: std::fmt::Display>(out: &mut String, key: &str, value: Option<T>) {
    if let Some(value) = value {
        let _ = write!(out, " {key}={value}");
    }
}

/// Appends ` key=value` for a real number, when it is given and finite. Its shortest round-trip form
/// reads back to the same bits; a NaN or an infinity has no form the format takes, so it is absent.
fn real(out: &mut String, key: &str, value: Option<f64>) {
    if let Some(value) = value.filter(|v| v.is_finite()) {
        let _ = write!(out, " {key}={value:?}");
    }
}

fn state_name(state: Option<u8>) -> &'static str {
    match state {
        Some(0) => "running",
        Some(1) => "retrying",
        Some(2) => "off",
        _ => "unknown",
    }
}

fn device_record(
    description: &DeviceDescription,
    generation: Option<u64>,
    lease_seconds: Option<u64>,
) -> Result<String, String> {
    let mut out = String::from("device");
    number(&mut out, "generation", generation);
    number(&mut out, "lease_seconds", lease_seconds);
    let address = description
        .address
        .map_or_else(|| "none".to_owned(), |a| format!("{a:#04x}"));
    let _ = write!(
        out,
        " label={} kind={} bus={} address={address} location={} state={} reason={} live={}",
        quote(&description.label)?,
        quote(&description.kind)?,
        quote(&description.bus)?,
        quote(&description.location)?,
        state_name(description.state),
        quote(&description.reason)?,
        if description.live { "true" } else { "false" },
    );
    Ok(out)
}

fn method_record(contract: usize, method: &OwnedMethod) -> Result<String, String> {
    let params: Vec<String> = method
        .params
        .iter()
        .map(|param| format!("{}:{}", param.name, shape_name(&param.shape)))
        .collect();
    Ok(format!(
        "method contract={contract} id={} name={} params={} returns={} out_buffer={} job={}",
        method.id,
        quote(&method.name)?,
        quote(&params.join(" "))?,
        quote(&shape_name(&method.returns))?,
        method.has_out_buffer,
        method.job,
    ))
}

fn item_record(item: &OwnedItem) -> Result<String, String> {
    let kind = match item.kind {
        ItemKind::Value => "value",
        ItemKind::Control => "control",
        ItemKind::Action => "action",
    };
    let unit_state = match item.unit_state {
        UnitState::Known => "known",
        UnitState::Dimensionless => "dimensionless",
        UnitState::Unknown => "unknown",
    };
    let reading = match item.reading {
        Some(Reading::Number) => "number",
        Some(Reading::Range) => "range",
        Some(Reading::Switch) => "switch",
        Some(Reading::Text) => "text",
        Some(Reading::ColourArray) => "colour_array",
        Some(Reading::ThreeAxes) => "three_axes",
        None => "unknown",
    };
    let mut out = format!(
        "item kind={kind} name={} unit={} unit_state={unit_state} exponent={}",
        quote(&item.name)?,
        quote(&item.unit)?,
        item.exponent,
    );
    number(&mut out, "min", item.min);
    number(&mut out, "max", item.max);
    real(&mut out, "real_min", item.real_min);
    real(&mut out, "real_max", item.real_max);
    real(&mut out, "real_step", item.real_step);
    number(&mut out, "step", item.step);
    number(&mut out, "warn_low", item.warn_low);
    number(&mut out, "warn_high", item.warn_high);
    let _ = write!(
        out,
        " contract={} method={} field={} reading={reading} group={}",
        item.contract,
        item.method,
        option_text(item.field.as_deref())?,
        option_text(item.group.as_deref())?,
    );
    Ok(out)
}

/// One `key=value` pair of a record. `quoted` is whether the value was written in quotes, so that
/// [`write_records`] writes it the same way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub key: String,
    /// The value, unquoted and unescaped; a bare value as written.
    pub value: String,
    pub quoted: bool,
}

/// One record of a description: its word, and its pairs in the order they came.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub word: String,
    pub fields: Vec<Field>,
}

impl Record {
    /// The value of `key`, if the record has it.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|field| field.key == key)
            .map(|field| field.value.as_str())
    }
}

/// Reads a description text under the grammar the module documents: the version line, then its
/// records. Refuses any other version, and anything the grammar does not allow, by line number.
///
/// # Errors
///
/// When the text is over a bound, does not end with a newline, has a first line other than
/// [`VERSION_LINE`], or a line the grammar does not allow.
pub fn parse(text: &str) -> Result<Vec<Record>, String> {
    if text.len() > MAX_TEXT {
        return Err(too_many("bytes of text", text.len(), MAX_TEXT));
    }
    let Some(body) = text.strip_suffix('\n') else {
        return Err("the text does not end with a newline".to_owned());
    };
    let mut lines = body.split('\n');
    match lines.next() {
        Some(VERSION_LINE) => {}
        Some(other) => return Err(format!("line 1: not a version 1 description: {other:?}")),
        None => return Err("line 1: empty description".to_owned()),
    }
    let mut records: Vec<Record> = Vec::new();
    // Counted by word: contract, method, item.
    let mut counts = [0usize; 3];
    for (index, line) in lines.enumerate() {
        let number = index + 2;
        let at = |message: String| format!("line {number}: {message}");
        if line.len() > MAX_LINE {
            return Err(at(format!(
                "{} bytes; a line takes at most {MAX_LINE}",
                line.len()
            )));
        }
        let record = parse_line(line).map_err(at)?;
        let limit = match record.word.as_str() {
            "contract" => Some((0, MAX_CONTRACTS, "contract")),
            "method" => Some((1, MAX_METHODS, "method")),
            "item" => Some((2, MAX_ITEMS, "item")),
            _ => None,
        };
        if let Some((slot, max, what)) = limit {
            counts[slot] += 1;
            if counts[slot] > max {
                return Err(at(format!("more than {max} {what} records")));
            }
        }
        if records.len() == MAX_RECORDS {
            return Err(at(format!("more than {MAX_RECORDS} records")));
        }
        records.push(record);
    }
    Ok(records)
}

fn is_word(byte: u8) -> bool {
    byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'
}

fn is_bare(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b':' | b'+' | b'-')
}

/// The index just past the run of bytes from `from` that `keep` accepts.
fn run(bytes: &[u8], from: usize, keep: fn(u8) -> bool) -> usize {
    from + bytes[from..].iter().take_while(|&&b| keep(b)).count()
}

/// Reads one record line under the grammar. Errors name the column (a byte offset, from 1).
fn parse_line(line: &str) -> Result<Record, String> {
    let bytes = line.as_bytes();
    let word_end = run(bytes, 0, is_word);
    if word_end == 0 || !(bytes[0].is_ascii_lowercase() || bytes[0] == b'_') {
        return Err("no record word".to_owned());
    }
    let word = line[..word_end].to_owned();
    let mut fields: Vec<Field> = Vec::new();
    let mut at = word_end;
    while at < bytes.len() {
        if bytes[at] != b' ' {
            return Err(format!(
                "column {}: expected a space after {word:?}",
                at + 1
            ));
        }
        at += 1;
        if at == bytes.len() {
            return Err("a trailing space".to_owned());
        }
        if bytes[at] == b' ' {
            return Err(format!(
                "column {}: two spaces; one separates the pairs",
                at + 1
            ));
        }
        let key_end = run(bytes, at, is_word);
        if key_end == at || bytes.get(key_end) != Some(&b'=') {
            return Err(format!("column {}: expected key=value", at + 1));
        }
        let key = line[at..key_end].to_owned();
        at = key_end + 1;
        let (value, quoted) = if bytes.get(at) == Some(&b'"') {
            let (value, end) =
                quoted_value(line, at + 1).map_err(|e| format!("column {}: {e}", at + 1))?;
            at = end;
            if at < bytes.len() && bytes[at] != b' ' {
                return Err(format!(
                    "column {}: a closing quote is followed by a space or the end of the line",
                    at + 1
                ));
            }
            (value, true)
        } else {
            let end = run(bytes, at, is_bare);
            if end == at {
                return Err(format!("column {}: {key} has no value", at + 1));
            }
            if end < bytes.len() && bytes[end] != b' ' {
                return Err(format!(
                    "column {}: a bare value holds a character that needs quotes",
                    end + 1
                ));
            }
            let value = line[at..end].to_owned();
            at = end;
            (value, false)
        };
        if fields.iter().any(|field| field.key == key) {
            return Err(format!("the key {key:?} appears twice"));
        }
        if fields.len() == MAX_KEYS {
            return Err(format!("more than {MAX_KEYS} keys"));
        }
        fields.push(Field { key, value, quoted });
    }
    Ok(Record { word, fields })
}

/// The text after an opening quote at `start`, unescaped, and the index just past its closing quote.
fn quoted_value(line: &str, start: usize) -> Result<(String, usize), String> {
    let mut out = String::new();
    let mut chars = line[start..].char_indices();
    while let Some((offset, c)) = chars.next() {
        match c {
            '"' => return Ok((out, start + offset + 1)),
            '\\' => {
                let Some((_, escape)) = chars.next() else {
                    return Err("a backslash at the end of the line".to_owned());
                };
                out.push(match escape {
                    '\\' => '\\',
                    '"' => '"',
                    'n' => '\n',
                    'r' => '\r',
                    't' => '\t',
                    other => return Err(format!("the escape \\{other} is not one the format has")),
                });
            }
            c if c.is_control() => {
                return Err(format!("a raw control character {c:?} in a text value"));
            }
            c => out.push(c),
        }
    }
    Err("the text value is never closed".to_owned())
}

/// Writes records in the grammar: the version line, then each record, each value as it was read
/// (quoted or bare). `write_records(&parse(text)?)` is `text` for any text the reader takes in
/// canonical form, which is the form [`render`] writes.
///
/// # Errors
///
/// When a bare value is not a bare token, or a quoted one holds a control character.
pub fn write_records(records: &[Record]) -> Result<String, String> {
    let mut out = String::from(VERSION_LINE);
    out.push('\n');
    for record in records {
        out.push_str(&record.word);
        for field in &record.fields {
            out.push(' ');
            out.push_str(&field.key);
            out.push('=');
            if field.quoted {
                out.push_str(&quote(&field.value)?);
            } else {
                if field.value.is_empty() || !field.value.bytes().all(is_bare) {
                    return Err(format!("{:?} is not a bare value", field.value));
                }
                out.push_str(&field.value);
            }
        }
        out.push('\n');
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lemnos_wire::OwnedContract;

    fn item(real_min: Option<f64>, real_max: Option<f64>, real_step: Option<f64>) -> OwnedItem {
        OwnedItem {
            kind: ItemKind::Value,
            name: "level".to_owned(),
            unit: "m".to_owned(),
            unit_state: UnitState::Known,
            exponent: -2,
            min: Some(0),
            max: Some(100),
            real_min,
            real_max,
            real_step,
            contract: 0,
            method: 1,
            field: None,
            reading: Some(Reading::Number),
            step: Some(1),
            warn_low: None,
            warn_high: None,
            group: None,
        }
    }

    /// Labels and reasons that a naive format would break: spaces, `=`, quotes, backslashes,
    /// newlines, tabs, non-ASCII text, and the word `none`.
    const HARD: &[&str] = &[
        "plain",
        "two words",
        "a=b",
        "say \"hi\"",
        "back\\slash",
        "line one\nline two",
        "tab\there",
        "carriage\rreturn",
        "none",
        "ünïcødé ✓ 日本語",
        "ends with backslash \\",
        "",
    ];

    fn described(label: &str, reason: &str) -> DeviceDescription {
        DeviceDescription {
            label: label.to_owned(),
            kind: "sensor".to_owned(),
            bus: "i2c 1".to_owned(),
            address: Some(0x48),
            location: "/sys/x y".to_owned(),
            state: Some(1),
            reason: reason.to_owned(),
            live: true,
            contracts: vec![OwnedContract {
                tag: "imu/1".to_owned(),
                kind: "sensor".to_owned(),
                hash: 0xabc,
                methods: Vec::new(),
            }],
            items: vec![item(Some(-1.5), None, Some(0.25))],
        }
    }

    fn render_item(item: OwnedItem) -> Vec<Record> {
        let description = DeviceDescription {
            items: vec![item],
            live: true,
            ..DeviceDescription::default()
        };
        parse(&render(&description, None, None).expect("renders")).expect("parses")
    }

    fn method(id: u16, job: bool) -> lemnos_wire::OwnedMethod {
        lemnos_wire::OwnedMethod {
            id,
            name: format!("m{id}"),
            params: Vec::new(),
            returns: lemnos_wire::OwnedShape::Unit,
            has_out_buffer: false,
            job,
            hash: 0,
            added: false,
            added_fields: Vec::new(),
        }
    }

    #[test]
    fn an_item_record_carries_its_real_range_or_none() {
        let records = render_item(item(Some(-1.5), Some(2.0), Some(0.25)));
        let record = records.iter().find(|r| r.word == "item").expect("item");
        assert_eq!(record.get("real_min"), Some("-1.5"));
        assert_eq!(record.get("real_max"), Some("2.0"));
        assert_eq!(record.get("real_step"), Some("0.25"));
        let records = render_item(item(None, None, None));
        let record = records.iter().find(|r| r.word == "item").expect("item");
        assert_eq!(record.get("real_min"), None, "an unknown bound is absent");
        assert_eq!(record.get("real_max"), None);
        assert_eq!(record.get("real_step"), None);
        assert_eq!(record.get("min"), Some("0"));
        assert_eq!(record.get("field"), Some("none"));
    }

    /// NaN, the infinities, signed zero, subnormals, and the extremes of the exponent: a real bound
    /// is written as a number that reads back to the same bits, or it is absent. Never a placeholder.
    #[test]
    fn a_real_bound_reads_back_to_the_same_bits_or_is_absent() {
        let mut values = vec![
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            -0.0,
            0.0,
            f64::from_bits(1),
            -f64::from_bits(1),
            f64::MIN_POSITIVE,
            f64::MAX,
            f64::MIN,
            1e300,
            -1e300,
            1e-300,
            5e-324,
            1e16,
            1e-5,
            0.1,
            1.0 / 3.0,
        ];
        // A deterministic spread of bit patterns: every exponent, and NaNs with payloads.
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        for _ in 0..3000 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            values.push(f64::from_bits(state));
        }
        for value in values {
            let records = render_item(item(Some(value), Some(value), Some(value)));
            let record = records.iter().find(|r| r.word == "item").expect("item");
            for key in ["real_min", "real_max", "real_step"] {
                match record.get(key) {
                    None => assert!(!value.is_finite(), "{key} of {value:?} is absent"),
                    Some(text) => {
                        let back: f64 = text.parse().expect("a number");
                        assert!(value.is_finite(), "{key} of {value:?} is {text}");
                        assert_eq!(back.to_bits(), value.to_bits(), "{key} {text}");
                    }
                }
            }
        }
    }

    #[test]
    fn a_control_character_is_refused_by_name_never_written() {
        assert!(quote("a\u{1}b").is_err());
        assert!(quote("a\u{7f}b").is_err());
        let mut description = described("label", "reason");
        description.reason = "bad\u{1b}".to_owned();
        let error = render(&description, None, None).unwrap_err();
        assert!(error.contains("control character"), "{error}");
        assert_eq!(quote("tab\tnl\n").unwrap(), "\"tab\\tnl\\n\"");
    }

    #[test]
    fn text_round_trips_through_the_reader_whatever_it_holds() {
        for label in HARD {
            for reason in HARD {
                let text = render(&described(label, reason), Some(7), Some(60)).expect("renders");
                let records = parse(&text).expect("parses");
                let device = &records[0];
                assert_eq!(device.word, "device");
                assert_eq!(device.get("label"), Some(*label), "label {label:?}");
                assert_eq!(device.get("reason"), Some(*reason), "reason {reason:?}");
                assert_eq!(device.get("generation"), Some("7"));
                assert_eq!(device.get("location"), Some("/sys/x y"));
                assert_eq!(write_records(&records).expect("writes"), text);
            }
        }
    }

    /// Random labels and reasons, through render, the strict reader and the writer: the same text,
    /// and each value back as it went in.
    #[test]
    fn random_labels_and_reasons_round_trip() {
        const ALPHABET: &[char] = &[
            'a', 'Z', '0', '9', ' ', '=', '"', '\\', '\n', '\r', '\t', 'é', '日', '✓', '😀', '-',
            '_', '.', ':', '+', '{', '}', 'n', 'o', 'e', '\'',
        ];
        let mut state = 0x2545_f491_4f6c_dd1du64;
        let mut next = |n: usize| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state % n as u64) as usize
        };
        for _ in 0..400 {
            let mut random = |max: usize| -> String {
                let len = next(max + 1);
                (0..len).map(|_| ALPHABET[next(ALPHABET.len())]).collect()
            };
            let label = random(24);
            let reason = random(40);
            let text = render(&described(&label, &reason), Some(3), None).expect("renders");
            let records = parse(&text).expect("parses");
            assert_eq!(records[0].get("label"), Some(label.as_str()));
            assert_eq!(records[0].get("reason"), Some(reason.as_str()));
            assert_eq!(write_records(&records).expect("writes"), text);
        }
    }

    #[test]
    fn every_text_field_of_every_record_round_trips() {
        let mut description = described("l", "r");
        description.items[0].name = "a \"name\"".to_owned();
        description.items[0].unit = "°C\\".to_owned();
        description.items[0].group = Some("none".to_owned());
        description.items[0].field = Some("x=1".to_owned());
        let records =
            parse(&render(&description, None, Some(60)).expect("renders")).expect("parses");
        let item = records.iter().find(|r| r.word == "item").expect("item");
        assert_eq!(item.get("name"), Some("a \"name\""));
        assert_eq!(item.get("unit"), Some("°C\\"));
        assert_eq!(item.get("group"), Some("none"));
        assert_eq!(item.get("field"), Some("x=1"));
        assert_eq!(item.get("reading"), Some("number"));
        let contract = records
            .iter()
            .find(|r| r.word == "contract")
            .expect("contract");
        assert_eq!(contract.get("tag"), Some("imu/1"));
        assert_eq!(contract.get("hash"), Some("0x0000000000000abc"));
    }

    /// The `methods` count of each contract is the number of method records that follow it for that
    /// contract, and the method records are the contract's own.
    #[test]
    fn a_contract_count_is_its_method_records() {
        let mut description = described("l", "r");
        description.contracts = vec![
            OwnedContract {
                tag: "a/1".to_owned(),
                kind: "sensor".to_owned(),
                hash: 1,
                methods: vec![method(0, false), method(1, true)],
            },
            OwnedContract {
                tag: "b/1".to_owned(),
                kind: "other".to_owned(),
                hash: 2,
                methods: vec![method(7, false)],
            },
            OwnedContract {
                tag: "c/1".to_owned(),
                kind: "other".to_owned(),
                hash: 3,
                methods: Vec::new(),
            },
        ];
        let records = parse(&render(&description, None, None).expect("renders")).expect("parses");
        for (index, contract) in records.iter().filter(|r| r.word == "contract").enumerate() {
            let count: usize = contract.get("methods").expect("methods").parse().unwrap();
            let own = records
                .iter()
                .filter(|r| {
                    r.word == "method" && r.get("contract") == Some(index.to_string().as_str())
                })
                .count();
            assert_eq!(count, own, "contract {index}");
        }
        let methods = records.iter().filter(|r| r.word == "method").count();
        assert_eq!(methods, 3);
    }

    /// A device that is not running: its state and reason are there, `live` is false, and it has no
    /// items. Its contracts stay (the manifest's). A running one is live, and its lease shows only
    /// while the lease is on.
    #[test]
    fn a_device_that_is_not_running_is_described_with_its_state_and_reason() {
        let mut description = described("pump", "switched off");
        description.state = Some(2);
        description.live = false;
        description.items.clear();
        description.address = None;
        description.location.clear();
        let text = render(&description, Some(4), None).expect("renders");
        let records = parse(&text).expect("parses");
        let device = &records[0];
        assert_eq!(device.get("state"), Some("off"));
        assert_eq!(device.get("reason"), Some("switched off"));
        assert_eq!(device.get("live"), Some("false"));
        assert_eq!(device.get("lease_seconds"), None, "the lease is off");
        assert_eq!(device.get("address"), Some("none"));
        assert!(records.iter().all(|r| r.word != "item"));
        assert_eq!(records.iter().filter(|r| r.word == "contract").count(), 1);

        let running = render(&described("pump", ""), Some(4), Some(60)).expect("renders");
        let device = &parse(&running).expect("parses")[0];
        assert_eq!(device.get("live"), Some("true"));
        assert_eq!(device.get("lease_seconds"), Some("60"));
        assert_eq!(device.get("generation"), Some("4"));
    }

    #[test]
    fn a_device_with_no_generation_or_lease_has_neither_key() {
        let text = render(&described("pump", ""), None, None).expect("renders");
        let device = &parse(&text).expect("parses")[0];
        assert_eq!(device.get("generation"), None);
        assert_eq!(device.get("lease_seconds"), None);
    }

    #[test]
    fn a_reader_refuses_another_version_and_ignores_keys_and_records_it_does_not_know() {
        let text = render(&described("l", "r"), Some(1), Some(60)).expect("renders");
        assert!(parse(&text.replacen("describe 1", "describe 2", 1)).is_err());
        assert!(parse("").is_err());
        let extended = text.replacen("device ", "device future=\"x\" ", 1);
        let records = parse(&extended).expect("parses");
        assert_eq!(records[0].get("future"), Some("x"));
        assert_eq!(records[0].get("label"), Some("l"));
        let with_record = format!("{text}later a=1 b=\"two\"\n");
        let records = parse(&with_record).expect("an unknown record is ignored");
        assert_eq!(records.last().expect("record").word, "later");
    }

    #[test]
    fn a_text_value_of_none_is_not_the_absent_option() {
        let text = render(&described("none", "r"), None, Some(60)).expect("renders");
        let records = parse(&text).expect("parses");
        assert_eq!(records[0].get("label"), Some("none"));
        let item = records.iter().find(|r| r.word == "item").expect("item");
        assert_eq!(item.get("field"), Some("none"));
        assert!(
            records[0]
                .fields
                .iter()
                .any(|f| f.key == "label" && f.quoted)
        );
        assert!(
            records[0]
                .fields
                .iter()
                .any(|f| f.key == "address" && !f.quoted)
        );
    }

    /// Each line the grammar refuses is refused by name, with its line number.
    #[test]
    fn the_grammar_is_strict_and_names_the_line() {
        let refused = [
            "device  label=\"x\"",
            "device label=\"x\" ",
            "device label=\"x\"y",
            "device label=x y",
            "device label=",
            "device label=a\"b",
            "device label=a=b",
            "Device label=\"x\"",
            "device Label=\"x\"",
            "device label=\"\\q\"",
            "device label=\"a\tb\"",
            "device label=\"a\\",
            "device label=\"unclosed",
            "device label=\"x\" label=\"y\"",
            " device label=\"x\"",
            "device label=\"x\" ",
            "device label=\"x\"\r",
            "device label=\"caf\u{e9}\" x=\u{e9}",
            "device =\"y\"",
            "device label=\"x\" KEY=y",
            "1device",
            "",
        ];
        for line in refused {
            let text = format!("describe 1\n{line}\n");
            let error = parse(&text).expect_err(line);
            assert!(error.starts_with("line 2: "), "{line:?}: {error}");
        }
        let accepted = [
            "device label=\"x\"",
            "device label=\"\" live=true",
            "device label=\"a \\\"b\\\" \\\\ \\n \\r \\t\"",
            "device label=\"caf\u{e9} \u{65e5}\" x=-1.5e3",
            "future a=1 b=\"\"",
            "device address=0x48 state=off",
        ];
        for line in accepted {
            parse(&format!("describe 1\n{line}\n")).unwrap_or_else(|e| panic!("{line:?}: {e}"));
        }
        // The version line and the end of the text are checked too.
        assert!(parse("describe 1").unwrap_err().contains("newline"));
        assert!(parse("describe 1 \n").unwrap_err().contains("line 1"));
        assert!(parse("describe 1\n\n").unwrap_err().starts_with("line 2: "));
        assert!(parse("describe 1\n").expect("no records").is_empty());
        let long = format!("device label=\"{}\"\n", "x".repeat(MAX_LINE));
        assert!(
            parse(&format!("describe 1\n{long}"))
                .unwrap_err()
                .starts_with("line 2: ")
        );
        let keys: String = (0..=MAX_KEYS).map(|i| format!(" k{i}=1")).collect();
        let error = parse(&format!("describe 1\nlong{keys}\n")).unwrap_err();
        assert!(error.contains("more than 64 keys"), "{error}");
    }

    /// The reader enforces the bounds the consumer's reader does: the counts by word, the records,
    /// and the text itself.
    #[test]
    fn the_reader_enforces_the_bounds() {
        let contract = "contract index=0 tag=\"t\" kind=\"k\" hash=0x1 methods=0\n";
        let many = format!("describe 1\n{}", contract.repeat(MAX_CONTRACTS + 1));
        let error = parse(&many).unwrap_err();
        assert!(error.contains("more than 256 contract records"), "{error}");
        let records = format!("describe 1\n{}", "other\n".repeat(MAX_RECORDS + 1));
        assert!(
            parse(&records)
                .unwrap_err()
                .contains("more than 4096 records")
        );
        let huge = format!("describe 1\n{}", "other\n".repeat(MAX_TEXT / 6 + 1));
        assert!(parse(&huge).unwrap_err().contains("bytes of text"));
        let ok = format!("describe 1\n{}", "other\n".repeat(MAX_RECORDS));
        assert_eq!(parse(&ok).expect("4096 records").len(), MAX_RECORDS);
    }

    /// The renderer refuses, by name, a description over a bound, and never cuts one.
    #[test]
    fn the_renderer_refuses_a_description_over_a_bound() {
        let mut description = described("l", "r");
        description.contracts = (0..=MAX_CONTRACTS)
            .map(|i| OwnedContract {
                tag: format!("c/{i}"),
                kind: "other".to_owned(),
                hash: 0,
                methods: Vec::new(),
            })
            .collect();
        let error = render(&description, None, None).unwrap_err();
        assert!(
            error.contains("257 contracts") && error.contains("never cut"),
            "{error}"
        );

        let mut description = described("l", "r");
        description.items = (0..=MAX_ITEMS).map(|_| item(None, None, None)).collect();
        let error = render(&description, None, None).unwrap_err();
        assert!(error.contains("2049 items"), "{error}");

        let mut description = described("l", "r");
        description.reason = "x".repeat(MAX_LINE + 1);
        let error = render(&description, None, None).unwrap_err();
        assert!(error.contains("line takes at most"), "{error}");

        let mut description = described("l", "r");
        description.items = (0..MAX_ITEMS)
            .map(|_| {
                let mut item = item(None, None, None);
                item.name = "n".repeat(4000);
                item
            })
            .collect();
        let error = render(&description, None, None).unwrap_err();
        assert!(
            error.contains("bytes of text") || error.contains("line takes"),
            "{error}"
        );
    }

    /// The example descriptions in `docs/daemon.md` are the format: each parses, and writes back to
    /// the same text.
    #[test]
    fn the_documents_example_descriptions_round_trip() {
        let doc = include_str!("../../../docs/daemon.md");
        let mut blocks = Vec::new();
        let mut current: Option<String> = None;
        for line in doc.lines() {
            if let Some(text) = current.as_mut() {
                if line == "```" {
                    blocks.push(current.take().unwrap_or_default());
                } else {
                    text.push_str(line);
                    text.push('\n');
                }
            } else if line == "```describe" {
                current = Some(String::new());
            }
        }
        assert!(current.is_none(), "an example block is not closed");
        assert!(
            blocks.len() >= 2,
            "the document has an example for a running device and an off one"
        );
        for block in &blocks {
            let records = parse(block).unwrap_or_else(|e| panic!("{e}\n{block}"));
            assert_eq!(&write_records(&records).expect("writes"), block);
        }
    }
}
