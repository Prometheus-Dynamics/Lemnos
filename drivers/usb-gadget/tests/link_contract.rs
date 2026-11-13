//! `contracts/usb-link.toml` against the code: the managed link's identity, read from the file
//! and compared with the constants. A reader refuses an unknown version and any unknown key, so
//! a file another team edits cannot silently mean something else.

use lemnos_driver_usb_gadget::{
    COMPOSITE_CLASS, LINK_INTERFACE_NAME, LINK_PRODUCT, LINK_VENDOR, revision,
};
use lemnos_hal::usb_descriptors::ms_os;
use toml::{Table, Value};

const FILE: &str = include_str!("../../../contracts/usb-link.toml");

/// Reads the contract, refusing a version other than 1, an unknown key at any level, and a
/// missing one. Returns the table.
fn read(text: &str) -> Result<Table, String> {
    let table: Table = text.parse().map_err(|error| format!("not TOML: {error}"))?;
    match table.get("version") {
        Some(Value::Integer(1)) => {}
        other => return Err(format!("unknown version {other:?}; this reader knows 1")),
    }
    let top = [
        "version",
        "device",
        "link",
        "functions",
        "os",
        "reservation",
    ];
    refuse_unknown(&table, &top, "top level")?;
    let tables = [
        (
            "device",
            &[
                "vendor",
                "product",
                "bcd_device",
                "class",
                "subclass",
                "protocol",
            ][..],
        ),
        (
            "link",
            &[
                "interface_string",
                "class",
                "subclass",
                "protocol",
                "interface",
                "property",
                "guid",
                "compatible_id",
            ][..],
        ),
        ("functions", &["ncm", "acm"][..]),
        ("os", &["vendor_code", "qw_sign"][..]),
        ("reservation", &["managed", "other"][..]),
    ];
    for (name, allowed) in tables {
        let value = table.get(name).ok_or(format!("missing [{name}]"))?;
        let inner = value.as_table().ok_or(format!("[{name}] is not a table"))?;
        if name == "functions" {
            for function in ["ncm", "acm"] {
                let part = inner
                    .get(function)
                    .and_then(Value::as_table)
                    .ok_or(format!("missing [functions.{function}]"))?;
                refuse_unknown(part, &["compatible_id"], &format!("functions.{function}"))?;
            }
            refuse_unknown(inner, allowed, name)?;
        } else if name == "reservation" {
            for range in ["managed", "other"] {
                let part = inner
                    .get(range)
                    .and_then(Value::as_table)
                    .ok_or(format!("missing [reservation.{range}]"))?;
                refuse_unknown(
                    part,
                    &["vendor", "product", "bcd_min", "bcd_max"],
                    &format!("reservation.{range}"),
                )?;
            }
            refuse_unknown(inner, allowed, name)?;
        } else {
            refuse_unknown(inner, allowed, name)?;
            for key in allowed {
                if !inner.contains_key(*key) {
                    return Err(format!("missing {name}.{key}"));
                }
            }
        }
    }
    Ok(table)
}

fn refuse_unknown(table: &Table, allowed: &[&str], at: &str) -> Result<(), String> {
    for key in table.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(format!("unknown key {at}.{key}"));
        }
    }
    Ok(())
}

fn int(table: &Table, path: &[&str]) -> i64 {
    let mut at = table;
    for key in &path[..path.len() - 1] {
        at = at[*key].as_table().expect("table");
    }
    at[path[path.len() - 1]].as_integer().expect("integer")
}

fn text<'a>(table: &'a Table, path: &[&str]) -> &'a str {
    let mut at = table;
    for key in &path[..path.len() - 1] {
        at = at[*key].as_table().expect("table");
    }
    at[path[path.len() - 1]].as_str().expect("string")
}

fn trimmed(id: &[u8; 8]) -> &str {
    let end = id.iter().position(|&b| b == 0).unwrap_or(id.len());
    std::str::from_utf8(&id[..end]).unwrap()
}

#[test]
fn the_file_reads_at_version_one() {
    read(FILE).expect("the contract reads");
}

#[test]
fn the_code_agrees_with_the_file() {
    let file = read(FILE).unwrap();
    assert_eq!(int(&file, &["device", "vendor"]), i64::from(LINK_VENDOR));
    assert_eq!(int(&file, &["device", "product"]), i64::from(LINK_PRODUCT));
    assert_eq!(
        int(&file, &["device", "bcd_device"]),
        i64::from(revision::RELEASE),
        "the default release is the file's bcd_device"
    );
    assert_eq!(
        (
            int(&file, &["device", "class"]),
            int(&file, &["device", "subclass"]),
            int(&file, &["device", "protocol"])
        ),
        COMPOSITE_CLASS
    );
    assert_eq!(
        text(&file, &["link", "interface_string"]),
        LINK_INTERFACE_NAME
    );
    assert_eq!(
        text(&file, &["link", "property"]),
        ms_os::LINK_GUID_PROPERTY
    );
    assert_eq!(text(&file, &["link", "guid"]), ms_os::LINK_GUID);
    assert_eq!(
        text(&file, &["link", "compatible_id"]),
        trimmed(&ms_os::LINK_COMPATIBLE_ID)
    );
    assert_eq!(
        text(&file, &["functions", "ncm", "compatible_id"]),
        trimmed(&ms_os::NCM_COMPATIBLE_ID)
    );
    assert_eq!(text(&file, &["functions", "acm", "compatible_id"]), "");
    assert_eq!(int(&file, &["link", "class"]), 0xff);
    assert_eq!(int(&file, &["link", "interface"]), 0);
    assert_eq!(
        int(&file, &["os", "vendor_code"]),
        i64::from(ms_os::VENDOR_CODE)
    );
    assert_eq!(text(&file, &["os", "qw_sign"]), ms_os::QW_SIGN);
}

#[test]
fn the_reservations_match_the_revision_policy() {
    let file = read(FILE).unwrap();
    assert_eq!(
        int(&file, &["reservation", "managed", "bcd_min"]),
        i64::from(revision::RESERVED_MIN)
    );
    assert_eq!(
        int(&file, &["reservation", "managed", "bcd_max"]),
        i64::from(revision::RESERVED_MAX)
    );
    assert_eq!(
        int(&file, &["reservation", "other", "bcd_min"]),
        i64::from(revision::OTHER_MIN)
    );
    assert_eq!(
        int(&file, &["reservation", "managed", "product"]),
        i64::from(LINK_PRODUCT)
    );
    let release = int(&file, &["device", "bcd_device"]);
    assert!(
        (0x0100..0x0b00).contains(&release),
        "the managed release {release:#06x} is in its range"
    );
}

#[test]
fn a_reader_refuses_a_version_it_does_not_know() {
    let two = FILE.replacen("version = 1", "version = 2", 1);
    assert!(read(&two).unwrap_err().contains("unknown version"));
}

#[test]
fn a_reader_refuses_an_unknown_key_at_any_level() {
    let top = FILE.replacen("version = 1", "version = 1\nextra = 1", 1);
    assert!(
        read(&top)
            .unwrap_err()
            .contains("unknown key top level.extra")
    );
    let device = FILE.replacen("[device]", "[device]\nextra = 1", 1);
    assert!(
        read(&device)
            .unwrap_err()
            .contains("unknown key device.extra")
    );
    let function = FILE.replacen("[functions.ncm]", "[functions.ncm]\nextra = 1", 1);
    assert!(
        read(&function)
            .unwrap_err()
            .contains("unknown key functions.ncm.extra")
    );
}

#[test]
fn a_reader_refuses_a_missing_key() {
    let missing = FILE.replacen("product = 0x0104", "", 1);
    assert!(
        read(&missing)
            .unwrap_err()
            .contains("missing device.product")
    );
}
