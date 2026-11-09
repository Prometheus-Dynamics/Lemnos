//! The backend over fixture sysfs and devfs directories and a recording stand-in for usbdevfs. No
//! test opens, claims or resets a real USB device, and none touches `/dev/bus/usb`.

use std::cell::Cell;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use lemnos_hal::ErrorKind;
use lemnos_hal::blocks::{ControlSetup, IsoPacket, UsbDeviceId, UsbDeviceInfo, UsbSpeed};
use lemnos_hal::{UsbDevice, UsbHost};
use lemnos_linux_sys::usbfs::UsbfsFile;
use lemnos_linux_sys::usbfs::fake::{Call, CraftedUrb, FakeUsbfs, Reply};
use lemnos_measure::test_alloc::ALLOCATIONS;

use crate::error::{kind_of, kind_of_errno, timeout_ms};
use crate::{UsbfsDevice, UsbfsHost};

const TIMEOUT: Duration = Duration::from_millis(50);

const VENDOR_IN: ControlSetup = ControlSetup {
    request_type: 0xc0,
    request: 1,
    value: 2,
    index: 3,
};

/// Fixture directories under the temporary directory, removed when dropped.
struct Fixture {
    root: PathBuf,
    sys: PathBuf,
    dev: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let root = std::env::temp_dir().join(format!(
            "lemnos-usb-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let (sys, dev) = (root.join("sys"), root.join("dev"));
        fs::create_dir_all(&sys).unwrap();
        fs::create_dir_all(&dev).unwrap();
        Self { root, sys, dev }
    }

    fn host(&self) -> UsbfsHost {
        UsbfsHost::at(&self.sys, &self.dev)
    }

    /// A sysfs device directory with `attributes`.
    fn device(&self, name: &str, attributes: &[(&str, &str)]) {
        let dir = self.sys.join(name);
        fs::create_dir_all(&dir).unwrap();
        for (attribute, value) in attributes {
            fs::write(dir.join(attribute), format!("{value}\n")).unwrap();
        }
    }

    /// A well-formed device.
    fn plain(&self, name: &str, bus: &str, address: &str) {
        self.device(
            name,
            &[
                ("busnum", bus),
                ("devnum", address),
                ("idVendor", "1d6b"),
                ("idProduct", "0002"),
                ("bDeviceClass", "09"),
                ("speed", "480"),
            ],
        );
    }

    /// A regular file at the device node path, holding `descriptors`.
    fn node(&self, bus: &str, address: &str, descriptors: &[u8]) {
        let dir = self.dev.join(bus);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(address), descriptors).unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

const NONE: UsbDeviceInfo = UsbDeviceInfo {
    id: UsbDeviceId { bus: 0, address: 0 },
    vendor: 0,
    product: 0,
    class: 0,
    speed: UsbSpeed::Unknown,
};

fn device() -> (UsbfsDevice, FakeUsbfs) {
    let fake = FakeUsbfs::new();
    let device = UsbfsDevice::from_file(UsbfsFile::fake(fake.clone()));
    (device, fake)
}

/// A device and configuration descriptors: config 0 of 12 bytes, config 1 of 9.
fn blob() -> Vec<u8> {
    let mut bytes = vec![18, 1, 0, 2, 0, 0, 0, 64, 0x6b, 0x1d, 2, 0, 0, 1, 1, 2, 3, 2];
    bytes.extend_from_slice(&[9, 2, 12, 0, 1, 1, 0, 0x80, 50, 7, 7, 7]);
    bytes.extend_from_slice(&[9, 2, 9, 0, 0, 2, 0, 0x80, 25]);
    bytes
}

fn open_descriptor_file(fixture: &Fixture, descriptors: &[u8]) -> UsbfsDevice {
    fixture.node("001", "002", descriptors);
    fixture
        .host()
        .open(UsbDeviceId { bus: 1, address: 2 })
        .unwrap()
}

// Enumeration, from fixture directories.

#[test]
fn the_default_host_names_the_system_roots() {
    let host = UsbfsHost::default();
    let text = format!("{host:?}");
    assert!(text.contains("/sys/bus/usb/devices"), "{text}");
    assert!(text.contains("/dev/bus/usb"), "{text}");
}

#[test]
fn enumeration_reads_each_attribute() {
    let fixture = Fixture::new();
    fixture.device(
        "3-1.2",
        &[
            ("busnum", "3"),
            ("devnum", "7"),
            ("idVendor", "1d50"),
            ("idProduct", "6018"),
            ("bDeviceClass", "ef"),
            ("speed", "5000"),
        ],
    );
    let mut out = [NONE; 4];
    assert_eq!(fixture.host().enumerate(&mut out).unwrap(), 1);
    assert_eq!(
        out[0],
        UsbDeviceInfo {
            id: UsbDeviceId { bus: 3, address: 7 },
            vendor: 0x1d50,
            product: 0x6018,
            class: 0xef,
            speed: UsbSpeed::Super,
        }
    );
    assert_eq!(out[1], NONE, "the rest of the slice is left alone");
}

#[test]
fn enumeration_leaves_out_root_hubs_and_interfaces() {
    let fixture = Fixture::new();
    fixture.plain("usb3", "3", "1");
    fixture.plain("3-1:1.0", "3", "2");
    fixture.plain("3-1", "3", "2");
    let mut out = [NONE; 4];
    assert_eq!(fixture.host().enumerate(&mut out).unwrap(), 1);
    assert_eq!(out[0].id, UsbDeviceId { bus: 3, address: 2 });
}

#[test]
fn enumeration_orders_by_bus_and_address() {
    let fixture = Fixture::new();
    fixture.plain("2-1", "2", "9");
    fixture.plain("1-2", "1", "4");
    fixture.plain("1-1", "1", "3");
    let mut out = [NONE; 4];
    assert_eq!(fixture.host().enumerate(&mut out).unwrap(), 3);
    let ids: Vec<_> = out[..3].iter().map(|d| (d.id.bus, d.id.address)).collect();
    assert_eq!(ids, vec![(1, 3), (1, 4), (2, 9)]);
}

#[test]
fn enumeration_counts_past_what_fits_and_keeps_the_lowest() {
    let fixture = Fixture::new();
    for address in 1..=5u8 {
        fixture.plain(&format!("1-{address}"), "1", &address.to_string());
    }
    let mut out = [NONE; 3];
    assert_eq!(fixture.host().enumerate(&mut out).unwrap(), 5);
    let ids: Vec<_> = out.iter().map(|d| d.id.address).collect();
    assert_eq!(ids, vec![1, 2, 3]);

    let mut empty: [UsbDeviceInfo; 0] = [];
    assert_eq!(fixture.host().enumerate(&mut empty).unwrap(), 5);
}

#[test]
fn enumeration_skips_entries_it_cannot_read() {
    let fixture = Fixture::new();
    fixture.plain("1-1", "1", "1");
    // Each has one attribute out of range, garbled, or missing.
    fixture.plain("1-2", "256", "2");
    fixture.plain("1-3", "1", "999");
    fixture.plain("1-4", "1", "0");
    fixture.plain("1-5", "0", "4");
    fixture.plain("1-6", "-1", "4");
    fixture.plain("1-7", "1", "not a number");
    fixture.device(
        "1-8",
        &[
            ("busnum", "1"),
            ("devnum", "8"),
            ("idVendor", "zzzz"),
            ("idProduct", "0002"),
            ("bDeviceClass", "09"),
        ],
    );
    fixture.device(
        "1-9",
        &[
            ("busnum", "1"),
            ("devnum", "9"),
            ("idVendor", "12345"),
            ("idProduct", "0002"),
            ("bDeviceClass", "09"),
        ],
    );
    fixture.device("1-10", &[("busnum", "1")]);
    fixture.device("1-11", &[]);
    let mut out = [NONE; 8];
    assert_eq!(fixture.host().enumerate(&mut out).unwrap(), 1);
    assert_eq!(out[0].id, UsbDeviceId { bus: 1, address: 1 });
}

#[test]
fn enumeration_reads_a_long_attribute_only_as_far_as_its_buffer() {
    let fixture = Fixture::new();
    let long = "1".repeat(1024);
    fixture.plain("1-1", &long, "1");
    let mut out = [NONE; 1];
    assert_eq!(fixture.host().enumerate(&mut out).unwrap(), 0);
}

#[test]
fn speeds_map_to_the_table() {
    let fixture = Fixture::new();
    let table = [
        ("1.5", UsbSpeed::Low),
        ("12", UsbSpeed::Full),
        ("480", UsbSpeed::High),
        ("5000", UsbSpeed::Super),
        ("10000", UsbSpeed::SuperPlus),
        ("20000", UsbSpeed::SuperPlus),
        ("unknown", UsbSpeed::Unknown),
    ];
    for (index, (text, speed)) in table.iter().enumerate() {
        let name = format!("1-{}", index + 1);
        fixture.device(
            &name,
            &[
                ("busnum", "1"),
                ("devnum", &(index + 1).to_string()),
                ("idVendor", "0001"),
                ("idProduct", "0002"),
                ("bDeviceClass", "00"),
                ("speed", text),
            ],
        );
        let mut out = [NONE; 8];
        fixture.host().enumerate(&mut out).unwrap();
        assert_eq!(out[index].speed, *speed, "{text}");
    }
    // An absent speed attribute doesn't drop the device.
    fixture.device(
        "2-1",
        &[
            ("busnum", "2"),
            ("devnum", "1"),
            ("idVendor", "0001"),
            ("idProduct", "0002"),
            ("bDeviceClass", "00"),
        ],
    );
    let mut out = [NONE; 8];
    assert_eq!(fixture.host().enumerate(&mut out).unwrap(), 8);
    assert_eq!(out[7].speed, UsbSpeed::Unknown);
}

#[test]
fn a_kernel_without_usb_has_nothing_plugged_in() {
    let fixture = Fixture::new();
    let mut host = UsbfsHost::at(&fixture.root.join("absent"), &fixture.dev);
    let mut out = [NONE; 1];
    assert_eq!(host.enumerate(&mut out).unwrap(), 0);
}

#[test]
fn a_sysfs_root_that_cannot_be_read_is_an_error() {
    let fixture = Fixture::new();
    let not_a_directory = fixture.root.join("file");
    fs::write(&not_a_directory, b"").unwrap();
    let mut host = UsbfsHost::at(&not_a_directory, &fixture.dev);
    assert_eq!(host.enumerate(&mut [NONE; 1]), Err(ErrorKind::Failed));
}

// Opening nodes, under the fixture devfs.

#[test]
fn open_names_the_node_in_the_kernels_layout() {
    let fixture = Fixture::new();
    fixture.node("001", "002", &blob());
    let mut device = fixture
        .host()
        .open(UsbDeviceId { bus: 1, address: 2 })
        .unwrap();
    let mut out = [0u8; 12];
    assert_eq!(device.config_descriptor(0, &mut out), Ok(12));
    assert_eq!(out[..4], [9, 2, 12, 0]);
}

#[test]
fn an_absent_node_is_not_found() {
    let fixture = Fixture::new();
    let id = UsbDeviceId { bus: 9, address: 9 };
    assert_eq!(fixture.host().open(id).unwrap_err(), ErrorKind::NotFound);
}

#[test]
fn an_impossible_id_is_not_found_without_a_look() {
    let fixture = Fixture::new();
    fixture.node("000", "002", &blob());
    let bus0 = UsbDeviceId { bus: 0, address: 2 };
    assert_eq!(fixture.host().open(bus0).unwrap_err(), ErrorKind::NotFound);
    let address0 = UsbDeviceId { bus: 2, address: 0 };
    assert_eq!(
        fixture.host().open(address0).unwrap_err(),
        ErrorKind::NotFound
    );
}

// Descriptors, from plain files. Every length in them is untrusted.

#[test]
fn a_configuration_is_found_by_walking_the_ones_before_it() {
    let fixture = Fixture::new();
    let mut device = open_descriptor_file(&fixture, &blob());
    let mut out = [0u8; 32];
    assert_eq!(device.config_descriptor(1, &mut out), Ok(9));
    assert_eq!(out[..9], [9, 2, 9, 0, 0, 2, 0, 0x80, 25]);
    assert_eq!(device.config_descriptor(0, &mut out), Ok(12));
    assert_eq!(out[9..12], [7, 7, 7]);
}

#[test]
fn a_short_output_gets_the_start_of_the_configuration() {
    let fixture = Fixture::new();
    let mut device = open_descriptor_file(&fixture, &blob());
    let mut out = [0u8; 4];
    assert_eq!(device.config_descriptor(0, &mut out), Ok(4));
    assert_eq!(out, [9, 2, 12, 0]);
    assert_eq!(device.config_descriptor(0, &mut []), Ok(0));
}

#[test]
fn an_index_past_the_count_is_not_found() {
    let fixture = Fixture::new();
    let mut device = open_descriptor_file(&fixture, &blob());
    assert_eq!(
        device.config_descriptor(2, &mut [0u8; 8]),
        Err(ErrorKind::NotFound)
    );
}

#[test]
fn malformed_descriptors_are_refused() {
    type Change = fn(&mut Vec<u8>);
    let cases: [(&str, Change, u8); 8] = [
        ("device bLength", |b| b[0] = 9, 0),
        ("device type", |b| b[1] = 2, 0),
        ("short configuration bLength", |b| b[18] = 8, 0),
        ("wrong configuration type", |b| b[19] = 3, 0),
        (
            "wTotalLength under the header",
            |b| {
                b[20] = 8;
                b[21] = 0;
            },
            0,
        ),
        ("truncated before the header", |b| b.truncate(18), 0),
        ("truncated inside the header", |b| b.truncate(22), 0),
        // config 0's wTotalLength walks past the end of the file.
        (
            "a walk past the end",
            |b| {
                b[20] = 0xff;
                b[21] = 0xff;
            },
            1,
        ),
    ];
    for (name, change, index) in cases {
        let fixture = Fixture::new();
        let mut bytes = blob();
        change(&mut bytes);
        let mut device = open_descriptor_file(&fixture, &bytes);
        assert_eq!(
            device.config_descriptor(index, &mut [0u8; 64]),
            Err(ErrorKind::Configuration),
            "{name}"
        );
    }
}

#[test]
fn a_configuration_claiming_more_than_the_file_returns_what_is_there() {
    let fixture = Fixture::new();
    let mut bytes = blob();
    bytes[20] = 0xff;
    bytes[21] = 0xff;
    let mut device = open_descriptor_file(&fixture, &bytes);
    let mut out = vec![0u8; 0x1_0000];
    let copied = device.config_descriptor(0, &mut out).unwrap();
    assert_eq!(copied, bytes.len() - 18);
}

// Transfers, recorded by the fake.

#[test]
fn a_control_in_passes_its_setup_and_timeout() {
    let (mut device, fake) = device();
    fake.queue_reply(Reply {
        data: vec![1, 2],
        returned: None,
    });
    let mut data = [0u8; 4];
    assert_eq!(
        device.control_in(VENDOR_IN, &mut data, Duration::from_millis(250)),
        Ok(2)
    );
    assert_eq!(data[..2], [1, 2]);
    assert_eq!(
        fake.calls(),
        vec![Call::Control {
            request_type: 0xc0,
            request: 1,
            value: 2,
            index: 3,
            length: 4,
            timeout: 250,
            data: data.as_ptr() as usize,
        }]
    );
}

#[test]
fn a_control_out_in_the_wrong_direction_is_refused() {
    let (mut device, fake) = device();
    assert_eq!(
        device.control_out(VENDOR_IN, &[0u8; 2], TIMEOUT),
        Err(ErrorKind::InvalidInput)
    );
    assert_eq!(fake.calls(), vec![]);
}

#[test]
fn a_bulk_out_on_an_in_endpoint_is_refused() {
    let (mut device, fake) = device();
    assert_eq!(
        device.bulk_out(0x81, &[0u8; 2], TIMEOUT),
        Err(ErrorKind::InvalidInput)
    );
    assert_eq!(
        device.bulk_in(0x01, &mut [0u8; 2], TIMEOUT),
        Err(ErrorKind::InvalidInput)
    );
    assert_eq!(fake.calls(), vec![]);
}

#[test]
fn bulk_and_interrupt_carry_their_endpoints() {
    let (mut device, fake) = device();
    fake.queue_reply(Reply {
        data: vec![4, 4],
        returned: None,
    });
    fake.queue_reply(Reply {
        data: vec![5],
        returned: None,
    });
    let mut read = [0u8; 8];
    assert_eq!(device.bulk_in(0x82, &mut read, TIMEOUT), Ok(2));
    assert_eq!(read[..2], [4, 4]);
    assert_eq!(device.interrupt_in(0x83, &mut read, TIMEOUT), Ok(1));
    assert_eq!(read[0], 5);
    assert_eq!(device.bulk_out(0x01, &[1, 2, 3], TIMEOUT), Ok(3));
    let endpoints: Vec<_> = fake
        .calls()
        .iter()
        .map(|call| match call {
            Call::Bulk {
                endpoint, length, ..
            } => (*endpoint, *length),
            other => panic!("unexpected call {other:?}"),
        })
        .collect();
    assert_eq!(endpoints, vec![(0x82, 8), (0x83, 8), (0x01, 3)]);
}

#[test]
fn a_reported_count_past_the_buffer_is_a_failure() {
    let (mut device, fake) = device();
    fake.queue_reply(Reply {
        data: vec![0; 4],
        returned: Some(4096),
    });
    let mut data = [0u8; 4];
    assert_eq!(
        device.bulk_in(0x81, &mut data, TIMEOUT),
        Err(ErrorKind::Failed)
    );
    fake.queue_reply(Reply {
        data: vec![0; 4],
        returned: Some(5),
    });
    assert_eq!(
        device.control_in(VENDOR_IN, &mut data, TIMEOUT),
        Err(ErrorKind::Failed)
    );
}

#[test]
fn interface_calls_reach_the_kernel() {
    let (mut device, fake) = device();
    device.claim_interface(1).unwrap();
    device.set_alternate(1, 3).unwrap();
    device.release_interface(1).unwrap();
    assert_eq!(
        fake.calls(),
        vec![
            Call::Claim(1),
            Call::SetInterface {
                interface: 1,
                alternate: 3
            },
            Call::Release(1)
        ]
    );
}

#[test]
fn errnos_become_kinds() {
    let table = [
        (libc::ENODEV, ErrorKind::Unavailable),
        (libc::ESHUTDOWN, ErrorKind::Unavailable),
        (libc::ENXIO, ErrorKind::Unavailable),
        (libc::EHOSTUNREACH, ErrorKind::Unavailable),
        (libc::ETIMEDOUT, ErrorKind::Timeout),
        (libc::ETIME, ErrorKind::Timeout),
        (libc::EPIPE, ErrorKind::Failed),
        (libc::EBUSY, ErrorKind::Busy),
        (libc::EACCES, ErrorKind::PermissionDenied),
        (libc::EPERM, ErrorKind::PermissionDenied),
        (libc::ENOENT, ErrorKind::NotFound),
        (libc::EINVAL, ErrorKind::InvalidInput),
        (libc::EPROTO, ErrorKind::Failed),
        (libc::EOVERFLOW, ErrorKind::Overrun),
        (libc::ENOTTY, ErrorKind::Unsupported),
    ];
    for (errno, kind) in table {
        assert_eq!(kind_of_errno(errno), kind, "errno {errno}");
    }
    let (mut device, fake) = device();
    fake.fail_next(libc::EBUSY);
    assert_eq!(device.claim_interface(0), Err(ErrorKind::Busy));
    fake.fail_next(libc::ENODEV);
    assert_eq!(
        device.bulk_out(0x01, &[], TIMEOUT),
        Err(ErrorKind::Unavailable)
    );
}

#[test]
fn a_malformed_report_from_below_is_a_failure_not_a_bad_request() {
    let error = std::io::Error::from(std::io::ErrorKind::InvalidData);
    assert_eq!(kind_of(&error), ErrorKind::Failed);
    let error = std::io::Error::from(std::io::ErrorKind::InvalidInput);
    assert_eq!(kind_of(&error), ErrorKind::InvalidInput);
}

#[test]
fn a_timeout_is_never_the_kernels_forever() {
    assert_eq!(timeout_ms(Duration::ZERO), 1);
    assert_eq!(timeout_ms(Duration::from_nanos(1)), 1);
    assert_eq!(timeout_ms(Duration::from_micros(1500)), 2);
    assert_eq!(timeout_ms(Duration::from_millis(40)), 40);
    assert_eq!(timeout_ms(Duration::MAX), u32::MAX);
    let (mut device, fake) = device();
    device.bulk_out(0x01, &[], Duration::from_nanos(1)).unwrap();
    device.bulk_out(0x01, &[], Duration::ZERO).unwrap();
    for call in fake.calls() {
        assert!(matches!(call, Call::Bulk { timeout: 1, .. }), "{call:?}");
    }
}

/// `capacity`s and the crafted reports for an iso transfer of 12 bytes in 3 packets.
fn iso_packets(capacities: [u32; 3]) -> [IsoPacket; 3] {
    capacities.map(|capacity| IsoPacket {
        capacity,
        received: 99,
        ok: true,
    })
}

fn good_urb() -> CraftedUrb {
    CraftedUrb {
        packets: vec![(0, 2), (0, 5), (0, 3)],
        payload: (0..12).collect(),
        ..CraftedUrb::default()
    }
}

#[test]
fn an_isochronous_transfer_lands_back_to_back_by_capacity() {
    let (mut device, fake) = device();
    fake.queue_urb(CraftedUrb {
        packets: vec![(0, 2), (-libc::EPROTO, 5), (0, 3)],
        payload: (0..12).collect(),
        ..CraftedUrb::default()
    });
    let mut packets = iso_packets([4, 5, 3]);
    let mut data = [0xeeu8; 16];
    device
        .iso_in(0x81, &mut data, &mut packets, TIMEOUT)
        .unwrap();

    // Packet 1 starts at the first packet's capacity, not at the bytes that arrived.
    assert_eq!(data[..2], [0, 1]);
    assert_eq!(data[2..4], [0xee, 0xee], "the gap is left alone");
    assert_eq!(data[4..9], [4, 5, 6, 7, 8]);
    assert_eq!(data[9..12], [9, 10, 11]);
    assert_eq!(data[12..], [0xee; 4], "nothing past the capacities");
    assert_eq!(
        packets,
        [
            IsoPacket {
                capacity: 4,
                received: 2,
                ok: true
            },
            IsoPacket {
                capacity: 5,
                received: 5,
                ok: false
            },
            IsoPacket {
                capacity: 3,
                received: 3,
                ok: true
            },
        ]
    );
    let submits: Vec<_> = fake
        .calls()
        .into_iter()
        .filter_map(|call| match call {
            Call::Submit {
                endpoint,
                buffer_length,
                lengths,
                ..
            } => Some((endpoint, buffer_length, lengths)),
            _ => None,
        })
        .collect();
    assert_eq!(submits, vec![(0x81, 12, vec![4, 5, 3])]);
}

#[test]
fn crafted_lengths_past_a_buffer_are_failures_not_panics() {
    let bad = [
        CraftedUrb {
            actual_length: Some(13),
            ..good_urb()
        },
        CraftedUrb {
            actual_length: Some(-1),
            ..good_urb()
        },
        CraftedUrb {
            actual_length: Some(i32::MIN),
            ..good_urb()
        },
        CraftedUrb {
            packets: vec![(0, 5), (0, 5), (0, 3)],
            ..good_urb()
        },
        CraftedUrb {
            packets: vec![(0, u32::MAX), (0, 0), (0, 0)],
            ..good_urb()
        },
        CraftedUrb {
            number_of_packets: Some(200),
            ..good_urb()
        },
        CraftedUrb {
            number_of_packets: Some(-3),
            ..good_urb()
        },
        CraftedUrb {
            buffer_length: Some(4096),
            ..good_urb()
        },
        CraftedUrb {
            buffer: Some(0x1000),
            ..good_urb()
        },
    ];
    for crafted in bad {
        let (mut device, fake) = device();
        fake.queue_urb(crafted.clone());
        let mut packets = iso_packets([4, 5, 3]);
        let mut data = [0xeeu8; 12];
        assert_eq!(
            device.iso_in(0x81, &mut data, &mut packets, TIMEOUT),
            Err(ErrorKind::Failed),
            "{crafted:?}"
        );
        assert_eq!(data, [0xee; 12], "nothing delivered: {crafted:?}");
        assert!(
            packets.iter().all(|p| p.received == 0 && !p.ok),
            "{crafted:?}"
        );

        // The URB came back, so the next transfer runs as normal.
        fake.queue_urb(good_urb());
        device
            .iso_in(0x81, &mut data, &mut packets, TIMEOUT)
            .unwrap();
        assert_eq!(packets[1].received, 5, "{crafted:?}");
    }
}

#[test]
fn a_foreign_urb_is_a_failure() {
    let (mut device, fake) = device();
    fake.queue_urb(CraftedUrb {
        foreign: Some(0x10),
        ..good_urb()
    });
    let mut packets = iso_packets([4, 5, 3]);
    let mut data = [0u8; 12];
    assert_eq!(
        device.iso_in(0x81, &mut data, &mut packets, TIMEOUT),
        Err(ErrorKind::Failed)
    );
    assert_eq!(fake.in_flight(), 1, "the real URB is still held");
}

#[test]
fn a_status_of_i32_min_does_not_overflow() {
    let (mut device, fake) = device();
    fake.queue_urb(CraftedUrb {
        status: i32::MIN,
        ..good_urb()
    });
    let mut packets = iso_packets([4, 5, 3]);
    let mut data = [0u8; 12];
    assert_eq!(
        device.iso_in(0x81, &mut data, &mut packets, TIMEOUT),
        Ok(())
    );
}

#[test]
fn urb_status_decides_whether_the_whole_urb_failed() {
    let table = [
        (-libc::ENODEV, Err(ErrorKind::Unavailable)),
        (-libc::ESHUTDOWN, Err(ErrorKind::Unavailable)),
        (-libc::ENOENT, Err(ErrorKind::Failed)),
        (-libc::ECONNRESET, Err(ErrorKind::Failed)),
        (-libc::EXDEV, Ok(())),
        (0, Ok(())),
    ];
    for (status, expected) in table {
        let (mut device, fake) = device();
        fake.queue_urb(CraftedUrb {
            status,
            ..good_urb()
        });
        let mut packets = iso_packets([4, 5, 3]);
        let mut data = [0u8; 12];
        assert_eq!(
            device.iso_in(0x81, &mut data, &mut packets, TIMEOUT),
            expected,
            "status {status}"
        );
    }
}

#[test]
fn bad_isochronous_requests_never_reach_the_kernel() {
    let (mut device, fake) = device();
    let cases: Vec<(Vec<IsoPacket>, usize, u8)> = vec![
        (vec![], 8, 0x81),
        (vec![IsoPacket::default(); 129], 8, 0x81),
        (iso_packets([4, 5, 3]).to_vec(), 11, 0x81),
        (iso_packets([0, 0, 0]).to_vec(), 8, 0x81),
        (iso_packets([u32::MAX, 1, 0]).to_vec(), 8, 0x81),
        (iso_packets([4, 5, 3]).to_vec(), 12, 0x01),
        (iso_packets([4, 5, 3]).to_vec(), 12, 0x80),
    ];
    for (mut packets, len, endpoint) in cases {
        let mut data = vec![0u8; len];
        assert_eq!(
            device.iso_in(endpoint, &mut data, &mut packets, TIMEOUT),
            Err(ErrorKind::InvalidInput),
            "{packets:?}"
        );
    }
    assert!(
        fake.calls()
            .iter()
            .all(|call| !matches!(call, Call::Submit { .. })),
    );
}

#[test]
fn an_isochronous_transfer_with_no_completion_times_out_and_cancels() {
    let (mut device, fake) = device();
    let mut packets = iso_packets([4, 5, 3]);
    let mut data = [0xeeu8; 12];
    assert_eq!(
        device.iso_in(0x81, &mut data, &mut packets, Duration::from_millis(5)),
        Err(ErrorKind::Timeout)
    );
    assert_eq!(fake.in_flight(), 0, "the discard brought it back");
    assert_eq!(data, [0xee; 12]);
    assert!(
        fake.calls()
            .iter()
            .any(|call| matches!(call, Call::Discard(_)))
    );

    fake.queue_urb(good_urb());
    device
        .iso_in(0x81, &mut data, &mut packets, TIMEOUT)
        .unwrap();
    assert_eq!(packets[1].received, 5);
}

#[test]
fn a_urb_the_kernel_keeps_is_busy_until_it_returns_and_never_stuck_for_good() {
    let (mut device, fake) = device();
    fake.set_stuck(true);
    let mut packets = iso_packets([4, 5, 3]);
    let mut data = [0xeeu8; 12];
    assert_eq!(
        device.iso_in(0x81, &mut data, &mut packets, Duration::from_millis(1)),
        Err(ErrorKind::Timeout)
    );
    assert_eq!(fake.in_flight(), 1);
    for _ in 0..3 {
        assert_eq!(
            device.iso_in(0x81, &mut data, &mut packets, Duration::from_millis(1)),
            Err(ErrorKind::Busy)
        );
    }
    // Once the kernel lets go, the next call's cancel reaps it and the transfer runs.
    fake.set_stuck(false);
    fake.set_steady_urb(good_urb());
    device
        .iso_in(0x81, &mut data, &mut packets, TIMEOUT)
        .unwrap();
    assert_eq!(packets[0].received, 2);
}

#[test]
fn a_device_that_leaves_mid_transfer_is_unavailable() {
    let (mut device, fake) = device();
    fake.set_gone(true);
    let mut packets = iso_packets([4, 5, 3]);
    let mut data = [0u8; 12];
    assert_eq!(
        device.iso_in(0x81, &mut data, &mut packets, Duration::from_secs(30)),
        Err(ErrorKind::Unavailable),
        "ENODEV from the reap"
    );
}

/// The URB can't be reaped once the device has left, and the next call says the device left
/// rather than that something is busy.
#[test]
fn after_a_device_leaves_the_next_call_is_unavailable_too() {
    let (mut device, fake) = device();
    fake.set_stuck(true);
    fake.set_gone(true);
    let mut packets = iso_packets([4, 5, 3]);
    let mut data = [0u8; 12];
    assert_eq!(
        device.iso_in(0x81, &mut data, &mut packets, TIMEOUT),
        Err(ErrorKind::Unavailable)
    );
    assert_eq!(fake.in_flight(), 1);
    assert_eq!(
        device.iso_in(0x81, &mut data, &mut packets, TIMEOUT),
        Err(ErrorKind::Unavailable)
    );
}

#[test]
fn a_timeout_of_any_size_does_not_panic() {
    let (mut device, fake) = device();
    fake.set_steady_urb(good_urb());
    let mut packets = iso_packets([4, 5, 3]);
    let mut data = [0u8; 12];
    for timeout in [Duration::ZERO, Duration::from_nanos(1), Duration::MAX] {
        device
            .iso_in(0x81, &mut data, &mut packets, timeout)
            .unwrap();
    }
}

#[test]
fn a_timeout_waits_about_that_long() {
    let (mut device, _fake) = device();
    let mut packets = iso_packets([4, 5, 3]);
    let mut data = [0u8; 12];
    let start = Instant::now();
    assert_eq!(
        device.iso_in(0x81, &mut data, &mut packets, Duration::from_millis(30)),
        Err(ErrorKind::Timeout)
    );
    let elapsed = start.elapsed();
    assert!(elapsed >= Duration::from_millis(30), "{elapsed:?}");
    assert!(elapsed < Duration::from_secs(5), "{elapsed:?}");
}

#[test]
fn the_ring_is_kept_while_transfers_fit_and_grows_when_they_do_not() {
    let (mut device, fake) = device();
    let buffer_of = |fake: &FakeUsbfs, index: usize| match &fake.calls()[index] {
        Call::Submit { urb, .. } => *urb,
        other => panic!("{other:?}"),
    };
    fake.set_steady_urb(good_urb());
    let mut packets = iso_packets([4, 5, 3]);
    let mut data = vec![0u8; 64];
    device
        .iso_in(0x81, &mut data, &mut packets, TIMEOUT)
        .unwrap();
    device
        .iso_in(0x82, &mut data, &mut packets, TIMEOUT)
        .unwrap();
    assert_eq!(buffer_of(&fake, 0), buffer_of(&fake, 1), "the same URB");

    // A larger transfer makes a larger ring.
    let mut large = iso_packets([20, 20, 20]);
    device.iso_in(0x81, &mut data, &mut large, TIMEOUT).unwrap();
    assert_eq!(large[0].received, 2);
}

#[test]
fn a_steady_stream_of_every_transfer_allocates_nothing() {
    let (mut device, fake) = device();
    fake.set_steady_reply(Reply {
        data: vec![1, 2, 3, 4],
        returned: None,
    });
    fake.set_steady_urb(good_urb());
    fake.stop_recording();

    let mut data = [0u8; 12];
    let mut packets = iso_packets([4, 5, 3]);
    // The first isochronous call makes the ring; the steady state starts after it.
    device
        .iso_in(0x81, &mut data, &mut packets, TIMEOUT)
        .unwrap();

    let mut moved = 0;
    let start = ALLOCATIONS.with(Cell::get);
    for _ in 0..1000 {
        moved += device.control_in(VENDOR_IN, &mut data, TIMEOUT).unwrap();
        moved += device.bulk_in(0x81, &mut data, TIMEOUT).unwrap();
        moved += device.interrupt_in(0x82, &mut data, TIMEOUT).unwrap();
        moved += device.bulk_out(0x01, &data, TIMEOUT).unwrap();
        device
            .iso_in(0x81, &mut data, &mut packets, TIMEOUT)
            .unwrap();
        moved += packets.iter().map(|p| p.received as usize).sum::<usize>();
    }
    assert_eq!(ALLOCATIONS.with(Cell::get) - start, 0);
    assert_eq!(moved, 1000 * (4 + 4 + 4 + 12 + 10));
}

#[test]
fn the_fixtures_stay_out_of_dev() {
    let root = Fixture::new();
    assert!(
        !root.dev.starts_with(Path::new("/dev")),
        "tests must not touch device nodes: {}",
        root.dev.display()
    );
}
