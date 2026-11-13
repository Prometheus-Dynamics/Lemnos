//! The host side of the check: a child process that finds the gadget on the bus, reads what it
//! presents, and loops the bulk pair.
//!
//! Every expected value comes from `contract::Contract`, which is the file, and from the USB and
//! CDC numbers the specifications give. None comes from the driver's constants: the driver and this
//! check share no identity value, so a changed driver constant shows up here as a mismatch. Each
//! line names every field it compares, so a FAIL shows what differed.

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use lemnos_linux_sys::usbfs::{ControlSetup, UsbfsFile};

use crate::contract::{self, Contract};
use crate::{CHUNK, EP_IN, EP_OUT, ROUNDS};

/// The device descriptor's fields, as the host reads them.
#[derive(Debug, PartialEq)]
struct Device {
    vendor: u16,
    product: u16,
    bcd_device: u16,
    class: (u8, u8, u8),
}

/// One interface setting, as the host reads it: every field is compared and printed.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Iface {
    number: u8,
    alternate: u8,
    class: u8,
    subclass: u8,
    protocol: u8,
}

/// The host side. `true` when every check passed.
pub fn host_side() -> bool {
    let contract = match contract::embedded() {
        Ok(contract) => contract,
        Err(error) => {
            say!("FAIL the contract file is read: {error}");
            return false;
        }
    };
    let Some(path) = scan_within(&contract, Duration::from_secs(60)) else {
        say!(
            "FAIL the host sees the gadget ({:04x}:{:04x}, the contract's) on the bus",
            contract.vendor,
            contract.product
        );
        return false;
    };
    let usb = match UsbfsFile::open(&path) {
        Ok(usb) => usb,
        Err(error) => {
            say!("FAIL open {}: {error}", path.display());
            return false;
        }
    };
    let mut descriptors = [0u8; 4096];
    let n = usb.read_descriptors(0, &mut descriptors).unwrap_or(0);
    let bytes = &descriptors[..n];

    let mut ok = device_descriptor(bytes, &contract);
    let seen = interfaces(bytes);
    ok &= interface_list(&seen, &contract);

    // The link is interface 0 on the bus, and no interface association covers it.
    let link_bus = seen
        .iter()
        .find(|i| (i.class, i.subclass, i.protocol) == contract.link_class)
        .map_or(contract.link_interface, |i| i.number);
    let link_is_zero = link_bus == 0;
    say!(
        "{} the link is interface {link_bus} on the bus (class {:02x}/{:02x}/{:02x}; layout requires 0)",
        if link_is_zero { "ok  " } else { "FAIL" },
        contract.link_class.0,
        contract.link_class.1,
        contract.link_class.2
    );
    ok &= link_is_zero;
    let covering = associations(bytes)
        .into_iter()
        .find(|&(first, count)| first <= link_bus && link_bus < first.saturating_add(count));
    match covering {
        None => say!("ok   no interface association covers interface {link_bus}"),
        Some((first, count)) => {
            say!("FAIL interface {link_bus} is covered by an association of {count} from {first}");
            ok = false;
        }
    }

    ok &= link_string(&usb, bytes, link_bus, &contract);
    ok &= os_descriptors(&usb, &contract, &seen, link_bus);

    if let Err(error) = usb.claim_interface(0) {
        say!("FAIL claim the vendor interface: {error}");
        return false;
    }
    let mut pattern = [0u8; CHUNK];
    let mut back = [0u8; CHUNK];
    for round in 0..ROUNDS {
        for (i, byte) in pattern.iter_mut().enumerate() {
            *byte = (round * 31 + i) as u8;
        }
        let sent = usb.bulk_out(EP_OUT, &pattern, 10_000);
        let got = usb.bulk_in(EP_IN, &mut back, 10_000);
        if sent.as_ref().ok() != Some(&CHUNK)
            || got.as_ref().ok() != Some(&CHUNK)
            || back != pattern
        {
            say!("FAIL round {round}: sent {sent:?}, received {got:?}");
            ok = false;
            break;
        }
    }
    if ok {
        say!("ok   {ROUNDS} rounds of {CHUNK} bytes went out and came back");
    }
    ok
}

/// The device descriptor: vendor, product, release and class, against the file.
fn device_descriptor(bytes: &[u8], contract: &Contract) -> bool {
    let seen = device(bytes);
    let want = Device {
        vendor: contract.vendor,
        product: contract.product,
        bcd_device: contract.bcd_device,
        class: contract.device_class,
    };
    let ok = seen.as_ref() == Some(&want);
    say!(
        "{} the device descriptor is {seen:?} (want {want:?})",
        if ok { "ok  " } else { "FAIL" }
    );
    ok
}

/// The interface list, every setting in order, against the link from the file and the CDC pair
/// the functions give (numbers 1 to 4, the USB CDC classes). The NCM data interface has its
/// endpoint-bearing alternate setting 1, which the CDC NCM specification requires.
fn interface_list(seen: &[Iface], contract: &Contract) -> bool {
    let (class, subclass, protocol) = contract.link_class;
    let want = [
        Iface {
            number: contract.link_interface,
            alternate: 0,
            class,
            subclass,
            protocol,
        },
        Iface {
            number: 1,
            alternate: 0,
            class: 0x02,
            subclass: 0x0d,
            protocol: 0x00,
        },
        Iface {
            number: 2,
            alternate: 0,
            class: 0x0a,
            subclass: 0x00,
            protocol: 0x01,
        },
        Iface {
            number: 2,
            alternate: 1,
            class: 0x0a,
            subclass: 0x00,
            protocol: 0x01,
        },
        Iface {
            number: 3,
            alternate: 0,
            class: 0x02,
            subclass: 0x02,
            protocol: 0x01,
        },
        Iface {
            number: 4,
            alternate: 0,
            class: 0x0a,
            subclass: 0x00,
            protocol: 0x00,
        },
    ];
    let ok = seen == want;
    say!(
        "{} the host sees {seen:?} (want {want:?})",
        if ok { "ok  " } else { "FAIL" }
    );
    ok
}

/// The link's interface string: the index in its interface descriptor, read as a string
/// descriptor in English, against the file.
fn link_string(usb: &UsbfsFile, bytes: &[u8], link: u8, contract: &Contract) -> bool {
    let Some(index) = interface_descriptors(bytes)
        .find(|d| d[2] == link && d[3] == 0)
        .map(|d| d[8])
    else {
        say!("FAIL the link interface {link} is not in the descriptors");
        return false;
    };
    if index == 0 {
        say!("FAIL the link interface {link} has no interface string");
        return false;
    }
    let setup = ControlSetup {
        request_type: 0x80,
        request: 6,
        value: 0x0300 | u16::from(index),
        index: 0x0409,
    };
    let got = fetch(usb, setup, 255).and_then(|text| {
        let len = usize::from(*text.first().unwrap_or(&0)).min(text.len());
        if text.get(1) != Some(&3) || len < 2 {
            return Err(format!("not a string descriptor: {text:02x?}"));
        }
        let (pairs, _) = text[2..len].as_chunks::<2>();
        let units: Vec<u16> = pairs.iter().map(|&pair| u16::from_le_bytes(pair)).collect();
        String::from_utf16(&units).map_err(|error| error.to_string())
    });
    let ok = got.as_deref() == Ok(contract.interface_string.as_str());
    say!(
        "{} the link's interface string is {got:?} (want {:?}, string {index})",
        if ok { "ok  " } else { "FAIL" },
        contract.interface_string
    );
    ok
}

/// The Microsoft OS 1.0 descriptors, requested the way Windows requests them and read back byte
/// for byte. Every expected byte comes from the file: the signature, the vendor code, the
/// compatible IDs per function (an empty ID means no entry), the property's name and GUID.
fn os_descriptors(usb: &UsbfsFile, contract: &Contract, seen: &[Iface], link: u8) -> bool {
    let mut ok = true;
    // The file's interface number for the link; the bus must agree (checked below).
    let file_link = contract.link_interface;

    let mut want = vec![0x12, 0x03];
    want.extend(wide(&contract.qw_sign, false));
    want.extend([contract.vendor_code, 0]);
    let got = fetch(
        usb,
        ControlSetup {
            request_type: 0x80,
            request: 6,
            value: 0x03ee,
            index: 0,
        },
        want.len() + 8,
    );
    ok &= expect("string 0xEE", &got, &want);

    // One entry per function that names a compatible ID, in interface order: the link first.
    let mut entries: Vec<(u8, [u8; 8])> = Vec::new();
    if !contract.link_compatible.is_empty() {
        match pad8(&contract.link_compatible) {
            Ok(id) => entries.push((file_link, id)),
            Err(error) => {
                say!("FAIL the link's compatible ID: {error}");
                ok = false;
            }
        }
    }
    for (name, id_text, class, subclass) in [
        ("NCM", &contract.ncm_compatible, 0x02, 0x0d),
        ("ACM", &contract.acm_compatible, 0x02, 0x02),
    ] {
        if id_text.is_empty() {
            continue;
        }
        let number = seen
            .iter()
            .find(|i| i.class == class && i.subclass == subclass)
            .map(|i| i.number);
        match (number, pad8(id_text)) {
            (Some(number), Ok(id)) => entries.push((number, id)),
            _ => {
                say!(
                    "FAIL the {name} function is not in the descriptors, or its ID is not 8 bytes"
                );
                ok = false;
            }
        }
    }
    let mut want = Vec::new();
    want.extend(((16 + 24 * entries.len()) as u32).to_le_bytes());
    want.extend([0x00, 0x01]);
    want.extend(4u16.to_le_bytes());
    want.push(entries.len() as u8);
    want.extend([0; 7]);
    for (first, id) in &entries {
        want.extend([*first, 1]);
        want.extend(id);
        want.extend([0; 8 + 6]);
    }
    let got = fetch(
        usb,
        ControlSetup {
            request_type: 0xc0,
            request: contract.vendor_code,
            value: 0,
            index: 4,
        },
        want.len() + 8,
    );
    ok &= expect("extended compatible IDs", &got, &want);
    // The first function section names the link: its first interface is the link's number on the bus.
    let first = got.as_ref().ok().and_then(|bytes| bytes.get(16)).copied();
    let names_link = first == Some(link);
    say!(
        "{} the compatible-ID function section names interface {first:?} (the link is {link} on the bus)",
        if names_link { "ok  " } else { "FAIL" }
    );
    ok &= names_link;

    let name = wide(&contract.property, true);
    let data = wide(&contract.guid, true);
    let size = 14 + name.len() + data.len();
    let mut want = Vec::new();
    want.extend(((10 + size) as u32).to_le_bytes());
    want.extend([0x00, 0x01]);
    want.extend(5u16.to_le_bytes());
    want.extend(1u16.to_le_bytes());
    want.extend((size as u32).to_le_bytes());
    want.extend(1u32.to_le_bytes());
    want.extend((name.len() as u16).to_le_bytes());
    want.extend(&name);
    want.extend((data.len() as u32).to_le_bytes());
    want.extend(&data);
    let got = fetch(
        usb,
        ControlSetup {
            request_type: 0xc1,
            request: contract.vendor_code,
            value: u16::from(link),
            index: 5,
        },
        want.len() + 8,
    );
    ok &= expect("extended properties of interface 0", &got, &want);
    ok
}

/// One control read in, at most `len` bytes.
fn fetch(usb: &UsbfsFile, setup: ControlSetup, len: usize) -> Result<Vec<u8>, String> {
    let mut got = vec![0u8; len];
    match usb.control_in(setup, &mut got, 5000) {
        Ok(n) => {
            got.truncate(n);
            Ok(got)
        }
        Err(error) => Err(error.to_string()),
    }
}

/// Says whether a read returned exactly `want`, and prints what it returned if not.
fn expect(what: &str, got: &Result<Vec<u8>, String>, want: &[u8]) -> bool {
    match got {
        Ok(bytes) if bytes == want => {
            say!("ok   {what}: {} bytes as the host reads them", bytes.len());
            true
        }
        Ok(bytes) => {
            say!("FAIL {what}: got {bytes:02x?}, want {want:02x?}");
            false
        }
        Err(error) => {
            say!("FAIL {what}: {error}");
            false
        }
    }
}

/// UTF-16LE of `text`, with a terminating NUL when `nul`.
fn wide(text: &str, nul: bool) -> Vec<u8> {
    let mut units: Vec<u16> = text.encode_utf16().collect();
    if nul {
        units.push(0);
    }
    units.iter().flat_map(|unit| unit.to_le_bytes()).collect()
}

/// An ASCII compatible ID, NUL-padded to the descriptor's 8 bytes.
fn pad8(text: &str) -> Result<[u8; 8], String> {
    if !text.is_ascii() || text.len() > 8 {
        return Err(format!("{text:?} is not an ASCII ID of at most 8 bytes"));
    }
    let mut out = [0u8; 8];
    out[..text.len()].copy_from_slice(text.as_bytes());
    Ok(out)
}

/// The device descriptor: the first 18 bytes of the read.
fn device(bytes: &[u8]) -> Option<Device> {
    if bytes.len() < 18 || bytes[1] != 1 {
        return None;
    }
    Some(Device {
        vendor: u16::from_le_bytes([bytes[8], bytes[9]]),
        product: u16::from_le_bytes([bytes[10], bytes[11]]),
        bcd_device: u16::from_le_bytes([bytes[12], bytes[13]]),
        class: (bytes[4], bytes[5], bytes[6]),
    })
}

/// The descriptors after the device's: each has a length, then a type. A length that runs past
/// the end, or below 2, ends the walk.
fn walk(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut at = 0;
    std::iter::from_fn(move || {
        if at + 2 > bytes.len() {
            return None;
        }
        let len = usize::from(bytes[at]);
        if len < 2 || at + len > bytes.len() {
            return None;
        }
        let descriptor = &bytes[at..at + len];
        at += len;
        Some(descriptor)
    })
}

/// The interface descriptors (type 4), each as the host reads it.
fn interface_descriptors(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    walk(bytes).filter(|d| d[1] == 4 && d.len() >= 9)
}

fn interfaces(bytes: &[u8]) -> Vec<Iface> {
    interface_descriptors(bytes)
        .map(|d| Iface {
            number: d[2],
            alternate: d[3],
            class: d[5],
            subclass: d[6],
            protocol: d[7],
        })
        .collect()
}

/// The interface associations (type 11): the first interface and the count each covers.
fn associations(bytes: &[u8]) -> Vec<(u8, u8)> {
    walk(bytes)
        .filter(|d| d[1] == 0x0b && d.len() >= 8)
        .map(|d| (d[2], d[3]))
        .collect()
}

/// The bus path of the gadget, by the contract's vendor and product numbers, once it is listed.
/// `None` if the contract cannot be read or the device does not appear within `within`.
pub(crate) fn find_device(within: Duration) -> Option<PathBuf> {
    let contract = contract::embedded().ok()?;
    scan_within(&contract, within)
}

fn scan_within(contract: &Contract, within: Duration) -> Option<PathBuf> {
    let until = Instant::now() + within;
    loop {
        if let Some(path) = scan(contract) {
            return Some(path);
        }
        if Instant::now() >= until {
            return None;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn scan(contract: &Contract) -> Option<PathBuf> {
    let (vendor, product) = (
        format!("{:04x}", contract.vendor),
        format!("{:04x}", contract.product),
    );
    for entry in fs::read_dir("/sys/bus/usb/devices").ok()?.flatten() {
        let dir = entry.path();
        let text = |name: &str| {
            fs::read_to_string(dir.join(name))
                .ok()
                .map(|s| s.trim().to_string())
        };
        if text("idVendor").as_deref() != Some(vendor.as_str())
            || text("idProduct").as_deref() != Some(product.as_str())
        {
            continue;
        }
        let bus: u32 = text("busnum")?.parse().ok()?;
        let dev: u32 = text("devnum")?.parse().ok()?;
        let path = PathBuf::from(format!("/dev/bus/usb/{bus:03}/{dev:03}"));
        if path.exists() {
            return Some(path);
        }
    }
    None
}
