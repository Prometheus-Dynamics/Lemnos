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
