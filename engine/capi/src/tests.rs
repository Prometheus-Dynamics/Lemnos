use core::mem::{offset_of, size_of};
use std::process::Command;

use crate::surface::{
    DeviceInfo, Frame, ItemInfo, LAYOUT, PackageInfo, STREAM_BUFFER_HANDLES, STREAM_OPEN,
    STREAM_WRITABLE, StreamInfo, Subscription, VERSION, Value,
};

const HEADER: &str = include_str!("../../../include/lemnos.h");
const EXPORTS: &str = include_str!("../lemnos.exports");
/// The surface's functions, in the order they are declared.
const SURFACE: [&str; 2] = [
    include_str!("surface.rs"),
    include_str!("surface/streams.rs"),
];

fn define(name: &str) -> &'static str {
    HEADER
        .lines()
        .find_map(|l| l.strip_prefix(&format!("#define {name} ")))
        .unwrap_or_else(|| panic!("{name} in lemnos.h"))
        .trim()
}

#[test]
fn the_flag_defines_match() {
    assert_eq!(define("LEMNOS_STREAM_WRITABLE"), "(1u << 0)");
    assert_eq!(STREAM_WRITABLE, 1 << 0);
    assert_eq!(define("LEMNOS_STREAM_BUFFER_HANDLES"), "(1u << 1)");
    assert_eq!(STREAM_BUFFER_HANDLES, 1 << 1);
    assert_eq!(define("LEMNOS_STREAM_OPEN"), "(1u << 2)");
    assert_eq!(STREAM_OPEN, 1 << 2);
}

#[test]
fn the_version_defines_match() {
    assert_eq!(define("LEMNOS_ABI_MAJOR"), VERSION.major.to_string());
    assert_eq!(define("LEMNOS_ABI_MINOR"), VERSION.minor.to_string());
    assert_eq!(define("LEMNOS_LAYOUT_HASH"), format!("{LAYOUT:#x}ULL"));
}

fn listed() -> Vec<&'static str> {
    EXPORTS
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
        .collect()
}

#[test]
fn the_export_list_matches_the_header() {
    let mut code = String::new();
    let mut rest = HEADER;
    while let Some((before, after)) = rest.split_once("/*") {
        code.push_str(before);
        rest = after.split_once("*/").unwrap().1;
    }
    code.push_str(rest);
    let declared: Vec<&str> = code
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|w| w.starts_with("lemnos_"))
        .fold(Vec::new(), |mut names, word| {
            if !names.contains(&word) {
                names.push(word);
            }
            names
        });
    assert_eq!(listed(), declared);
}

#[test]
fn the_export_list_matches_the_functions() {
    let exported: Vec<&str> = SURFACE
        .iter()
        .flat_map(|file| file.lines())
        .filter_map(|l| l.split("extern \"C\" fn ").nth(1))
        .filter_map(|l| l.split('(').next())
        .collect();
    assert_eq!(listed(), exported);
}

#[test]
fn a_c_compiler_agrees_on_every_layout() {
    if Command::new("cc").arg("--version").output().is_err() {
        return;
    }
    macro_rules! check {
        ($c:literal, $rust:ident, $($field:ident),*) => {{
            let mut text = format!(
                "_Static_assert(sizeof({}) == {}, \"size\");\n",
                $c,
                size_of::<$rust>()
            );
            $(text += &format!(
                "_Static_assert(offsetof({}, {}) == {}, \"offset\");\n",
                $c,
                stringify!($field),
                offset_of!($rust, $field)
            );)*
            text
        }};
    }
    let mut source = String::from("#include <stddef.h>\n#include \"lemnos.h\"\n");
    source += &check!("LemnosDeviceInfo", DeviceInfo, state, health, reserved);
    source += &check!("LemnosSubscription", Subscription, reserved, id, period_us);
    source += &check!("LemnosValue", Value, len, subscription, stamp_ns);
    source += &check!(
        "LemnosItemInfo",
        ItemInfo,
        kind,
        contract,
        method,
        exponent,
        has_range,
        min,
        max
    );
    source += &check!("LemnosPackageInfo", PackageInfo, loaded);
    source += &check!(
        "LemnosStreamInfo",
        StreamInfo,
        flags,
        buffers,
        buffer_bytes,
        frame_slots,
        job_slots,
        job_bytes,
        planes
    );
    source += &check!(
        "LemnosFrame",
        Frame,
        buffer,
        len,
        reserved,
        sequence,
        identity,
        timestamp_ns
    );
    let dir = std::env::temp_dir().join(format!("lemnos-capi-layout-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("check.c");
    std::fs::write(&file, source).unwrap();
    let output = Command::new("cc")
        .args(["-std=c11", "-fsyntax-only", "-I"])
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/../../include"))
        .arg(&file)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
