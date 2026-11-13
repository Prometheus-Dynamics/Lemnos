//! Just enough JSON for a slot's manifest (`slot.json`) and the lists this driver writes.

use std::fmt::Write as _;

/// A parsed JSON value. Numbers keep their text: the manifest's are byte counts.
#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Number(String),
    Text(String),
    List(Vec<Json>),
    Object(Vec<(String, Json)>),
}

/// The deepest nesting accepted: a manifest is three levels deep.
const MAX_DEPTH: u32 = 16;

impl Json {
    /// Parses a whole document. `None` for anything that isn't one JSON value.
    pub fn parse(text: &str) -> Option<Self> {
        let mut parser = Parser {
            bytes: text.as_bytes(),
            at: 0,
        };
        let value = parser.value(0)?;
        parser.space();
        (parser.at == parser.bytes.len()).then_some(value)
    }

    /// The member named `key` of an object.
    pub fn get(&self, key: &str) -> Option<&Self> {
        match self {
            Self::Object(members) => members.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// A member through objects, as `verity.root_hash`.
    pub fn path(&self, path: &str) -> Option<&Self> {
        path.split('.').try_fold(self, |at, key| at.get(key))
    }

    pub fn text(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text),
            _ => None,
        }
    }

    pub fn u64(&self) -> Option<u64> {
        match self {
            Self::Number(text) => text.parse().ok(),
            _ => None,
        }
    }

    pub fn members(&self) -> &[(String, Self)] {
        match self {
            Self::Object(members) => members,
            _ => &[],
        }
    }
}

struct Parser<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Parser<'_> {
    fn space(&mut self) {
        while matches!(self.bytes.get(self.at), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.at += 1;
        }
    }

    fn eat(&mut self, byte: u8) -> bool {
        self.space();
        if self.bytes.get(self.at) == Some(&byte) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn word(&mut self, word: &str) -> bool {
        if self.bytes[self.at..].starts_with(word.as_bytes()) {
            self.at += word.len();
            true
        } else {
            false
        }
    }

    fn value(&mut self, depth: u32) -> Option<Json> {
        if depth > MAX_DEPTH {
            return None;
        }
        self.space();
        match *self.bytes.get(self.at)? {
            b'{' => {
                self.at += 1;
                let mut members = Vec::new();
                if self.eat(b'}') {
                    return Some(Json::Object(members));
                }
                loop {
                    self.space();
                    let key = self.string()?;
                    if !self.eat(b':') {
                        return None;
                    }
                    members.push((key, self.value(depth + 1)?));
                    if self.eat(b'}') {
                        return Some(Json::Object(members));
                    }
                    if !self.eat(b',') {
                        return None;
                    }
                }
            }
            b'[' => {
                self.at += 1;
                let mut items = Vec::new();
                if self.eat(b']') {
                    return Some(Json::List(items));
                }
                loop {
                    items.push(self.value(depth + 1)?);
                    if self.eat(b']') {
                        return Some(Json::List(items));
                    }
                    if !self.eat(b',') {
                        return None;
                    }
                }
            }
            b'"' => self.string().map(Json::Text),
            b't' => self.word("true").then_some(Json::Bool(true)),
            b'f' => self.word("false").then_some(Json::Bool(false)),
            b'n' => self.word("null").then_some(Json::Null),
            b'-' | b'0'..=b'9' => {
                let start = self.at;
                while matches!(
                    self.bytes.get(self.at),
                    Some(b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')
                ) {
                    self.at += 1;
                }
                let text = core::str::from_utf8(&self.bytes[start..self.at]).ok()?;
                text.parse::<f64>().ok()?;
                Some(Json::Number(text.to_string()))
            }
            _ => None,
        }
    }

    fn string(&mut self) -> Option<String> {
        if self.bytes.get(self.at) != Some(&b'"') {
            return None;
        }
        self.at += 1;
        let mut out = String::new();
        loop {
            let start = self.at;
            while !matches!(self.bytes.get(self.at), Some(b'"' | b'\\') | None) {
                self.at += 1;
            }
            out.push_str(core::str::from_utf8(&self.bytes[start..self.at]).ok()?);
            match *self.bytes.get(self.at)? {
                b'"' => {
                    self.at += 1;
                    return Some(out);
                }
                _ => {
                    self.at += 1;
                    let escaped = *self.bytes.get(self.at)?;
                    self.at += 1;
                    out.push(match escaped {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => {
                            let digits = self.bytes.get(self.at..self.at + 4)?;
                            self.at += 4;
                            let code =
                                u32::from_str_radix(core::str::from_utf8(digits).ok()?, 16).ok()?;
                            // A surrogate half stands for itself only as a replacement.
                            char::from_u32(code).unwrap_or('\u{fffd}')
                        }
                        _ => return None,
                    });
                }
            }
        }
    }
}

/// `text` as a JSON string, quotes included.
pub fn quoted(text: &str, out: &mut String) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if u32::from(c) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_manifest_reads_through_its_paths() {
        let json = Json::parse(
            r#"{"id":"a-1","verity":{"root_hash":"ab","data_blocks":12},
                "boot":{"files":{"Image":"00","overlays/x.dtbo":"11"}},"tags":[1,"x",null,true]}"#,
        )
        .unwrap();
        assert_eq!(json.path("id").and_then(Json::text), Some("a-1"));
        assert_eq!(
            json.path("verity.root_hash").and_then(Json::text),
            Some("ab")
        );
        assert_eq!(
            json.path("verity.data_blocks").and_then(Json::u64),
            Some(12)
        );
        let files = json.path("boot.files").unwrap().members();
        assert_eq!(files.len(), 2);
        assert_eq!(files[1].0, "overlays/x.dtbo");
        assert!(json.path("verity.nothing").is_none());
    }

    #[test]
    fn what_isnt_json_is_refused() {
        for bad in ["", "{", "{\"a\":}", "[1,]", "{\"a\":1} x", "\"\\q\"", "01x"] {
            assert_eq!(Json::parse(bad), None, "{bad}");
        }
        let deep = "[".repeat(40) + &"]".repeat(40);
        assert_eq!(Json::parse(&deep), None);
    }

    #[test]
    fn text_is_quoted_back_as_it_reads() {
        let mut out = String::new();
        quoted("a\"b\\c\n\u{1}é", &mut out);
        assert_eq!(out, r#""a\"b\\c\n\u0001é""#);
        assert_eq!(Json::parse(&out).unwrap().text(), Some("a\"b\\c\n\u{1}é"));
    }
}
