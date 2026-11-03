use lemnos_core::{Value, ValueMap};

pub const MAX_RETAINED_OUTPUT_BYTES: usize = 64;
pub const OUTPUT_BYTES_PREVIEW_KIND: &str = "bytes-preview";
pub const OUTPUT_KIND: &str = "kind";
pub const OUTPUT_LEN: &str = "len";
pub const OUTPUT_PREVIEW: &str = "preview";
pub const OUTPUT_RETAINED_LEN: &str = "retained_len";
pub const OUTPUT_TRUNCATED: &str = "truncated";

pub fn bounded_bytes_output(bytes: &[u8]) -> Value {
    if bytes.len() <= MAX_RETAINED_OUTPUT_BYTES {
        return Value::from(bytes.to_vec());
    }

    let mut map = ValueMap::new();
    map.insert(OUTPUT_KIND.into(), Value::from(OUTPUT_BYTES_PREVIEW_KIND));
    map.insert(OUTPUT_LEN.into(), Value::from(bytes.len() as u64));
    map.insert(
        OUTPUT_RETAINED_LEN.into(),
        Value::from(MAX_RETAINED_OUTPUT_BYTES as u64),
    );
    map.insert(OUTPUT_TRUNCATED.into(), Value::from(true));
    map.insert(
        OUTPUT_PREVIEW.into(),
        Value::from(bytes[..MAX_RETAINED_OUTPUT_BYTES].to_vec()),
    );
    Value::from(map)
}
