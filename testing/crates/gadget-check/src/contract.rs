//! The managed link's identity, read from `contracts/usb-link.toml` by the host side of the check.
//!
//! The host expects what the file says and nothing the driver's constants say. The driver and this
//! check share no identity constant, so a changed constant in the driver fails here at the bus, and
//! so does a changed value in the file. The file is embedded at build time: an edit rebuilds the
//! check.

use toml::{Table, Value};

const FILE: &str = include_str!("../../../../contracts/usb-link.toml");

/// The identity values the host compares with what it receives.
pub struct Contract {
    pub vendor: u16,
    pub product: u16,
    pub bcd_device: u16,
    /// Class, subclass, protocol of the device descriptor.
    pub device_class: (u8, u8, u8),
    pub link_interface: u8,
    /// Class, subclass, protocol of the link's interface.
    pub link_class: (u8, u8, u8),
    pub interface_string: String,
    pub property: String,
    pub guid: String,
    pub link_compatible: String,
    pub ncm_compatible: String,
    pub acm_compatible: String,
    pub vendor_code: u8,
    pub qw_sign: String,
}

/// The contract as embedded in this build.
pub fn embedded() -> Result<Contract, String> {
    read(FILE)
}

/// Reads the contract text. Refuses a version other than 1 and any missing key.
pub fn read(text: &str) -> Result<Contract, String> {
    let table: Table = text.parse().map_err(|error| format!("not TOML: {error}"))?;
    if table.get("version").and_then(Value::as_integer) != Some(2) {
        return Err("version is not 2".to_string());
    }
    Ok(Contract {
        vendor: u16_at(&table, &["device", "vendor"])?,
        product: u16_at(&table, &["device", "product"])?,
        bcd_device: u16_at(&table, &["device", "bcd_device"])?,
        device_class: triple_at(&table, "device")?,
        link_interface: u8_at(&table, &["link", "interface"])?,
        link_class: triple_at(&table, "link")?,
        interface_string: text_at(&table, &["link", "interface_string"])?,
        property: text_at(&table, &["link", "property"])?,
        guid: text_at(&table, &["link", "guid"])?,
        link_compatible: text_at(&table, &["link", "compatible_id"])?,
        ncm_compatible: text_at(&table, &["functions", "ncm", "compatible_id"])?,
        acm_compatible: text_at(&table, &["functions", "acm", "compatible_id"])?,
        vendor_code: u8_at(&table, &["os", "vendor_code"])?,
        qw_sign: text_at(&table, &["os", "qw_sign"])?,
    })
}

fn at<'a>(table: &'a Table, path: &[&str]) -> Result<&'a Value, String> {
    let (last, parents) = path.split_last().ok_or("empty key path")?;
    let mut here = table;
    for key in parents {
        here = here
            .get(*key)
            .and_then(Value::as_table)
            .ok_or_else(|| format!("missing [{}]", parents.join(".")))?;
    }
    here.get(*last)
        .ok_or_else(|| format!("missing {}", path.join(".")))
}

fn int_at(table: &Table, path: &[&str]) -> Result<i64, String> {
    at(table, path)?
        .as_integer()
        .ok_or_else(|| format!("{} is not an integer", path.join(".")))
}

fn u16_at(table: &Table, path: &[&str]) -> Result<u16, String> {
    u16::try_from(int_at(table, path)?).map_err(|_| format!("{} is out of range", path.join(".")))
}

fn u8_at(table: &Table, path: &[&str]) -> Result<u8, String> {
    u8::try_from(int_at(table, path)?).map_err(|_| format!("{} is out of range", path.join(".")))
}

fn text_at(table: &Table, path: &[&str]) -> Result<String, String> {
    at(table, path)?
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| format!("{} is not a string", path.join(".")))
}

/// The class, subclass and protocol keys under `section`.
fn triple_at(table: &Table, section: &str) -> Result<(u8, u8, u8), String> {
    let (class, subclass, protocol) = if section == "device" {
        (
            ["device", "class"],
            ["device", "subclass"],
            ["device", "protocol"],
        )
    } else {
        (
            ["link", "class"],
            ["link", "subclass"],
            ["link", "protocol"],
        )
    };
    Ok((
        u8_at(table, &class)?,
        u8_at(table, &subclass)?,
        u8_at(table, &protocol)?,
    ))
}
