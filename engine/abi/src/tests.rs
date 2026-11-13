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

    let header = include_str!("../../../include/lemnos/abi.h");
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
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/../../include"))
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

mod package {
    use crate::package::*;
    use crate::{AbiVersion, Status};

    #[test]
    fn the_descriptor_starts_with_what_says_it_is_one() {
        assert_eq!(PACKAGE_MAGIC.to_le_bytes(), *b"LEMNOSPK");
        assert_eq!(PACKAGE_ENTRY.to_bytes(), b"lemnos_package");
        assert_eq!(core::mem::offset_of!(PackageDescriptor, magic), 0);
        assert_eq!(core::mem::offset_of!(PackageDescriptor, version), 8);
        assert_eq!(DESCRIPTOR_HEAD, 24);
        assert_eq!(
            core::mem::offset_of!(PackageDescriptor, register),
            DESCRIPTOR_HEAD
        );
        assert_eq!((PACKAGE_VERSION.major, PACKAGE_VERSION.minor), (2, 0));
        assert_eq!(PACKAGE_VERSION.layout, PACKAGE_LAYOUT_HASH);
        assert_eq!(
            PACKAGE_VERSION.struct_size as usize,
            size_of::<AbiVersion>()
        );
        // The boundary's hash is not this crate's frozen one: it covers more.
        assert_ne!(PACKAGE_LAYOUT_HASH, crate::LAYOUT_HASH);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn layouts_are_what_the_documents_say() {
        assert_eq!(size_of::<Str<'static>>(), 16);
        assert_eq!(size_of::<Slice<'static, u8>>(), 16);
        assert_eq!(size_of::<SliceMut<'static, u8>>(), 16);
        assert_eq!(size_of::<Out<'static, u64>>(), 8);
        assert_eq!(size_of::<PackageNeed>(), 40);
        assert_eq!(size_of::<PackageShape>(), 40);
        assert_eq!(size_of::<PackageField>(), 56);
        assert_eq!(size_of::<PackageMethod>(), 80);
        assert_eq!(size_of::<PackageContract>(), 32);
        assert_eq!(size_of::<PackageManifest>(), 48);
        assert_eq!(size_of::<PackageHealth>(), 24);
        assert_eq!(size_of::<PackageItem>(), 56);
        assert_eq!(size_of::<PackageSetting<'static>>(), 56);
        assert_eq!(size_of::<DriverTable>(), 72);
        assert_eq!(size_of::<HostTable>(), 568);
        assert_eq!(core::mem::offset_of!(HostTable, device), 32);
        assert_eq!(core::mem::offset_of!(HostTable, usb_function), 496);
        // What the tables carry.
        assert_eq!(size_of::<Fd>(), 4);
        assert_eq!(size_of::<PackageTransfer<'static>>(), 40);
        assert_eq!(size_of::<PackageSpiSegment<'static>>(), 48);
        assert_eq!(size_of::<PackageLineConfig>(), 12);
        assert_eq!(size_of::<PackageEdge>(), 24);
        assert_eq!(size_of::<PackagePwmConfig>(), 24);
        assert_eq!(size_of::<PackageInterruptEvent>(), 16);
        assert_eq!(size_of::<PackageDmaBuffer>(), 24);
        assert_eq!(size_of::<PackageFrame>(), 32);
        assert_eq!(size_of::<PackageControlSetup>(), 8);
        assert_eq!(size_of::<PackageIsoPacket>(), 12);
        assert_eq!(size_of::<PackageUsbEvent>(), 16);
        // Each table is its functions and nothing else.
        let pointer = size_of::<usize>();
        for (table, functions) in [
            (size_of::<DeviceTable>(), 2),
            (size_of::<I2cTable>(), 1),
            (size_of::<SpiTable>(), 2),
            (size_of::<LineTable>(), 4),
            (size_of::<PwmTable>(), 2),
            (size_of::<UartTable>(), 3),
            (size_of::<RegistersTable>(), 5),
            (size_of::<InterruptTable>(), 3),
            (size_of::<DmaTable>(), 6),
            (size_of::<ClocksTable>(), 4),
            (size_of::<ResetTable>(), 2),
            (size_of::<StreamTable>(), 10),
            (size_of::<UsbDeviceTable>(), 14),
            (size_of::<UsbFunctionTable>(), 9),
        ] {
            assert_eq!(table, functions * pointer);
        }
        assert_eq!(size_of::<PackageDriver>(), 120);
        assert_eq!(size_of::<PackageDescriptor>(), 32);
    }

    #[test]
    fn references_cross_and_come_back_as_they_were() {
        assert_eq!(Str::new("näme").as_str(), "näme");
        assert_eq!(Str::EMPTY.as_str(), "");
        assert_eq!(Slice::new(&[1u16, 2, 3]).as_slice(), [1, 2, 3]);
        assert!(Slice::<u64>::EMPTY.as_slice().is_empty());

        let mut room = [0u8; 4];
        let mut slice = SliceMut::new(&mut room);
        assert_eq!(slice.len(), 4);
        slice.as_mut_slice()[0] = 7;
        slice.into_slice()[3] = 9;
        assert_eq!(room, [7, 0, 0, 9]);
        assert!(SliceMut::<u8>::empty().is_empty());

        let mut place = 1u32;
        Out::new(&mut place).set(5);
        assert_eq!(place, 5);
    }

    #[cfg(feature = "std")]
    mod tables {
        use core::ffi::c_void;
        use std::sync::atomic::{AtomicU32, Ordering};

        use super::*;
        use crate::Handle;

        static DROPS: AtomicU32 = AtomicU32::new(0);

        /// Adds its arguments' first bytes; panics on method 9.
        struct Adder {
            base: u8,
            last_caller: u64,
        }

        impl Drop for Adder {
            fn drop(&mut self) {
                DROPS.fetch_add(1, Ordering::Relaxed);
            }
        }

        impl PackageInstance for Adder {
            fn bind(
                host: &HostTable,
                binding: *mut c_void,
                settings: &[PackageSetting<'_>],
                link: Option<&str>,
            ) -> Result<Self, Status> {
                if (host.provided)(binding, Str::new("refuse")) != 0 {
                    return Err(Status::NO_DEVICE);
                }
                let mut handle = Handle::NULL;
                let taken = (host.take)(
                    binding,
                    7,
                    Str::new("bus"),
                    Str::EMPTY,
                    Out::new(&mut handle),
                );
                assert_eq!((taken, handle), (Status::OK, Handle::new(3, 1)));
                assert_eq!(link, Some("other"));
                Ok(Self {
                    base: settings[0].int as u8,
                    last_caller: 0,
                })
            }

            fn describe(&self, items: &mut [PackageItem]) -> u32 {
                items[0].method = 4;
                3
            }

            fn health(&self) -> PackageHealth {
                PackageHealth::new(HEALTH_DEGRADED, "warm")
            }

            fn dispatch(
                &mut self,
                _contract: u8,
                method: u16,
                args: &[u8],
                reply: &mut [u8],
            ) -> (Status, usize) {
                assert_ne!(method, 9, "deliberate panic");
                reply[0] = self.base + args[0];
                (Status::OK, 1)
            }

            fn tick(&mut self, now_ns: u64) -> Option<u64> {
                (now_ns < 100).then_some(now_ns + 1)
            }

            fn caller(&mut self, caller: u64) {
                self.last_caller = caller;
            }

            fn caller_gone(&mut self, caller: u64) {
                assert_eq!(caller, self.last_caller);
            }
        }

        struct Board {
            refuse: bool,
        }

        impl PackageBinding for Board {
            fn provided(&mut self, need: &str) -> bool {
                need == "refuse" && self.refuse
            }

            fn take(&mut self, kind: u32, need: &str, _tag: &str) -> Result<Handle, Status> {
                assert_eq!((kind, need), (7, "bus"));
                Ok(Handle::new(3, 1))
            }
        }

        struct Nothing;

        impl PackageHost for Nothing {
            fn release(_resource: Handle) -> Status {
                panic!("deliberate panic in the host");
            }
        }

        // This host has I2C buses and lines, and nothing else: every other kind is left as it
        // is.
        impl I2cHost for Nothing {
            fn transaction(
                _bus: Handle,
                _address: u16,
                mut steps: SliceMut<'_, PackageTransfer<'_>>,
            ) -> Status {
                let steps = steps.as_mut_slice();
                steps[1].read.as_mut_slice()[0] = steps[0].write.as_slice()[0] + 1;
                Status::OK
            }
        }

        impl LineHost for Nothing {
            fn get(_line: Handle, value: Out<'_, u32>) -> Status {
                value.set(1);
                Status::OK
            }

            fn set(_line: Handle, _value: u32) -> Status {
                panic!("deliberate panic in a table");
            }
        }

        impl DeviceHost for Nothing {}
        impl SpiHost for Nothing {}
        impl PwmHost for Nothing {}
        impl UartHost for Nothing {}
        impl RegistersHost for Nothing {}
        impl InterruptHost for Nothing {}
        impl DmaHost for Nothing {}
        impl ClocksHost for Nothing {}
        impl ResetHost for Nothing {}
        impl StreamHost for Nothing {}
        impl UsbDeviceHost for Nothing {}
        impl UsbFunctionHost for Nothing {}

        static DRIVER: DriverTable = driver_table::<Adder>(DRIVER_TICKS);
        static HOST: HostTable = host_table::<Board, Nothing>();

        fn setting(int: i64) -> PackageSetting<'static> {
            PackageSetting {
                key: Str::new("base"),
                kind: SETTING_INT,
                reserved: 0,
                int,
                float: 0.0,
                text: Str::EMPTY,
            }
        }

        #[test]
        fn a_driver_is_served_through_its_table_and_a_panic_is_a_status() {
            assert_eq!(DRIVER.flags, DRIVER_TICKS);
            assert_eq!(DRIVER.struct_size as usize, size_of::<DriverTable>());
            let mut board = Board { refuse: true };
            let refused = Loaded::bind(&DRIVER, &HOST, &mut board, &[setting(1)], None);
            assert_eq!(refused.err(), Some(Status::NO_DEVICE));

            let mut board = Board { refuse: false };
            let mut driver =
                Loaded::bind(&DRIVER, &HOST, &mut board, &[setting(40)], Some("other")).unwrap();
            let mut reply = [0u8; 4];
            assert_eq!(driver.dispatch(0, 1, &[2], &mut reply), (Status::OK, 1));
            assert_eq!(reply[0], 42);
            let mut items = [PackageItem::EMPTY; 2];
            assert_eq!(driver.describe(&mut items), Ok(3));
            assert_eq!(items[0].method, 4);
            let health = driver.health().unwrap();
            assert_eq!(
                (health.kind, health.reason.as_str()),
                (HEALTH_DEGRADED, "warm")
            );
            assert_eq!(driver.tick(5), Ok(Some(6)));
            assert_eq!(driver.tick(500), Ok(None));
            assert_eq!(driver.caller(8), Status::OK);
            assert_eq!(driver.caller_gone(8), Status::OK);
            // Panics inside the driver stop at its table.
            assert_eq!(driver.caller_gone(9), Status::PANIC);
            assert_eq!(driver.dispatch(0, 9, &[2], &mut reply).0, Status::PANIC);

            let before = DROPS.load(Ordering::Relaxed);
            drop(driver);
            assert_eq!(DROPS.load(Ordering::Relaxed), before + 1);
        }

        #[test]
        fn the_host_is_served_through_its_table_and_a_panic_is_a_status() {
            assert_eq!(HOST.struct_size as usize, size_of::<HostTable>());
            assert_eq!((HOST.release)(Handle::new(1, 1)), Status::PANIC);
            // A kind this host does not have answers for itself.
            let mut len = 0;
            let called = (HOST.device.call)(
                Handle::new(1, 1),
                1,
                Slice::EMPTY,
                SliceMut::empty(),
                Out::new(&mut len),
            );
            assert_eq!(called, Status::NOT_SUPPORTED);
            assert_eq!(
                (HOST.reset.assert_reset)(Handle::new(1, 1), 0),
                Status::NOT_SUPPORTED
            );
            let mut fd = Fd::NONE;
            assert_eq!(
                (HOST.stream.buffer_handle)(Handle::new(1, 1), 0, Out::new(&mut fd)),
                Status::NOT_SUPPORTED
            );
            assert!(fd.is_none());
            let mut byte = [0u8];
            let mut steps = [
                PackageTransfer::write(&[4]),
                PackageTransfer::read(&mut byte),
            ];
            assert_eq!(
                (HOST.i2c.transaction)(Handle::new(1, 1), 0x20, SliceMut::new(&mut steps)),
                Status::OK
            );
            assert_eq!(byte, [5]);
            // Within one table, what the host serves works, what it leaves out says so, and a
            // panic stops at the table.
            let mut value = 0;
            assert_eq!(
                (HOST.line.get)(Handle::new(1, 1), Out::new(&mut value)),
                Status::OK
            );
            assert_eq!(value, 1);
            assert_eq!(
                (HOST.line.configure)(Handle::new(1, 1), PackageLineConfig::default()),
                Status::NOT_SUPPORTED
            );
            assert_eq!((HOST.line.set)(Handle::new(1, 1), 1), Status::PANIC);
            // No binding at all is refused, not followed.
            let mut handle = Handle::NULL;
            let taken = (HOST.take)(
                core::ptr::null_mut(),
                1,
                Str::EMPTY,
                Str::EMPTY,
                Out::new(&mut handle),
            );
            assert_eq!(taken, Status::INVALID_ARGUMENT);
            assert_eq!((HOST.provided)(core::ptr::null_mut(), Str::EMPTY), 0);
        }
    }
}
