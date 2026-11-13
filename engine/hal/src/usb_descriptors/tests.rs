extern crate alloc;

use alloc::vec::Vec;

use super::*;

const DEVICE_BYTES: [u8; 18] = [
    18, 1, 0x00, 0x02, 0xef, 2, 1, 64, 0x09, 0x12, 0x34, 0x12, 0x02, 0x01, 1, 2, 3, 1,
];
const INTERFACE_BYTES: [u8; 9] = [9, 4, 0, 0, 2, 0xff, 0, 0, 4];

fn endpoint(address: u8, attributes: u8, max_packet: u16) -> [u8; 7] {
    let [low, high] = max_packet.to_le_bytes();
    [7, 5, address, attributes, low, high, 0]
}

/// A configuration holding `parts`, saying it has `interfaces`.
fn configuration(interfaces: u8, parts: &[&[u8]]) -> Vec<u8> {
    let mut bytes = alloc::vec![9, 2, 0, 0, interfaces, 1, 5, 0xc0, 50];
    for part in parts {
        bytes.extend_from_slice(part);
    }
    let total = (bytes.len() as u16).to_le_bytes();
    bytes[2..4].copy_from_slice(&total);
    bytes
}

fn string(text: &str) -> Vec<u8> {
    let mut bytes = alloc::vec![0, 3];
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes[0] = bytes.len() as u8;
    bytes
}

fn strings() -> Vec<u8> {
    let mut bytes = alloc::vec![4, 3, 0x09, 0x04];
    for text in ["Maker", "Thing", "0001", "data", "config \u{1f600}"] {
        bytes.extend(string(text));
    }
    bytes
}

fn speed(max_packet: u16) -> Vec<u8> {
    configuration(
        1,
        &[
            &INTERFACE_BYTES,
            &endpoint(0x81, 2, max_packet),
            &endpoint(0x02, 2, max_packet),
        ],
    )
}

fn join(parts: &[&[u8]]) -> Vec<u8> {
    parts.concat()
}

/// Device, both speeds and the string table.
fn whole() -> Vec<u8> {
    join(&[&DEVICE_BYTES, &speed(64), &speed(512), &strings()])
}

fn refused(bytes: &[u8]) -> bool {
    UsbDescriptors::parse(bytes) == Err(ErrorKind::InvalidInput)
}

#[test]
fn a_whole_set_is_taken_apart_where_its_own_lengths_say() {
    let bytes = whole();
    let set = UsbDescriptors::parse(&bytes).unwrap();
    assert_eq!(set.bytes(), bytes);
    assert_eq!(set.device(), DEVICE_BYTES);
    assert_eq!(
        set.identity(),
        UsbIdentity {
            usb: 0x0200,
            class: 0xef,
            subclass: 2,
            protocol: 1,
            max_packet: 64,
            vendor: 0x1209,
            product: 0x1234,
            release: 0x0102,
            manufacturer: 1,
            product_name: 2,
            serial: 3,
        }
    );
    assert_eq!(
        set.configuration(),
        UsbConfiguration {
            interfaces: 1,
            value: 1,
            name: 5,
            attributes: 0xc0,
            max_power: 50,
        }
    );
    assert_eq!(set.full_speed(), speed(64));
    assert_eq!(set.high_speed(), Some(&speed(512)[..]));
    assert_eq!(set.at_speed(UsbSpeed::Full), speed(64));
    assert_eq!(set.at_speed(UsbSpeed::High), speed(512));
    assert_eq!(set.at_speed(UsbSpeed::Super), speed(64));
    let high: Vec<UsbEndpoint> = endpoints(set.at_speed(UsbSpeed::High)).collect();
    assert_eq!(
        high,
        [
            UsbEndpoint {
                address: 0x81,
                attributes: 2,
                max_packet: 512,
                interval: 0
            },
            UsbEndpoint {
                address: 0x02,
                attributes: 2,
                max_packet: 512,
                interval: 0
            },
        ]
    );
    assert_eq!(set.string_count(), 6);
    assert_eq!(set.language(), Some(0x0409));
    assert_eq!(set.string(0), Some(&[4, 3, 0x09, 0x04][..]));
    assert_eq!(set.string(2), Some(&string("Thing")[..]));
    assert_eq!(set.string(6), None);
    assert!(set.text(0).is_none());
    let text: Vec<u16> = set.text(5).unwrap().collect();
    assert_eq!(
        text,
        "config \u{1f600}".encode_utf16().collect::<Vec<u16>>()
    );
}

#[test]
fn one_speed_and_no_strings_is_a_set_when_nothing_names_a_string() {
    let mut device = DEVICE_BYTES;
    device[14..17].fill(0);
    let mut interface = INTERFACE_BYTES;
    interface[4] = 1;
    interface[8] = 0;
    let mut config = configuration(1, &[&interface, &endpoint(0x81, 3, 8), &[9, 0x21, 0]]);
    config[6] = 0;
    // The descriptor that doesn't fit its own length is the class descriptor above.
    assert!(refused(&join(&[&device, &config])));
    let mut config = configuration(
        1,
        &[&interface, &[3, 0x21, 0], &endpoint(0x81, 3, 8), &[2, 0x25]],
    );
    config[6] = 0;
    let bytes = join(&[&device, &config]);
    let set = UsbDescriptors::parse(&bytes).unwrap();
    assert_eq!(set.high_speed(), None);
    assert_eq!(set.at_speed(UsbSpeed::High), config);
    assert_eq!((set.string_count(), set.language()), (0, None));
    assert_eq!(endpoints(set.full_speed()).count(), 1);
    // A string index with no table behind it.
    assert!(refused(&join(&[&DEVICE_BYTES, &config])));
}

#[test]
fn alternate_settings_count_up_and_each_has_its_own_endpoints() {
    let alt = |number, alternate, endpoints| [9, 4, number, alternate, endpoints, 0xff, 0, 0, 0];
    let a = endpoint(0x81, 1, 64);
    let good = configuration(2, &[&alt(0, 0, 0), &alt(0, 1, 1), &a, &alt(1, 0, 1), &a]);
    let device = {
        let mut device = DEVICE_BYTES;
        device[14..17].fill(0);
        device
    };
    let set = |mut config: Vec<u8>| {
        config[6] = 0;
        join(&[&device, &config])
    };
    assert!(UsbDescriptors::parse(&set(good)).is_ok());
    let bad: [Vec<u8>; 7] = [
        // Starts at interface 1.
        configuration(1, &[&alt(1, 0, 0)]),
        // Skips interface 1.
        configuration(2, &[&alt(0, 0, 0), &alt(2, 0, 0)]),
        // Skips alternate setting 1.
        configuration(1, &[&alt(0, 0, 0), &alt(0, 2, 0)]),
        // Says two interfaces and has one.
        configuration(2, &[&alt(0, 0, 0)]),
        // An interface that owes an endpoint.
        configuration(1, &[&alt(0, 0, 2), &a]),
        // An endpoint more than the interface says.
        configuration(1, &[&alt(0, 0, 0), &a]),
        // The same address twice in one setting.
        configuration(1, &[&alt(0, 0, 2), &a, &a]),
    ];
    for (n, config) in bad.into_iter().enumerate() {
        assert!(refused(&set(config)), "case {n}");
    }
}

#[test]
fn every_length_and_count_is_checked() {
    let good = whole();
    assert!(UsbDescriptors::parse(&good).is_ok());
    let full = DEVICE_LEN;
    let high = full + speed(64).len();
    let table = high + speed(512).len();
    // (offset, value, what it breaks)
    let cases: [(usize, u8, &str); 27] = [
        (0, 17, "device bLength"),
        (1, 2, "device type"),
        (7, 9, "bMaxPacketSize0"),
        (14, 6, "iManufacturer past the table"),
        (16, 200, "iSerialNumber past the table"),
        (17, 2, "bNumConfigurations"),
        (full, 8, "configuration bLength"),
        (full + 1, 4, "configuration type"),
        (full + 2, 31, "wTotalLength short of its descriptors"),
        (full + 2, 33, "wTotalLength past its descriptors"),
        (full + 3, 0xff, "wTotalLength past everything"),
        (full + 4, 2, "bNumInterfaces"),
        (full + 5, 0, "bConfigurationValue"),
        (full + 6, 6, "iConfiguration past the table"),
        (full + 7, 0x40, "bmAttributes without bit 7"),
        (full + 7, 0x81, "bmAttributes reserved bit"),
        (full + 9, 8, "interface bLength"),
        (full + 9 + 8, 9, "iInterface past the table"),
        (full + 18, 6, "endpoint bLength"),
        (full + 18 + 2, 0x80, "endpoint number 0"),
        (full + 18 + 2, 0x91, "endpoint address reserved bit"),
        (full + 18 + 3, 0, "control endpoint"),
        (high + 5, 2, "high speed: another bConfigurationValue"),
        (high + 18 + 2, 0x83, "high speed: another endpoint address"),
        (high + 18 + 3, 3, "high speed: another transfer type"),
        (table, 6, "two languages"),
        (table + 4, 13, "odd string length"),
    ];
    for (at, value, what) in cases {
        let mut bytes = good.clone();
        assert_ne!(bytes[at], value, "{what}: the case changes nothing");
        bytes[at] = value;
        assert!(refused(&bytes), "{what}");
    }
    // Cut anywhere, it is refused or it is a shorter set that is whole in itself.
    for len in 0..good.len() {
        if let Ok(set) = UsbDescriptors::parse(&good[..len]) {
            assert!(set.string_count() < 6, "cut at {len}");
        }
    }
    assert!(
        refused(&good[..high]),
        "strings are named and the table is cut off"
    );
    assert!(refused(&join(&[&good, &[0]])), "a byte after the table");
    assert!(refused(&join(&[&good, &[9, 2, 9, 0, 0, 1, 0, 0x80, 0]])));
    assert!(refused(&join(&[
        &DEVICE_BYTES,
        &speed(64),
        &speed(64),
        &speed(64),
        &strings()
    ])));
}

#[test]
fn strings_are_whole_utf16_without_nul() {
    let with = |text: &[u16]| {
        let mut bytes = join(&[&DEVICE_BYTES, &speed(64), &strings()]);
        bytes.push(2 + 2 * text.len() as u8);
        bytes.push(3);
        for unit in text {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        bytes
    };
    assert!(UsbDescriptors::parse(&with(&[0x41, 0xd83d, 0xde00])).is_ok());
    assert!(UsbDescriptors::parse(&with(&[])).is_ok());
    assert!(refused(&with(&[0x41, 0])), "NUL");
    assert!(refused(&with(&[0xd83d])), "high surrogate alone");
    assert!(refused(&with(&[0xd83d, 0x41])), "high surrogate unpaired");
    assert!(refused(&with(&[0xde00])), "low surrogate alone");
    assert!(
        refused(&with(&[0xd83d, 0xd83d, 0xde00])),
        "two high surrogates"
    );
}

#[test]
fn a_descriptor_kind_that_belongs_outside_a_configuration_is_refused_inside_one() {
    for kind in [DEVICE, CONFIGURATION, STRING] {
        let mut device = DEVICE_BYTES;
        device[14..17].fill(0);
        let mut interface = INTERFACE_BYTES;
        interface[4] = 0;
        interface[8] = 0;
        let mut config = configuration(1, &[&interface, &[4, kind, 0, 0]]);
        config[6] = 0;
        assert!(refused(&join(&[&device, &config])), "type {kind}");
    }
}

#[test]
fn an_interface_name_is_one_template_with_one_number_the_kernel_accepts() {
    // A template with exactly one `%d`, the kernel's own rule.
    let name = IfName::new("lemnos%d").expect("a template");
    assert_eq!(name.as_str(), "lemnos%d");
    assert!(IfName::new("usb%d").is_some());
    // The longest the kernel's IFNAMSIZ allows: 15 bytes.
    assert!(IfName::new("abcdefghijklm%d").is_some());
    assert_eq!(IfName::new("abcdefghijklmn%d"), None, "16 bytes");
    // A fixed name is refused: the kernel refuses it too.
    assert_eq!(IfName::new("lemnos0"), None);
    assert_eq!(IfName::new("%d%d"), None, "two numbers");
    assert_eq!(IfName::new("%d%%"), None, "a second percent sign");
    assert_eq!(IfName::new(""), None);
    assert_eq!(IfName::new("a/%d"), None, "a slash");
    assert_eq!(IfName::new("a:%d"), None, "a colon");
    assert_eq!(IfName::new("a %d"), None, "a space");
    assert_eq!(IfName::new("a\u{e9}%d"), None, "not ASCII");
}
