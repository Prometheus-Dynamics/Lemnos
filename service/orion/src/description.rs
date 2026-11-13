//! A device's description as the bridge's `.call` action sends it: `call describe` on any
//! resource, with no arguments, answers this text. It is the whole of the description lemnosd
//! serves (what `lemnos-client` `describe` returns), in records, one per line.
//!
//! **The format (version 1).** The first line is `describe 1`. A reader refuses any other first
//! line, so a format change is a new number. Every other line is one record: a word, then
//! `key=value` pairs separated by single spaces. A reader ignores a key it does not know, and
//! ignores a record whose word it does not know; the keys and records listed here are the ones
//! version 1 has, and a version 1 record never loses one.
//!
//! **Values.** One rule, for every value:
//!
//! - A text value is always in double quotes. Inside the quotes, `\\` is a backslash, `\"` is a
//!   quote, `\n` a newline, `\r` a carriage return and `\t` a tab; every other character, including
//!   any non-ASCII text, is written as it is. A text value never contains a raw newline.
//! - A number is written bare, in decimal, or in hex with `0x` for an address and a hash.
//! - An enum word is bare: `running`, `retrying`, `off`, `unknown`, `known`, `dimensionless`,
//!   `value`, `control`, `action`, `number`, `range`, `switch`, `text`, `colour_array`, `three_axes`,
//!   `true`, `false`.
//! - A figure the device does not give is `unknown`, and an option that is not set is `none`.
//!   A text value of `none` is quoted (`"none"`), so it is never confused with the option.
//!
//! **The records.**
//!
//! - `device generation lease_seconds label kind bus address location state reason`: `lease_seconds`
//!   is the bridge's caller lease, in seconds, when the lease is on (also the hello fact `lease.seconds`); `generation` is the
//!   device's description generation, the same number as its `lemnos.<device>.description` topic
//!   (`unknown` when the device has none yet); `address` is `0xNN` or `none`; `state` is the
//!   device's state word above (`unknown` when none is given).
//! - `contract index tag kind hash methods`: `index` is the contract's position, which an item's
//!   `contract` names; `hash` is hex.
//! - `method contract id name params returns out_buffer job`: `params` is the parameters'
//!   shapes, each `name:shape`, separated by a space, as one quoted text.
//! - `item kind name unit unit_state exponent min max real_min real_max real_step step warn_low
//!   warn_high contract method field reading group`: `real_min`, `real_max` and `real_step` are
//!   decimal numbers or `none`; `field` and `group` are quoted or `none`; `reading` is one of the
//!   reading words, or `unknown`.
//!
//! Nothing here knows a kind of device: every word comes from the description. [`parse`] is the
//! reader this format is held to; the bridge's tests read the text back with it.

use std::fmt::Write as _;

use lemnos_core::{ItemKind, Reading, UnitState};
use lemnos_wire::text::shape_name;
use lemnos_wire::{DeviceDescription, OwnedItem, OwnedMethod};

/// The first line of every description: the format's version.
pub const VERSION_LINE: &str = "describe 1";

/// The text of a description, its version line first, then its records, with no final newline.
/// `generation` is the device's description generation, from the same place its topic takes it;
/// `lease_seconds` is the caller lease the bridge runs (see the hello fact `lease.seconds`).
pub fn render(
    description: &DeviceDescription,
    generation: Option<u64>,
    lease_seconds: Option<u64>,
) -> String {
    let mut out = String::from(VERSION_LINE);
    let generation = generation.map_or_else(|| "unknown".to_owned(), |g| g.to_string());
    // The field is there only when the lease is on, as the hello document's facts are.
    let lease = lease_seconds.map_or_else(String::new, |s| format!(" lease_seconds={s}"));
    let _ = write!(
        out,
        "\ndevice generation={generation}{lease} label={} kind={} bus={} address={} location={} state={} reason={}",
        quote(&description.label),
        quote(&description.kind),
        quote(&description.bus),
        description
            .address
            .map_or_else(|| "none".to_owned(), |a| format!("{a:#04x}")),
        quote(&description.location),
        state_name(description.state),
        quote(&description.reason),
    );
    for (index, contract) in description.contracts.iter().enumerate() {
        let _ = write!(
            out,
            "\ncontract index={index} tag={} kind={} hash={:#018x} methods={}",
            quote(&contract.tag),
            quote(&contract.kind),
            contract.hash,
            contract.methods.len(),
        );
    }
    for (index, contract) in description.contracts.iter().enumerate() {
        for method in &contract.methods {
            method_record(&mut out, index, method);
        }
    }
    for item in &description.items {
        item_record(&mut out, item);
    }
    out
}

/// Text in the format's quotes: the escapes the module documents, nothing else changed.
pub fn quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn state_name(state: Option<u8>) -> &'static str {
    match state {
        Some(0) => "running",
        Some(1) => "retrying",
        Some(2) => "off",
        _ => "unknown",
    }
}

fn method_record(out: &mut String, contract: usize, method: &OwnedMethod) {
    let params: Vec<String> = method
        .params
        .iter()
        .map(|param| format!("{}:{}", param.name, shape_name(&param.shape)))
        .collect();
    let _ = write!(
        out,
        "\nmethod contract={contract} id={} name={} params={} returns={} out_buffer={} job={}",
        method.id,
        quote(&method.name),
        quote(&params.join(" ")),
        quote(&shape_name(&method.returns)),
        method.has_out_buffer,
        method.job,
    );
}

fn item_record(out: &mut String, item: &OwnedItem) {
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
    let figure = |value: Option<i64>| value.map_or_else(|| "unknown".to_owned(), |v| v.to_string());
    let real = |value: Option<f64>| value.map_or_else(|| "none".to_owned(), |v| v.to_string());
    let text = |value: Option<&str>| value.map_or_else(|| "none".to_owned(), quote);
    let _ = write!(
        out,
        "\nitem kind={kind} name={} unit={} unit_state={unit_state} exponent={} min={} max={} real_min={} real_max={} real_step={} step={} warn_low={} warn_high={} contract={} method={} field={} reading={reading} group={}",
        quote(&item.name),
        quote(&item.unit),
        item.exponent,
        figure(item.min),
        figure(item.max),
        real(item.real_min),
        real(item.real_max),
        real(item.real_step),
        figure(item.step),
        figure(item.warn_low),
        figure(item.warn_high),
        item.contract,
        item.method,
        text(item.field.as_deref()),
        text(item.group.as_deref()),
    );
}

/// One record of a description: its word, and its `key=value` pairs in the order they came. A text
/// value is unquoted and unescaped; a bare value is as written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub word: String,
    pub fields: Vec<(String, String)>,
}

impl Record {
    /// The value of `key`, if the record has it.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

/// Reads a description text: the version line, then its records. Refuses any other version.
///
/// # Errors
///
/// When the first line is not [`VERSION_LINE`], or a record is malformed.
pub fn parse(text: &str) -> Result<Vec<Record>, String> {
    let mut lines = text.split('\n');
    match lines.next() {
        Some(VERSION_LINE) => {}
        Some(other) => return Err(format!("not a version 1 description: first line {other:?}")),
        None => return Err("empty description".to_owned()),
    }
    lines.map(parse_record).collect()
}

/// Reads one record line.
///
/// # Errors
///
/// When the line is not a word followed by `key=value` pairs.
pub fn parse_record(line: &str) -> Result<Record, String> {
    let mut chars = line.chars().peekable();
    let word = take_bare(&mut chars);
    if word.is_empty() {
        return Err(format!("no record word in {line:?}"));
    }
    let mut fields = Vec::new();
    loop {
        while chars.peek() == Some(&' ') {
            chars.next();
        }
        if chars.peek().is_none() {
            break;
        }
        let key = take_until(&mut chars, '=');
        if chars.next() != Some('=') || key.is_empty() {
            return Err(format!("expected key=value in {line:?}"));
        }
        let value = if chars.peek() == Some(&'"') {
            chars.next();
            take_quoted(&mut chars).ok_or_else(|| format!("unclosed text in {line:?}"))?
        } else {
            take_bare(&mut chars)
        };
        fields.push((key, value));
    }
    Ok(Record { word, fields })
}

type Chars<'a> = std::iter::Peekable<std::str::Chars<'a>>;

fn take_bare(chars: &mut Chars<'_>) -> String {
    let mut out = String::new();
    while let Some(&c) = chars.peek() {
        if c == ' ' {
            break;
        }
        out.push(c);
        chars.next();
    }
    out
}

fn take_until(chars: &mut Chars<'_>, stop: char) -> String {
    let mut out = String::new();
    while let Some(&c) = chars.peek() {
        if c == stop || c == ' ' {
            break;
        }
        out.push(c);
        chars.next();
    }
    out
}

/// The text after an opening quote, unescaped, through its closing quote.
fn take_quoted(chars: &mut Chars<'_>) -> Option<String> {
    let mut out = String::new();
    loop {
        match chars.next()? {
            '"' => return Some(out),
            '\\' => out.push(match chars.next()? {
                '\\' => '\\',
                '"' => '"',
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                _ => return None,
            }),
            c => out.push(c),
        }
    }
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
            contracts: vec![OwnedContract {
                tag: "imu/1".to_owned(),
                kind: "sensor".to_owned(),
                hash: 0xabc,
                methods: Vec::new(),
            }],
            items: vec![item(Some(-1.5), None, Some(0.25))],
        }
    }

    #[test]
    fn an_item_record_carries_its_real_range_or_none() {
        let mut out = String::new();
        item_record(&mut out, &item(Some(-1.5), Some(2.0), Some(0.25)));
        let record = parse_record(out.trim_start_matches('\n')).expect("record");
        assert_eq!(record.get("real_min"), Some("-1.5"));
        assert_eq!(record.get("real_max"), Some("2"));
        assert_eq!(record.get("real_step"), Some("0.25"));
        let mut out = String::new();
        item_record(&mut out, &item(None, None, None));
        let record = parse_record(out.trim_start_matches('\n')).expect("record");
        assert_eq!(record.get("real_min"), Some("none"));
        assert_eq!(record.get("real_max"), Some("none"));
        assert_eq!(record.get("real_step"), Some("none"));
    }

    #[test]
    fn text_round_trips_through_the_reader_whatever_it_holds() {
        for label in HARD {
            for reason in HARD {
                let text = render(&described(label, reason), Some(7), Some(60));
                let records = parse(&text).expect("parses");
                let device = &records[0];
                assert_eq!(device.word, "device");
                assert_eq!(device.get("label"), Some(*label), "label {label:?}");
                assert_eq!(device.get("reason"), Some(*reason), "reason {reason:?}");
                assert_eq!(device.get("generation"), Some("7"));
                assert_eq!(device.get("location"), Some("/sys/x y"));
            }
        }
    }

    #[test]
    fn every_text_field_of_every_record_round_trips() {
        let mut description = described("l", "r");
        description.items[0].name = "a \"name\"".to_owned();
        description.items[0].unit = "°C\\".to_owned();
        description.items[0].group = Some("none".to_owned());
        description.items[0].field = Some("x=1".to_owned());
        let records = parse(&render(&description, None, Some(60))).expect("parses");
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

    #[test]
    fn a_reader_refuses_another_version_and_ignores_keys_it_does_not_know() {
        let text = render(&described("l", "r"), Some(1), Some(60));
        assert!(parse(&text.replacen("describe 1", "describe 2", 1)).is_err());
        assert!(parse("").is_err());
        let extended = text.replacen("device ", "device future=\"x\" ", 1);
        let records = parse(&extended).expect("parses");
        assert_eq!(records[0].get("future"), Some("x"));
        assert_eq!(records[0].get("label"), Some("l"));
    }

    #[test]
    fn a_text_value_of_none_is_not_the_absent_option() {
        let text = render(&described("none", "r"), None, Some(60));
        let records = parse(&text).expect("parses");
        assert_eq!(records[0].get("generation"), Some("unknown"));
        assert_eq!(records[0].get("label"), Some("none"));
        let item = records.iter().find(|r| r.word == "item").expect("item");
        assert_eq!(item.get("field"), Some("none"));
    }
}
