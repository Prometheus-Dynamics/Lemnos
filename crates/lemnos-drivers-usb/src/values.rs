pub(crate) fn ports_name(ports: &[u8]) -> String {
    ports
        .iter()
        .map(u8::to_string)
        .collect::<Vec<_>>()
        .join(".")
}

pub(crate) fn hex_u16(value: u16) -> String {
    format!("{value:04x}")
}
