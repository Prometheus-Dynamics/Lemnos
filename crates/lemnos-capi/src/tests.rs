use std::fs;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use lemnos_abi::{AbiVersion, Handle, MessageSink, Status};
use lemnos_hal::register::AddressWidth;
use lemnosd::mock::{MockHardware, MockLemnosd};

use crate::ffi;
use crate::surface::{self, DeviceInfo, RawReading};
use crate::{Client, MAX_CHANNELS, VERSION};

const BOARD: &str = r#"
format = "lemnos.board"
schema_version = 1

[board]
id = "capi"

[[devices]]
id = "imu"
driver = "bmi088"
bus = "i2c-1"
address = 0x18
backend = "userspace"
poll_ms = 10
"#;

fn service() -> MockLemnosd {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let root = std::env::temp_dir().join(format!(
        "lemnos-capi-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("sys")).unwrap();
    let hardware = MockHardware::new();
    hardware
        .i2c(1)
        .with_target(0x18, AddressWidth::Bits8)
        .with_target(0x68, AddressWidth::Bits8)
        .with_registers(0x18, 0x00, &[0x1e])
        .with_registers(0x68, 0x00, &[0x0f])
        .with_sampler(0x18, 0x12, Duration::from_millis(10));
    MockLemnosd::start_in(root, BOARD, hardware).unwrap()
}

fn socket(service: &MockLemnosd) -> String {
    service.socket().to_str().unwrap().to_string()
}

#[test]
fn the_handshake_accepts_this_build_and_refuses_another() {
    assert_eq!(ffi::lemnos_version(), VERSION);
    let check = |version: *const AbiVersion| {
        // SAFETY: each caller below passes a pointer to a live version, or
        // null, which the surface refuses.
        unsafe { ffi::lemnos_check_version(version) }
    };
    assert_eq!(check(&VERSION), Status::OK);
    let newer = AbiVersion::new(VERSION.major, VERSION.minor + 1, VERSION.layout);
    assert_eq!(check(&newer), Status::VERSION_MISMATCH);
    let other = AbiVersion::new(VERSION.major, VERSION.minor, VERSION.layout ^ 1);
    assert_eq!(check(&other), Status::VERSION_MISMATCH);
    assert_eq!(check(core::ptr::null()), Status::INVALID_ARGUMENT);
}

#[test]
fn a_missing_socket_is_an_errno_with_a_message() {
    let err = Client::open("/nonexistent/lemnosd.sock", "capi", 200).unwrap_err();
    assert!(err.status.errno().is_some(), "{err:?}");
    assert!(!err.message.is_empty(), "{err:?}");
}

#[test]
fn lists_reads_and_streams_through_the_surface() {
    let service = service();
    let client = Client::open(&socket(&service), "capi", 0).unwrap();
    let devices = client.devices().unwrap();
    let imu = devices.iter().find(|d| d.id == "imu").expect("imu listed");
    assert_eq!(imu.info.class, 0, "LEMNOS_CLASS_IMU");
    assert!(imu.channels.iter().any(|c| c.name == "acceleration.x"));
    assert_eq!(imu.channels.len(), imu.info.channel_count as usize);

    let reading = client.read(imu.index).unwrap();
    assert_eq!(reading.raw.status, 0, "available");
    assert!(reading.raw.channel_count as usize <= MAX_CHANNELS);
    assert!(imu.value(&reading, "acceleration.x").is_some());
    assert!(reading.raw.timestamp_us > 0);

    let granted = client.subscribe(imu.index, 10).unwrap();
    assert!(granted >= 10, "{granted}");
    let mut got = Vec::new();
    for _ in 0..50 {
        if let Some(r) = client.next_reading(200).unwrap() {
            got.push(r);
            if got.len() == 5 {
                break;
            }
        }
    }
    assert_eq!(got.len(), 5, "five streamed readings");
    assert!(got.iter().all(|r| r.device == imu.index));
    assert!(
        got.windows(2)
            .all(|w| w[1].raw.timestamp_us > w[0].raw.timestamp_us)
    );
    service.stop();
}

#[test]
fn a_closed_client_is_stale_and_unknown_devices_are_not_found() {
    let service = service();
    let client = Client::open(&socket(&service), "capi", 0).unwrap();
    let handle = client.handle();
    assert!(ffi::lemnos_device_count(handle).0 >= 1);
    let err = client.read(9_999).unwrap_err();
    assert_eq!(err.status, Status::NOT_FOUND);
    drop(client);
    assert_eq!(ffi::lemnos_device_count(handle), Status::STALE_HANDLE);
    assert_eq!(ffi::lemnos_client_close(handle), Status::STALE_HANDLE);
    assert_eq!(ffi::lemnos_device_count(Handle::NULL), Status::STALE_HANDLE);
    service.stop();
}

#[test]
fn short_structs_null_pointers_and_small_buffers_are_refused() {
    let service = service();
    let client = Client::open(&socket(&service), "capi", 0).unwrap();
    let h = client.handle();

    let mut short = RawReading {
        struct_size: 8,
        ..RawReading::default()
    };
    // SAFETY: a live reading whose struct_size says it is too small.
    let status = unsafe { ffi::lemnos_read(h, 0, &mut short, MessageSink::NONE) };
    assert_eq!(status, Status::INVALID_ARGUMENT);
    // SAFETY: null is allowed and refused.
    let status = unsafe { ffi::lemnos_read(h, 0, core::ptr::null_mut(), MessageSink::NONE) };
    assert_eq!(status, Status::INVALID_ARGUMENT);

    let mut info = DeviceInfo {
        struct_size: size_of::<DeviceInfo>() as u32,
        ..DeviceInfo::default()
    };
    let mut buf = [0u8; 2];
    let mut len = 0usize;
    // SAFETY: live info, a 2-byte buffer and a length.
    let status = unsafe { ffi::lemnos_device_info(h, 0, &mut info, buf.as_mut_ptr(), 2, &mut len) };
    assert_eq!(status, Status::BUFFER_TOO_SMALL);
    assert_eq!(len, "imu".len(), "the length is reported anyway");
    assert_eq!(info.class, 0, "the info is written anyway");
    let mut buf = [0xffu8; 4];
    // SAFETY: as above, with room for "imu" and its NUL.
    let status = unsafe { ffi::lemnos_device_info(h, 0, &mut info, buf.as_mut_ptr(), 4, &mut len) };
    assert_eq!(status, Status::OK);
    assert_eq!(&buf, b"imu\0");

    let mut out = Handle::NULL;
    // SAFETY: null strings are refused before any use.
    let status = unsafe {
        ffi::lemnos_client_open(
            core::ptr::null(),
            core::ptr::null(),
            0,
            &mut out,
            MessageSink::NONE,
        )
    };
    assert_eq!(status, Status::INVALID_ARGUMENT);
    service.stop();
}

/// The header declares what the surface implements: its layout hash, and
/// (where a C compiler is installed) the sizes and offsets of its structs.
#[test]
fn the_header_matches_the_surface() {
    let header = include_str!("../include/lemnos.h");
    let hash = header
        .lines()
        .find_map(|l| l.strip_prefix("#define LEMNOS_LAYOUT_HASH "))
        .expect("LEMNOS_LAYOUT_HASH");
    let hash = u64::from_str_radix(
        hash.trim().trim_start_matches("0x").trim_end_matches("ULL"),
        16,
    )
    .expect("hex hash");
    assert_eq!(
        hash, VERSION.layout,
        "LEMNOS_LAYOUT_HASH should be {:#018x}",
        VERSION.layout
    );
    for (name, value) in [
        ("LEMNOS_ABI_MAJOR", i64::from(VERSION.major)),
        ("LEMNOS_ABI_MINOR", i64::from(VERSION.minor)),
        ("LEMNOS_MAX_CHANNELS", MAX_CHANNELS as i64),
    ] {
        let line = format!("#define {name} {value}");
        assert!(header.lines().any(|l| l == line), "{line}");
    }

    let Ok(cc) = std::process::Command::new("cc").arg("--version").output() else {
        return;
    };
    if !cc.status.success() {
        return;
    }
    use core::mem::offset_of;
    let checks = [
        ("sizeof(LemnosAbiVersion)", size_of::<AbiVersion>()),
        ("sizeof(LemnosMessageSink)", size_of::<MessageSink>()),
        ("sizeof(LemnosDeviceInfo)", size_of::<DeviceInfo>()),
        (
            "offsetof(LemnosDeviceInfo, pixels)",
            offset_of!(DeviceInfo, pixels),
        ),
        (
            "sizeof(LemnosChannelInfo)",
            size_of::<surface::ChannelInfo>(),
        ),
        ("sizeof(LemnosReading)", size_of::<RawReading>()),
        (
            "offsetof(LemnosReading, timestamp_us)",
            offset_of!(RawReading, timestamp_us),
        ),
        (
            "offsetof(LemnosReading, values)",
            offset_of!(RawReading, values),
        ),
        ("sizeof(LemnosHandle)", size_of::<Handle>()),
    ];
    let mut source = String::from("#include \"lemnos.h\"\n");
    for (expr, size) in checks {
        source.push_str(&format!("_Static_assert({expr} == {size}, \"{expr}\");\n"));
    }
    let dir = std::env::temp_dir().join(format!("lemnos-capi-header-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let file = dir.join("check.c");
    fs::write(&file, source).unwrap();
    let out = std::process::Command::new("cc")
        .args(["-std=c11", "-Wall", "-Werror", "-fsyntax-only", "-I"])
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/include"))
        .arg(&file)
        .output()
        .unwrap();
    let _ = fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `lemnos.exports` lists exactly the functions the header declares and the
/// crate exports, in the header's order.
#[test]
fn the_export_list_matches_the_header_and_the_exports() {
    let listed: Vec<&str> = include_str!("../lemnos.exports")
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();
    let name_before_paren = |line: &str| -> Option<String> {
        let at = line.find("lemnos_")?;
        let rest = &line[at..];
        let end = rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))?;
        rest[end..]
            .starts_with('(')
            .then(|| rest[..end].to_string())
    };
    let declared: Vec<String> = include_str!("../include/lemnos.h")
        .lines()
        .filter(|l| !l.trim_start().starts_with(['#', '/', '*']))
        .filter_map(name_before_paren)
        .collect();
    assert_eq!(listed, declared, "lemnos.exports against include/lemnos.h");
    let source = include_str!("surface.rs");
    let mut exported: Vec<String> = Vec::new();
    let mut lines = source.lines();
    while let Some(line) = lines.next() {
        if line.trim() == r#"#[cfg_attr(not(feature = "shared"), unsafe(no_mangle))]"# {
            let decl = lines.next().unwrap_or_default();
            exported.extend(name_before_paren(decl));
        }
    }
    assert_eq!(listed, exported, "lemnos.exports against src/surface.rs");
}

/// With `shared`, each declaration of the library is the type of the function
/// in `surface` it stands for: an array of the two only coerces when the two
/// signatures are the same.
#[cfg(feature = "shared")]
#[test]
fn ffi_matches_the_surface() {
    let _ = [surface::lemnos_version, ffi::lemnos_version];
    let _ = [surface::lemnos_check_version, ffi::lemnos_check_version];
    let _ = [surface::lemnos_client_open, ffi::lemnos_client_open];
    let _ = [surface::lemnos_client_close, ffi::lemnos_client_close];
    let _ = [surface::lemnos_device_count, ffi::lemnos_device_count];
    let _ = [surface::lemnos_device_info, ffi::lemnos_device_info];
    let _ = [surface::lemnos_channel_info, ffi::lemnos_channel_info];
    let _ = [surface::lemnos_read, ffi::lemnos_read];
    let _ = [surface::lemnos_subscribe, ffi::lemnos_subscribe];
    let _ = [surface::lemnos_next_reading, ffi::lemnos_next_reading];
}

/// With `shared`, the wrapper's process runs on the library: this crate
/// exports none of the surface itself.
#[cfg(feature = "shared")]
#[test]
fn the_shared_wrapper_exports_nothing_of_its_own() {
    assert!(!include_str!("surface.rs").contains("\n#[unsafe(no_mangle)]"));
    assert!(
        std::path::Path::new(env!("LEMNOS_CAPI_SHARED_DIR"))
            .join("liblemnos_capi.so.1")
            .exists()
    );
}

/// The shared types' header ships here as a copy of `lemnos-abi`'s, byte for
/// byte, so this crate's `include/` stands alone.
#[test]
fn the_abi_header_is_lemnos_abis() {
    let ours = include_str!("../include/lemnos/abi.h");
    let theirs = include_str!("../../lemnos-abi/include/lemnos/abi.h");
    assert!(
        ours == theirs,
        "copy crates/lemnos-abi/include/lemnos/abi.h to include/lemnos/"
    );
}

/// With `shared`, a test process holds two copies of the surface: the library
/// (through `Client`) and this crate's own `surface`, called directly. The
/// library opens first and claims the process, so the second copy's open is
/// refused with `-EBUSY` and a message, before it connects anywhere.
#[cfg(feature = "shared")]
#[test]
fn a_second_copy_of_the_surface_is_refused() {
    let service = service();
    let _first = Client::open(&socket(&service), "capi", 0).unwrap();
    let socket = std::ffi::CString::new(socket(&service)).unwrap();
    let mut text = String::new();
    extern "C" fn collect(context: *mut core::ffi::c_void, text: *const u8, len: usize) {
        // SAFETY: `text` holds `len` bytes during the callback.
        let bytes = unsafe { core::slice::from_raw_parts(text, len) };
        // SAFETY: `context` is the `String` below, alive for the call.
        let out = unsafe { &mut *context.cast::<String>() };
        out.push_str(&String::from_utf8_lossy(bytes));
    }
    let sink = MessageSink {
        context: (&raw mut text).cast(),
        write: Some(collect),
    };
    let mut handle = Handle::NULL;
    // SAFETY: NUL-terminated strings and a live handle, valid for the call.
    let status = unsafe {
        surface::lemnos_client_open(socket.as_ptr(), c"second".as_ptr(), 0, &mut handle, sink)
    };
    assert_eq!(status, Status::BUSY);
    assert!(handle.is_null());
    assert!(text.contains("another copy"), "{text}");
    service.stop();
}
