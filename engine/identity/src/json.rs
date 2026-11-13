//! Reads the top-level string fields of a JSON object, for a slot's manifest. Nothing else of
//! JSON is needed, so this is not a general parser: a value it does not need is skipped whole.

/// The string value of `key` in the top-level object of `text`, or `None` when the key is missing,
/// is not a string, or the text is not a JSON object.
#[must_use]
pub fn string_field(text: &str, key: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut at = skip_ws(bytes, 0);
    if bytes.get(at) != Some(&b'{') {
        return None;
    }
    at += 1;
    loop {
        at = skip_ws(bytes, at);
        match bytes.get(at)? {
            b'}' => return None,
            b',' => at += 1,
            b'"' => {
                let (name, next) = string(bytes, at)?;
                at = skip_ws(bytes, next);
                if bytes.get(at) != Some(&b':') {
                    return None;
                }
                at = skip_ws(bytes, at + 1);
                if name == key && bytes.get(at) == Some(&b'"') {
                    return string(bytes, at).map(|(value, _)| value);
                }
                at = skip_value(bytes, at)?;
            }
            _ => return None,
        }
    }
}

fn skip_ws(bytes: &[u8], mut at: usize) -> usize {
    while bytes.get(at).is_some_and(u8::is_ascii_whitespace) {
        at += 1;
    }
    at
}

/// Reads a string starting at its opening quote. Returns it and the index after its closing quote.
fn string(bytes: &[u8], start: usize) -> Option<(String, usize)> {
    let mut out = Vec::new();
    let mut at = start + 1;
    loop {
        match *bytes.get(at)? {
            b'"' => return Some((String::from_utf8(out).ok()?, at + 1)),
            b'\\' => {
                let escaped = *bytes.get(at + 1)?;
                match escaped {
                    b'"' | b'\\' | b'/' => out.push(escaped),
                    b'n' => out.push(b'\n'),
                    b't' => out.push(b'\t'),
                    b'r' => out.push(b'\r'),
                    b'b' => out.push(8),
                    b'f' => out.push(12),
                    // A \uXXXX escape is refused: no fact needs one.
                    _ => return None,
                }
                at += 2;
            }
            byte => {
                out.push(byte);
                at += 1;
            }
        }
    }
}

/// Skips one value of any kind, returning the index after it.
fn skip_value(bytes: &[u8], at: usize) -> Option<usize> {
    match *bytes.get(at)? {
        b'"' => string(bytes, at).map(|(_, next)| next),
        b'{' | b'[' => {
            let mut depth = 0usize;
            let mut at = at;
            loop {
                match *bytes.get(at)? {
                    b'"' => at = string(bytes, at)?.1,
                    b'{' | b'[' => {
                        depth += 1;
                        at += 1;
                    }
                    b'}' | b']' => {
                        depth = depth.checked_sub(1)?;
                        at += 1;
                        if depth == 0 {
                            return Some(at);
                        }
                    }
                    _ => at += 1,
                }
            }
        }
        _ => {
            let mut at = at;
            while bytes
                .get(at)
                .is_some_and(|b| !matches!(b, b',' | b'}' | b']') && !b.is_ascii_whitespace())
            {
                at += 1;
            }
            Some(at)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::string_field;

    #[test]
    fn reads_top_level_strings_and_skips_the_rest() {
        let text = r#"{"id":"os-2","image":{"sha256":"ab","size":12,"tags":["x}","y"]},"product":"camera-os","version":"1.2.3 \"rc\"","last":"z"}"#;
        assert_eq!(string_field(text, "id").as_deref(), Some("os-2"));
        assert_eq!(string_field(text, "product").as_deref(), Some("camera-os"));
        assert_eq!(
            string_field(text, "version").as_deref(),
            Some("1.2.3 \"rc\"")
        );
        assert_eq!(string_field(text, "last").as_deref(), Some("z"));
    }

    #[test]
    fn refuses_what_is_not_a_string_field() {
        assert_eq!(string_field(r#"{"id":12}"#, "id"), None);
        assert_eq!(string_field(r#"{"id":"a"}"#, "missing"), None);
        assert_eq!(string_field(r#"["id"]"#, "id"), None);
        assert_eq!(string_field(r#"{"id":"unterminated"#, "id"), None);
        assert_eq!(string_field(r#"{"a":{"b":1"#, "id"), None);
    }
}
