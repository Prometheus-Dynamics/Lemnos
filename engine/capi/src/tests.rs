use core::mem::{offset_of, size_of};
use std::process::Command;

use crate::surface::{
    DeviceInfo, Frame, ItemInfo, JOB_DONE, JOB_PROGRESS, JobEvent, LAYOUT, PackageInfo,
    STREAM_BUFFER_HANDLES, STREAM_OPEN, STREAM_WRITABLE, StreamInfo, Subscription, VERSION, Value,
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
    source += &check!(
        "LemnosJobEvent",
        JobEvent,
        kind,
        job,
        done,
        total,
        status,
        len
    );
    let dir = std::env::temp_dir().join(format!("lemnos-capi-layout-{}-a", std::process::id()));
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

#[test]
fn the_job_event_kinds_match() {
    assert_eq!(define("LEMNOS_JOB_PROGRESS"), JOB_PROGRESS.to_string());
    assert_eq!(define("LEMNOS_JOB_DONE"), JOB_DONE.to_string());
}

/// A service from another Lemnos revision: the C surface refuses at connect with
/// VERSION_MISMATCH, and its message names both layouts.
#[test]
fn a_service_from_another_lemnos_revision_is_refused_through_the_c_surface() {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;
    use std::sync::Mutex;

    use lemnos_wire::{Frame, LemnosName, VERSION, append, decode};

    static TEXT: Mutex<String> = Mutex::new(String::new());
    extern "C" fn collect(_: *mut core::ffi::c_void, text: *const u8, len: usize) {
        // SAFETY: the surface passes `len` bytes of UTF-8, valid for this call.
        let bytes = unsafe { core::slice::from_raw_parts(text, len) };
        TEXT.lock()
            .unwrap()
            .push_str(core::str::from_utf8(bytes).unwrap());
    }

    let dir = std::env::temp_dir().join(format!("lemnos-capi-layout-{}-b", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("svc.sock");
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).unwrap();
    let served = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = vec![0u8; 4096];
        let mut got = 0;
        loop {
            let n = stream.read(&mut buf[got..]).unwrap();
            got += n;
            if matches!(decode(&buf[..got]), Ok(Some(_))) || n == 0 {
                break;
            }
        }
        let mut out = Vec::new();
        append(
            &Frame::Welcome {
                core: 0xdead_beef_0bad_f00d,
                parts: lemnos_wire::Tail(&lemnos_wire::PART_BYTES[..]),
                lemnos: LemnosName("9.9.9"),
                version: VERSION,
                features: 0,
                host: "other",
            },
            &mut out,
        )
        .unwrap();
        stream.write_all(&out).unwrap();
    });

    let socket = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
    let name = std::ffi::CString::new("c").unwrap();
    let mut handle = lemnos_abi::Handle::NULL;
    let sink = lemnos_abi::MessageSink {
        context: core::ptr::null_mut(),
        write: Some(collect),
    };
    // SAFETY: both strings are NUL-terminated and live through the call; `handle` is valid for a
    // write.
    let status = unsafe {
        crate::ffi::lemnos_client_open(socket.as_ptr(), name.as_ptr(), 2_000, &mut handle, sink)
    };
    served.join().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(status, lemnos_abi::Status::VERSION_MISMATCH);
    let text = TEXT.lock().unwrap().clone();
    assert!(text.contains("0xdeadbeef0badf00d"), "{text}");
    assert!(
        text.contains(&format!("{:#018x}", lemnos_wire::CORE_HASH)),
        "{text}"
    );
    assert!(text.contains("9.9.9"), "{text}");
    assert!(text.contains("core protocol differs"), "{text}");
}

/// The named fields of `struct name` in `source`, in order: the ones its declaration gives in
/// named form. `None` when it has no such declaration.
fn named_fields(source: &str, name: &str) -> Option<Vec<String>> {
    let start = source.find(&format!("pub struct {name} {{"))?;
    let body = &source[start..];
    let end = body.find("\n}")?;
    Some(
        body[..end]
            .lines()
            .skip(1)
            .filter_map(|line| {
                let rest = line.trim().strip_prefix("pub ")?;
                let (field, _) = rest.split_once(':')?;
                Some(field.trim().to_string())
            })
            .collect(),
    )
}

/// Every `shape!(Type, field, ...)` in `layout`, as its type and its fields.
fn listed_shapes(layout: &str) -> Vec<(String, Vec<String>)> {
    layout
        .split("shape!(")
        .skip(1)
        .map(|call| {
            let inside = &call[..call.find(')').unwrap()];
            let mut parts = inside.split(',').map(str::trim).filter(|p| !p.is_empty());
            let name = parts.next().unwrap().to_string();
            (name, parts.map(str::to_string).collect())
        })
        .collect()
}

#[test]
fn every_field_of_a_hashed_type_is_in_the_layout() {
    let surface = include_str!("surface.rs");
    let streams = include_str!("surface/streams.rs");
    let sources = format!("{surface}\n{streams}");
    let shapes = listed_shapes(surface);
    assert!(shapes.len() >= 8, "the layout lists its types: {shapes:?}");
    for (name, listed) in shapes {
        let declared = named_fields(&sources, &name)
            .unwrap_or_else(|| panic!("{name} is hashed but not declared here"));
        assert_eq!(
            listed, declared,
            "{name}: the layout hash must list every field, in order"
        );
    }
}
