//! `include/lemnos/package.h` declares this module's boundary in C. This checks the header
//! against the Rust types: its version, hash and constants as text, and, where a C compiler is
//! installed, the size and alignment of every struct and the offset of every field.

use core::mem::{align_of, offset_of, size_of};
use std::format;
use std::process::Command;
use std::string::{String, ToString};
use std::vec::Vec;

use super::*;
use crate::AbiVersion;

/// A field's name in the header, where C cannot have the Rust one.
fn c_field(field: &str) -> &str {
    match field {
        "int" => "int_value",
        "float" => "float_value",
        "register" => "register_driver",
        other => other,
    }
}

macro_rules! c_struct {
    ($checks:ident, $c:literal, $t:ty $(, $field:ident)* $(,)?) => {
        $checks.push((format!("sizeof({})", $c), size_of::<$t>().to_string()));
        $checks.push((format!("_Alignof({})", $c), align_of::<$t>().to_string()));
        $($checks.push((
            format!("offsetof({}, {})", $c, c_field(stringify!($field))),
            offset_of!($t, $field).to_string(),
        ));)*
    };
}

fn c_table(checks: &mut Vec<(String, String)>, c: &str, size: usize, fields: &[(&str, usize)]) {
    checks.push((format!("sizeof({c})"), size.to_string()));
    for (field, offset) in fields {
        checks.push((format!("offsetof({c}, {field})"), offset.to_string()));
    }
}

#[test]
fn the_package_header_matches_the_crate() {
    let header = include_str!("../../../../include/lemnos/package.h");
    let define = |name: &str| {
        header
            .lines()
            .find_map(|l| {
                l.strip_prefix("#define ")?
                    .strip_prefix(name)?
                    .strip_prefix(' ')
            })
            .unwrap_or_else(|| panic!("{name} in package.h"))
            .trim()
    };
    assert_eq!(
        define("LEMNOS_PACKAGE_MAJOR"),
        format!("{}", PACKAGE_VERSION.major)
    );
    assert_eq!(
        define("LEMNOS_PACKAGE_MINOR"),
        format!("{}", PACKAGE_VERSION.minor)
    );
    assert_eq!(
        define("LEMNOS_PACKAGE_ENTRY"),
        format!("{:?}", PACKAGE_ENTRY.to_str().unwrap())
    );
    // The header gives the hash of 64-bit targets.
    #[cfg(target_pointer_width = "64")]
    {
        let hash = define("LEMNOS_PACKAGE_LAYOUT_HASH");
        let hash = u64::from_str_radix(hash.trim_start_matches("0x").trim_end_matches("ULL"), 16)
            .expect("hex hash");
        assert_eq!(
            hash, PACKAGE_LAYOUT_HASH,
            "LEMNOS_PACKAGE_LAYOUT_HASH should be {PACKAGE_LAYOUT_HASH:#018x}"
        );
    }

    if !Command::new("cc")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
    {
        return;
    }
    let mut checks: Vec<(String, String)> = Vec::new();
    let unsigned = |value: u64| format!("{value}ULL");
    for (name, value) in [
        ("LEMNOS_PACKAGE_MAGIC", unsigned(PACKAGE_MAGIC)),
        ("LEMNOS_FD_NONE", Fd::NONE.number().to_string()),
        ("LEMNOS_TAKE_DEVICE", TAKE_DEVICE.to_string()),
        ("LEMNOS_TAKE_I2C", TAKE_I2C.to_string()),
        ("LEMNOS_TAKE_SPI", TAKE_SPI.to_string()),
        ("LEMNOS_TAKE_LINE", TAKE_LINE.to_string()),
        ("LEMNOS_TAKE_PWM", TAKE_PWM.to_string()),
        ("LEMNOS_TAKE_UART", TAKE_UART.to_string()),
        ("LEMNOS_TAKE_USB_DEVICE", TAKE_USB_DEVICE.to_string()),
        ("LEMNOS_TAKE_USB_FUNCTION", TAKE_USB_FUNCTION.to_string()),
        ("LEMNOS_TAKE_REGISTERS", TAKE_REGISTERS.to_string()),
        ("LEMNOS_TAKE_INTERRUPT", TAKE_INTERRUPT.to_string()),
        ("LEMNOS_TAKE_DMA", TAKE_DMA.to_string()),
        ("LEMNOS_TAKE_CLOCKS", TAKE_CLOCKS.to_string()),
        ("LEMNOS_TAKE_RESET", TAKE_RESET.to_string()),
        ("LEMNOS_TAKE_STREAM", TAKE_STREAM.to_string()),
        ("LEMNOS_TAKE_RAW_SPI", TAKE_RAW_SPI.to_string()),
        ("LEMNOS_HEALTH_OK", HEALTH_OK.to_string()),
        ("LEMNOS_HEALTH_DEGRADED", HEALTH_DEGRADED.to_string()),
        ("LEMNOS_HEALTH_FAILED", HEALTH_FAILED.to_string()),
        ("LEMNOS_HEALTH_GONE", HEALTH_GONE.to_string()),
        ("LEMNOS_ITEM_HAS_MIN", ITEM_HAS_MIN.to_string()),
        ("LEMNOS_ITEM_HAS_MAX", ITEM_HAS_MAX.to_string()),
        ("LEMNOS_ITEM_HAS_FIELD", ITEM_HAS_FIELD.to_string()),
        ("LEMNOS_SETTING_INT", SETTING_INT.to_string()),
        ("LEMNOS_SETTING_FLOAT", SETTING_FLOAT.to_string()),
        ("LEMNOS_SETTING_BOOL", SETTING_BOOL.to_string()),
        ("LEMNOS_SETTING_TEXT", SETTING_TEXT.to_string()),
        ("LEMNOS_MAX_TRANSFER_STEPS", MAX_TRANSFER_STEPS.to_string()),
        ("LEMNOS_MAX_ISO_PACKETS", MAX_ISO_PACKETS.to_string()),
        ("LEMNOS_NO_TIMEOUT", unsigned(NO_TIMEOUT)),
        ("LEMNOS_NO_BUFFER", unsigned(NO_BUFFER.into())),
        ("LEMNOS_NO_JOB", unsigned(NO_JOB as u64)),
        ("LEMNOS_NO_TICK", unsigned(NO_TICK)),
        ("LEMNOS_BUFFERS_PLAIN", BUFFERS_PLAIN.to_string()),
        ("LEMNOS_BUFFERS_HANDLES", BUFFERS_HANDLES.to_string()),
        ("LEMNOS_TRANSFER_WRITE", TRANSFER_WRITE.to_string()),
        ("LEMNOS_TRANSFER_READ", TRANSFER_READ.to_string()),
        ("LEMNOS_TRANSFER_BOTH", TRANSFER_BOTH.to_string()),
        ("LEMNOS_TRANSFER_IN_PLACE", TRANSFER_IN_PLACE.to_string()),
        ("LEMNOS_TRANSFER_DELAY", TRANSFER_DELAY.to_string()),
        ("LEMNOS_USB_EVENT_NONE", USB_EVENT_NONE.to_string()),
        (
            "LEMNOS_USB_EVENT_CONFIGURED",
            USB_EVENT_CONFIGURED.to_string(),
        ),
        (
            "LEMNOS_USB_EVENT_DECONFIGURED",
            USB_EVENT_DECONFIGURED.to_string(),
        ),
        (
            "LEMNOS_USB_EVENT_SUSPENDED",
            USB_EVENT_SUSPENDED.to_string(),
        ),
        ("LEMNOS_USB_EVENT_RESUMED", USB_EVENT_RESUMED.to_string()),
        ("LEMNOS_USB_EVENT_SETUP", USB_EVENT_SETUP.to_string()),
        ("LEMNOS_DRIVER_TICKS", DRIVER_TICKS.to_string()),
        (
            "LEMNOS_DRIVER_TRACKS_CALLERS",
            DRIVER_TRACKS_CALLERS.to_string(),
        ),
    ] {
        checks.push((name.to_string(), value));
    }

    c_struct!(checks, "LemnosStr", Str<'static>, ptr, len);
    c_struct!(checks, "LemnosSlice", Slice<'static, u8>, ptr, len);
    c_struct!(checks, "LemnosSliceMut", SliceMut<'static, u8>, ptr, len);
    c_struct!(checks, "LemnosFd", Fd);
    c_struct!(checks, "size_t *", Out<'static, usize>);
    c_struct!(
        checks,
        "LemnosPackageNeed",
        PackageNeed,
        name,
        kind,
        optional,
        tag
    );
    c_struct!(
        checks,
        "LemnosPackageShape",
        PackageShape,
        kind,
        count,
        of,
        fields
    );
    c_struct!(checks, "LemnosPackageField", PackageField, name, shape);
    c_struct!(
        checks,
        "LemnosPackageMethod",
        PackageMethod,
        id,
        has_out_buffer,
        reserved,
        name,
        params,
        returns
    );
    c_struct!(
        checks,
        "LemnosPackageContract",
        PackageContract,
        tag,
        methods
    );
    c_struct!(
        checks,
        "LemnosPackageManifest",
        PackageManifest,
        name,
        needs,
        provides
    );
    c_struct!(
        checks,
        "LemnosPackageHealth",
        PackageHealth,
        kind,
        reserved,
        reason
    );
    c_struct!(
        checks,
        "LemnosPackageItem",
        PackageItem,
        kind,
        unit,
        exponent,
        flags,
        contract,
        reserved,
        method,
        min,
        max,
        name,
        field
    );
    c_struct!(
        checks,
        "LemnosPackageSetting",
        PackageSetting<'static>,
        key,
        kind,
        reserved,
        int,
        float,
        text
    );
    c_struct!(
        checks,
        "LemnosPackageTransfer",
        PackageTransfer<'static>,
        kind,
        delay_ns,
        write,
        read
    );
    c_struct!(
        checks,
        "LemnosPackageSpiSegment",
        PackageSpiSegment<'static>,
        mode,
        bits_per_word,
        cs_change,
        reserved,
        speed_hz,
        delay_us,
        reserved2,
        tx,
        rx
    );
    c_struct!(
        checks,
        "LemnosPackageLineConfig",
        PackageLineConfig,
        direction,
        initial,
        active_low,
        bias,
        drive,
        edge,
        reserved,
        debounce_us
    );
    c_struct!(
        checks,
        "LemnosPackageEdge",
        PackageEdge,
        present,
        rising,
        seq,
        reserved,
        timestamp_ns
    );
    c_struct!(
        checks,
        "LemnosPackagePwmConfig",
        PackagePwmConfig,
        period_ns,
        duty_ns,
        polarity,
        enabled
    );
    c_struct!(
        checks,
        "LemnosPackageInterruptEvent",
        PackageInterruptEvent,
        present,
        count,
        timestamp_ns
    );
    c_struct!(
        checks,
        "LemnosPackageDmaBuffer",
        PackageDmaBuffer,
        id,
        reserved,
        len,
        device_address
    );
    c_struct!(
        checks,
        "LemnosPackageFrame",
        PackageFrame,
        buffer,
        bytes,
        sequence,
        identity,
        timestamp_ns
    );
    c_struct!(
        checks,
        "LemnosPackageControlSetup",
        PackageControlSetup,
        request_type,
        request,
        value,
        index,
        reserved
    );
    c_struct!(
        checks,
        "LemnosPackageIsoPacket",
        PackageIsoPacket,
        capacity,
        received,
        ok
    );
    c_struct!(
        checks,
        "LemnosPackageUsbEvent",
        PackageUsbEvent,
        kind,
        length,
        setup
    );
    c_struct!(
        checks,
        "LemnosHostTable",
        HostTable,
        struct_size,
        reserved,
        provided,
        take,
        release,
        device,
        i2c,
        spi,
        line,
        pwm,
        uart,
        registers,
        interrupt,
        dma,
        clocks,
        reset,
        stream,
        usb_device,
        usb_function
    );
    c_struct!(
        checks,
        "LemnosDriverTable",
        DriverTable,
        struct_size,
        flags,
        bind,
        describe,
        health,
        dispatch,
        tick,
        caller,
        caller_gone,
        drop
    );
    c_struct!(
        checks,
        "LemnosPackageDriver",
        PackageDriver,
        manifest,
        table
    );
    c_struct!(
        checks,
        "LemnosPackageDescriptor",
        PackageDescriptor,
        magic,
        version,
        register
    );
    checks.push((
        "sizeof(LemnosAbiVersion)".to_string(),
        size_of::<AbiVersion>().to_string(),
    ));

    // Every function of every table, by the name it has in both.
    for (c, size, fields) in [
        (
            "LemnosDeviceTable",
            size_of::<DeviceTable>(),
            DeviceTable::FIELDS,
        ),
        ("LemnosI2cTable", size_of::<I2cTable>(), I2cTable::FIELDS),
        ("LemnosSpiTable", size_of::<SpiTable>(), SpiTable::FIELDS),
        ("LemnosLineTable", size_of::<LineTable>(), LineTable::FIELDS),
        ("LemnosPwmTable", size_of::<PwmTable>(), PwmTable::FIELDS),
        ("LemnosUartTable", size_of::<UartTable>(), UartTable::FIELDS),
        (
            "LemnosRegistersTable",
            size_of::<RegistersTable>(),
            RegistersTable::FIELDS,
        ),
        (
            "LemnosInterruptTable",
            size_of::<InterruptTable>(),
            InterruptTable::FIELDS,
        ),
        ("LemnosDmaTable", size_of::<DmaTable>(), DmaTable::FIELDS),
        (
            "LemnosClocksTable",
            size_of::<ClocksTable>(),
            ClocksTable::FIELDS,
        ),
        (
            "LemnosResetTable",
            size_of::<ResetTable>(),
            ResetTable::FIELDS,
        ),
        (
            "LemnosStreamTable",
            size_of::<StreamTable>(),
            StreamTable::FIELDS,
        ),
        (
            "LemnosUsbDeviceTable",
            size_of::<UsbDeviceTable>(),
            UsbDeviceTable::FIELDS,
        ),
        (
            "LemnosUsbFunctionTable",
            size_of::<UsbFunctionTable>(),
            UsbFunctionTable::FIELDS,
        ),
    ] {
        assert!(!fields.is_empty());
        c_table(&mut checks, c, size, fields);
    }
    assert!(checks.len() > 300, "{}", checks.len());

    let mut source = String::from("#include \"lemnos/package.h\"\n");
    for (expr, value) in &checks {
        source.push_str(&format!(
            "_Static_assert(({expr}) == ({value}), \"{expr}\");\n"
        ));
    }
    // The header alone, in C and in C++, and with the checks.
    let dir = std::env::temp_dir().join(format!("lemnos-package-header-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("check.c");
    std::fs::write(&file, source).unwrap();
    let out = Command::new("cc")
        .args([
            "-std=c11",
            "-Wall",
            "-Wextra",
            "-Wpedantic",
            "-Werror",
            "-fsyntax-only",
            "-I",
        ])
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/../../include"))
        .arg(&file)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
