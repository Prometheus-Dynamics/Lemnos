use crate::*;

#[test]
fn layouts_are_pinned() {
    // The C side declares these same sizes; a change here is a major version.
    assert_eq!(size_of::<Status>(), 4);
    assert_eq!(size_of::<Handle>(), 8);
    assert_eq!(size_of::<PlaneDesc>(), 32);
    assert_eq!(size_of::<BufferDesc>(), 32 + 32 * MAX_PLANES);
    assert_eq!(size_of::<FenceDesc>(), 16);
    assert_eq!(size_of::<AbiVersion>(), 16);
    assert_eq!(core::mem::offset_of!(BufferDesc, planes), 32);
}

#[test]
fn errnos_and_own_codes_do_not_overlap() {
    assert_eq!(Status::from_errno(22), Status::INVALID_ARGUMENT);
    assert_eq!(Status::INVALID_ARGUMENT.errno(), Some(22));
    assert_eq!(Status::from_errno(0), Status::IO);
    assert_eq!(Status::from_errno(4096), Status::IO);
    for own in [
        Status::STALE_HANDLE,
        Status::VERSION_MISMATCH,
        Status::BUFFER_TOO_SMALL,
        Status::PANIC,
    ] {
        assert_eq!(own.errno(), None, "{own:?}");
        assert!(!own.is_ok());
    }
    assert_eq!(Status(3).into_result(), Ok(3));
    assert_eq!(Status::BUSY.into_result(), Err(Status::BUSY));
}

#[cfg(feature = "std")]
#[test]
fn an_io_error_keeps_its_errno() {
    let e = std::io::Error::from_raw_os_error(16);
    assert_eq!(Status::from_io_error(&e), Status::BUSY);
    let other = std::io::Error::other("no errno");
    assert_eq!(Status::from_io_error(&other), Status::IO);
}

#[test]
fn a_handle_is_never_null_once_issued() {
    let h = Handle::new(7, 3);
    assert_eq!((h.index(), h.generation()), (7, 3));
    assert!(!h.is_null());
    assert!(Handle::NULL.is_null());
    assert!(Handle::new(5, 0).is_null());
}

#[cfg(feature = "alloc")]
#[test]
fn a_released_handle_is_stale_and_its_slot_is_reused() {
    let mut slab = Slab::new();
    let a = slab.insert("a").unwrap();
    let b = slab.insert("b").unwrap();
    assert_eq!(slab.remove(a), Ok("a"));
    assert_eq!(slab.get(a), Err(Status::STALE_HANDLE));
    assert_eq!(slab.remove(a), Err(Status::STALE_HANDLE));
    let c = slab.insert("c").unwrap();
    assert_eq!(c.index(), a.index(), "slot reused");
    assert_ne!(c, a, "with a new generation");
    assert_eq!(slab.get(a), Err(Status::STALE_HANDLE));
    assert_eq!(slab.get(c), Ok(&"c"));
    *slab.get_mut(b).unwrap() = "B";
    assert_eq!(slab.get(b), Ok(&"B"));
    assert_eq!(slab.get(Handle::NULL), Err(Status::STALE_HANDLE));
    assert_eq!(slab.get(Handle::new(99, 1)), Err(Status::STALE_HANDLE));
    assert_eq!(slab.len(), 2);
    let mut seen: alloc::vec::Vec<_> = slab.iter().map(|(_, v)| *v).collect();
    seen.sort_unstable();
    assert_eq!(seen, ["B", "c"]);
}

#[test]
fn buffers_are_validated_before_use() {
    let bytes = BufferDesc::bytes(5, 4096);
    assert_eq!(bytes.validate(), Status::OK);
    assert_eq!(bytes.planes().len(), 1);
    assert_eq!(
        BufferDesc::bytes(-1, 4096).validate(),
        Status::INVALID_ARGUMENT
    );

    const NV12: u32 = u32::from_le_bytes(*b"NV12");
    let plane = |offset, size| PlaneDesc {
        fd: 9,
        reserved: 0,
        offset,
        stride: 640,
        size,
    };
    let image = BufferDesc::image(640, 480, NV12, 0)
        .with_plane(plane(0, 640 * 480))
        .and_then(|d| d.with_plane(plane(640 * 480, 640 * 240)))
        .unwrap();
    assert_eq!(image.validate(), Status::OK);
    assert_eq!(
        BufferDesc::image(640, 480, NV12, 0).validate(),
        Status::INVALID_ARGUMENT
    );
    let mut no_stride = image;
    no_stride.planes[1].stride = 0;
    assert_eq!(no_stride.validate(), Status::INVALID_ARGUMENT);
    let mut overflow = image;
    overflow.planes[0].offset = u64::MAX;
    assert_eq!(overflow.validate(), Status::INVALID_ARGUMENT);
    let mut old = image;
    old.struct_size = 16;
    assert_eq!(old.validate(), Status::INVALID_ARGUMENT);
    let mut full = BufferDesc::image(1, 1, NV12, 0);
    for _ in 0..MAX_PLANES {
        full = full.with_plane(plane(0, 1)).unwrap();
    }
    assert_eq!(full.with_plane(plane(0, 1)), Err(Status::INVALID_ARGUMENT));
}

#[test]
fn fences_of_an_unknown_kind_are_refused() {
    assert_eq!(FenceDesc::none().validate(), Status::OK);
    assert_eq!(FenceDesc::sync_file(3).validate(), Status::OK);
    assert_eq!(FenceDesc::syncobj(3, 7).validate(), Status::OK);
    assert_eq!(
        FenceDesc::sync_file(-1).validate(),
        Status::INVALID_ARGUMENT
    );
    let unknown = FenceDesc {
        kind: 9,
        fd: 3,
        point: 0,
    };
    assert_eq!(unknown.validate(), Status::NOT_SUPPORTED);
}

#[test]
fn the_handshake_checks_major_minor_and_layout() {
    let surface = AbiVersion::new(2, 3, 0xabc);
    assert_eq!(surface.accepts(&AbiVersion::new(2, 3, 0xabc)), Status::OK);
    assert_eq!(surface.accepts(&AbiVersion::new(2, 1, 0xabc)), Status::OK);
    for caller in [
        AbiVersion::new(2, 4, 0xabc),
        AbiVersion::new(1, 3, 0xabc),
        AbiVersion::new(2, 3, 0xabd),
    ] {
        assert_eq!(
            surface.accepts(&caller),
            Status::VERSION_MISMATCH,
            "{caller:?}"
        );
    }
    let mut short = AbiVersion::new(2, 3, 0xabc);
    short.struct_size = 8;
    assert_eq!(surface.accepts(&short), Status::VERSION_MISMATCH);
    assert_eq!(VERSION.accepts(&VERSION), Status::OK);
    assert_ne!(layout_hash(&[1, 2]), layout_hash(&[2, 1]));
}

#[cfg(feature = "std")]
#[test]
fn a_panic_becomes_a_status() {
    let hook = std::panic::take_hook();
    std::panic::set_hook(std::boxed::Box::new(|_| {}));
    let status = guard(|| panic!("inside an entry point"));
    let handle = guard_or(Handle::NULL, || -> Handle { panic!("inside") });
    std::panic::set_hook(hook);
    assert_eq!(status, Status::PANIC);
    assert_eq!(handle, Handle::NULL);
    assert_eq!(guard(|| Status(4)), Status(4));
}

#[test]
fn a_sink_hands_the_message_to_its_callback() {
    use core::sync::atomic::{AtomicUsize, Ordering};
    static CALLS: AtomicUsize = AtomicUsize::new(0);
    static BYTES: AtomicUsize = AtomicUsize::new(0);
    extern "C" fn count(_: *mut core::ffi::c_void, _: *const u8, len: usize) {
        CALLS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(len, Ordering::Relaxed);
    }
    let sink = MessageSink {
        context: core::ptr::null_mut(),
        write: Some(count),
    };
    sink.write("plain");
    sink.write_fmt(format_args!("{}-{}", "formatted", 3));
    MessageSink::NONE.write("dropped");
    assert_eq!(
        BYTES.load(Ordering::Relaxed),
        "plain".len() + "formatted-3".len()
    );
    assert!(CALLS.load(Ordering::Relaxed) >= 2);
}

#[test]
fn features_are_bits_a_caller_requires() {
    let have = Features::NONE.with(0).with(5);
    assert!(have.contains(Features::NONE.with(5)));
    assert_eq!(have.require(Features::NONE.with(0)), Status::OK);
    assert_eq!(have.require(Features::NONE.with(1)), Status::NOT_SUPPORTED);
    assert_eq!(size_of::<Features>(), 8);
}

#[test]
fn out_buffers_take_all_or_nothing_and_report_the_length() {
    let mut buf = [0xaau8; 4];
    assert_eq!(
        write_out(b"abc", Some(&mut buf)),
        Written {
            status: Status::OK,
            needed: 3
        }
    );
    assert_eq!(&buf, b"abc\xaa");
    let mut small = [0xaau8; 2];
    assert_eq!(
        write_out(b"abc", Some(&mut small)),
        Written {
            status: Status::BUFFER_TOO_SMALL,
            needed: 3
        }
    );
    assert_eq!(small, [0xaa; 2]);
    assert_eq!(write_out(b"abc", None).needed, 3);
    // Text needs room for its NUL; the length does not count it.
    let mut exact = [0xaau8; 3];
    assert_eq!(
        write_text("abc", Some(&mut exact)).status,
        Status::BUFFER_TOO_SMALL
    );
    assert_eq!(exact, [0xaa; 3]);
    let mut room = [0xaau8; 4];
    assert_eq!(
        write_text("abc", Some(&mut room)),
        Written {
            status: Status::OK,
            needed: 3
        }
    );
    assert_eq!(&room, b"abc\0");
}

/// `include/lemnos/abi.h` declares this crate's values, and (where a C
/// compiler is installed) its sizes and offsets.
#[cfg(feature = "std")]
#[test]
fn the_header_matches_the_crate() {
    extern crate std;
    use core::mem::offset_of;
    use std::{format, fs, process::Command, string::String};

    let header = include_str!("../include/lemnos/abi.h");
    let define = |name: &str| {
        header
            .lines()
            .find_map(|l| {
                l.strip_prefix("#define ")?
                    .strip_prefix(name)?
                    .strip_prefix(' ')
            })
            .unwrap_or_else(|| panic!("{name} in abi.h"))
            .trim()
    };
    let hash = define("LEMNOS_ABI_TYPES_LAYOUT_HASH");
    let hash = u64::from_str_radix(hash.trim_start_matches("0x").trim_end_matches("ULL"), 16)
        .expect("hex hash");
    assert_eq!(
        hash, LAYOUT_HASH,
        "LEMNOS_ABI_TYPES_LAYOUT_HASH should be {LAYOUT_HASH:#018x}"
    );
    assert_eq!(
        define("LEMNOS_ABI_TYPES_MAJOR"),
        format!("{}", VERSION.major)
    );
    assert_eq!(
        define("LEMNOS_ABI_TYPES_MINOR"),
        format!("{}", VERSION.minor)
    );
    assert_eq!(define("LEMNOS_MAX_PLANES"), format!("{MAX_PLANES}"));

    if !Command::new("cc")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
    {
        return;
    }
    let checks: [(&str, i64); 26] = [
        ("LEMNOS_OK", Status::OK.0.into()),
        ("LEMNOS_E_BASE", Status::LEMNOS_BASE.into()),
        ("LEMNOS_E_STALE_HANDLE", Status::STALE_HANDLE.0.into()),
        (
            "LEMNOS_E_VERSION_MISMATCH",
            Status::VERSION_MISMATCH.0.into(),
        ),
        (
            "LEMNOS_E_BUFFER_TOO_SMALL",
            Status::BUFFER_TOO_SMALL.0.into(),
        ),
        ("LEMNOS_E_PANIC", Status::PANIC.0.into()),
        ("LEMNOS_BUFFER_OWNED", BufferDesc::OWNED.into()),
        ("LEMNOS_FENCE_NONE", FenceDesc::NONE.into()),
        ("LEMNOS_FENCE_SYNC_FILE", FenceDesc::SYNC_FILE.into()),
        ("LEMNOS_FENCE_SYNCOBJ", FenceDesc::SYNCOBJ.into()),
        ("sizeof(LemnosStatus)", size_of::<Status>() as i64),
        ("sizeof(LemnosHandle)", size_of::<Handle>() as i64),
        ("sizeof(LemnosFeatures)", size_of::<Features>() as i64),
        ("sizeof(LemnosAbiVersion)", size_of::<AbiVersion>() as i64),
        (
            "offsetof(LemnosAbiVersion, layout)",
            offset_of!(AbiVersion, layout) as i64,
        ),
        ("sizeof(LemnosMessageSink)", size_of::<MessageSink>() as i64),
        ("sizeof(LemnosPlaneDesc)", size_of::<PlaneDesc>() as i64),
        (
            "offsetof(LemnosPlaneDesc, offset)",
            offset_of!(PlaneDesc, offset) as i64,
        ),
        (
            "offsetof(LemnosPlaneDesc, size)",
            offset_of!(PlaneDesc, size) as i64,
        ),
        ("sizeof(LemnosBufferDesc)", size_of::<BufferDesc>() as i64),
        (
            "offsetof(LemnosBufferDesc, plane_count)",
            offset_of!(BufferDesc, plane_count) as i64,
        ),
        (
            "offsetof(LemnosBufferDesc, modifier)",
            offset_of!(BufferDesc, modifier) as i64,
        ),
        (
            "offsetof(LemnosBufferDesc, planes)",
            offset_of!(BufferDesc, planes) as i64,
        ),
        ("sizeof(LemnosFenceDesc)", size_of::<FenceDesc>() as i64),
        (
            "offsetof(LemnosFenceDesc, fd)",
            offset_of!(FenceDesc, fd) as i64,
        ),
        (
            "offsetof(LemnosFenceDesc, point)",
            offset_of!(FenceDesc, point) as i64,
        ),
    ];
    let mut source = String::from("#include \"lemnos/abi.h\"\n");
    for (expr, value) in checks {
        source.push_str(&format!(
            "_Static_assert(({expr}) == ({value}), \"{expr}\");\n"
        ));
    }
    let dir = std::env::temp_dir().join(format!("lemnos-abi-header-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let file = dir.join("check.c");
    fs::write(&file, source).unwrap();
    let out = Command::new("cc")
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
