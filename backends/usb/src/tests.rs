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
fn a_kernel_driver_is_detached_once_and_the_claim_then_succeeds() {
    let (mut device, fake) = device();
    fake.bind_driver(1, "uvcvideo");
    assert_eq!(device.claim_interface(1), Err(ErrorKind::Busy));
    assert_eq!(device.detach_kernel_driver(1), Ok(()));
    assert_eq!(fake.driver(1), None);
    assert_eq!(device.claim_interface(1), Ok(()));
    // Nothing bound any more: still fine, and nothing is disconnected.
    assert_eq!(device.detach_kernel_driver(1), Ok(()));
    assert_eq!(device.detach_kernel_driver(7), Ok(()));
    let disconnects = fake
        .calls()
        .iter()
        .filter(|call| matches!(call, Call::InterfaceIoctl { .. }))
        .count();
    assert_eq!(disconnects, 1);
}

#[test]
fn detaching_never_takes_a_usbfs_claim_away() {
    let (mut device, fake) = device();
    fake.bind_driver(0, "usbfs");
    assert_eq!(device.detach_kernel_driver(0), Ok(()));
    assert_eq!(fake.driver(0).as_deref(), Some("usbfs"));
}

#[test]
fn releasing_and_dropping_do_not_put_the_kernel_driver_back() {
    let (mut device, fake) = device();
    fake.bind_driver(2, "cdc_acm");
    device.detach_kernel_driver(2).unwrap();
    device.claim_interface(2).unwrap();
    device.release_interface(2).unwrap();
    drop(device);
    assert_eq!(fake.driver(2), None);
    assert_eq!(
        fake.calls(),
        vec![
            Call::GetDriver(2),
            Call::InterfaceIoctl {
                interface: 2,
                code: 0x5516,
                data: 0
            },
            Call::Claim(2),
            Call::Release(2),
        ],
        "no connect request, on release or on drop"
    );
}

#[test]
fn a_detach_the_kernel_refuses_says_why() {
    let (mut device, fake) = device();
    fake.fail_next(libc::EACCES);
    assert_eq!(
        device.detach_kernel_driver(0),
        Err(ErrorKind::PermissionDenied)
    );
    fake.fail_next(libc::ENODEV);
    assert_eq!(device.detach_kernel_driver(0), Err(ErrorKind::Unavailable));
}

#[test]
fn a_reset_that_keeps_the_device_leaves_it_usable() {
    let (mut device, fake) = device();
    fake.set_descriptors(&blob());
    device.claim_interface(0).unwrap();
    assert_eq!(device.reset(), Ok(()));
    let mut out = [0u8; 12];
    assert_eq!(device.config_descriptor(0, &mut out), Ok(12));
    assert_eq!(fake.calls(), vec![Call::Claim(0), Call::Reset]);
}

#[test]
fn a_reset_that_was_not_carried_out_can_be_made_again() {
    let (mut device, fake) = device();
    fake.fail_next(libc::EBUSY);
    assert_eq!(device.reset(), Err(ErrorKind::Busy));
    fake.fail_next(libc::EACCES);
    assert_eq!(device.reset(), Err(ErrorKind::PermissionDenied));
    assert_eq!(device.reset(), Ok(()));
    assert_eq!(device.claim_interface(0), Ok(()));
}

#[test]
fn a_reset_that_re_enumerates_is_unavailable_and_so_is_every_call_after_it() {
    let (mut device, fake) = device();
    fake.set_descriptors(&blob());
    fake.set_steady_reply(Reply {
        data: vec![1, 2, 3, 4],
        returned: None,
    });
    fake.set_reset_reenumerates(true);
    let gone = Err(ErrorKind::Unavailable);
    assert_eq!(device.reset(), Err(ErrorKind::Unavailable));

    let mut data = [0u8; 12];
    assert_eq!(device.reset(), Err(ErrorKind::Unavailable));
    assert_eq!(device.config_descriptor(0, &mut data), gone);
    assert_eq!(device.detach_kernel_driver(0), Err(ErrorKind::Unavailable));
    assert_eq!(device.claim_interface(0), Err(ErrorKind::Unavailable));
    assert_eq!(device.release_interface(0), Err(ErrorKind::Unavailable));
    assert_eq!(device.set_alternate(0, 1), Err(ErrorKind::Unavailable));
    assert_eq!(device.control_in(VENDOR_IN, &mut data, TIMEOUT), gone);
    assert_eq!(device.bulk_in(0x81, &mut data, TIMEOUT), gone);
    assert_eq!(device.bulk_out(0x01, &data, TIMEOUT), gone);
    assert_eq!(device.interrupt_in(0x82, &mut data, TIMEOUT), gone);
    assert_eq!(
        device.iso_in_start(0x81, 3, 3, 5),
        Err(ErrorKind::Unavailable)
    );
}

/// On a regular file the kernel refuses both requests, which is a real ioctl path that can't
/// touch a USB device.
#[test]
fn a_node_that_is_not_a_usb_device_refuses_reset_and_detach() {
    let fixture = Fixture::new();
    let mut device = open_descriptor_file(&fixture, &blob());
    assert_eq!(device.reset(), Err(ErrorKind::Unsupported));
    assert_eq!(device.detach_kernel_driver(0), Err(ErrorKind::Unsupported));
}

#[test]
fn resets_and_detaches_allocate_nothing() {
    let (mut device, fake) = device();
    fake.stop_recording();
    let start = ALLOCATIONS.with(Cell::get);
    for _ in 0..1000 {
        device.detach_kernel_driver(0).unwrap();
        device.reset().unwrap();
    }
    assert_eq!(ALLOCATIONS.with(Cell::get) - start, 0);
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

/// The stream every isochronous test starts: three transfers of three packets of 5 bytes.
const TRANSFERS: u8 = 3;
const PACKETS: u16 = 3;
const PACKET_BYTES: u32 = 5;

fn started() -> (UsbfsDevice, FakeUsbfs) {
    let (mut device, fake) = device();
    device
        .iso_in_start(0x81, TRANSFERS, PACKETS, PACKET_BYTES)
        .unwrap();
    (device, fake)
}

/// What the caller's packet records hold before a transfer: none of it should survive.
fn iso_packets() -> [IsoPacket; 3] {
    [IsoPacket {
        capacity: 99,
        received: 99,
        ok: true,
    }; 3]
}

/// A completion of 2, 5 and 3 bytes, each packet's at its place in the 15-byte buffer.
fn good_urb() -> CraftedUrb {
    CraftedUrb {
        packets: vec![(0, 2), (0, 5), (0, 3)],
        payload: (0..15).collect(),
        ..CraftedUrb::default()
    }
}

/// Each submit's URB, endpoint, buffer length and packet lengths, in order.
fn submits(fake: &FakeUsbfs) -> Vec<(usize, u8, i32, Vec<u32>)> {
    fake.calls()
        .into_iter()
        .filter_map(|call| match call {
            Call::Submit {
                urb,
                endpoint,
                buffer_length,
                lengths,
                ..
            } => Some((urb, endpoint, buffer_length, lengths)),
            _ => None,
        })
        .collect()
}

#[test]
fn a_stream_queues_every_transfer_at_once_and_each_again_as_it_is_taken() {
    let (mut device, fake) = started();
    let queued = submits(&fake);
    assert_eq!(queued.len(), 3);
    for (_, endpoint, length, lengths) in &queued {
        assert_eq!(
            (*endpoint, *length, lengths.as_slice()),
            (0x81, 15, &[5, 5, 5][..])
        );
    }
    assert_ne!(queued[0].0, queued[1].0, "three URBs of their own");
    assert_eq!(fake.in_flight(), 3);

    fake.queue_urb(CraftedUrb {
        packets: vec![(0, 2), (-libc::EPROTO, 5), (0, 3)],
        ..good_urb()
    });
    let mut packets = iso_packets();
    let mut data = [0xeeu8; 18];
    device
        .iso_in_next(&mut data, &mut packets, TIMEOUT)
        .unwrap();
    // Each packet at its place: a short one leaves the rest of its room alone.
    assert_eq!(data[..2], [0, 1]);
    assert_eq!(data[2..5], [0xee; 3]);
    assert_eq!(data[5..10], [5, 6, 7, 8, 9]);
    assert_eq!(data[10..13], [10, 11, 12]);
    assert_eq!(data[13..], [0xee; 5], "nothing past the transfer");
    let record = |received, ok| IsoPacket {
        capacity: PACKET_BYTES,
        received,
        ok,
    };
    assert_eq!(
        packets,
        [record(2, true), record(5, false), record(3, true)]
    );

    // The one handed over went straight back, so three are queued again.
    assert_eq!(fake.in_flight(), 3);
    let queued = submits(&fake);
    assert_eq!(queued.len(), 4);
    assert_eq!(queued[3].0, queued[0].0, "the oldest, again");

    // The others come in their turn.
    fake.set_steady_urb(good_urb());
    for _ in 0..5 {
        device
            .iso_in_next(&mut data, &mut packets, TIMEOUT)
            .unwrap();
        assert_eq!(fake.in_flight(), 3);
    }
    let order: Vec<usize> = submits(&fake).iter().map(|s| s.0).collect();
    assert_eq!(
        order[3..],
        [order[0], order[1], order[2], order[0], order[1], order[2]]
    );
}

#[test]
fn crafted_lengths_past_a_buffer_are_failures_not_panics() {
    let bad = [
        CraftedUrb {
            actual_length: Some(16),
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
            packets: vec![(0, 6), (0, 5), (0, 3)],
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
        let (mut device, fake) = started();
        fake.queue_urb(crafted.clone());
        let mut packets = iso_packets();
        let mut data = [0xeeu8; 15];
        assert_eq!(
            device.iso_in_next(&mut data, &mut packets, TIMEOUT),
            Err(ErrorKind::Failed),
            "{crafted:?}"
        );
        assert_eq!(data, [0xee; 15], "nothing delivered: {crafted:?}");
        assert!(
            packets.iter().all(|p| p.received == 0 && !p.ok),
            "{crafted:?}"
        );

        // The URB came back and went out again, so the stream runs on.
        assert_eq!(fake.in_flight(), 3, "{crafted:?}");
        fake.queue_urb(good_urb());
        device
            .iso_in_next(&mut data, &mut packets, TIMEOUT)
            .unwrap();
        assert_eq!(packets[1].received, 5, "{crafted:?}");
    }
}

#[test]
fn a_foreign_urb_is_a_failure() {
    let (mut device, fake) = started();
    fake.queue_urb(CraftedUrb {
        foreign: Some(0x10),
        ..good_urb()
    });
    let mut packets = iso_packets();
    let mut data = [0u8; 15];
    assert_eq!(
        device.iso_in_next(&mut data, &mut packets, TIMEOUT),
        Err(ErrorKind::Failed)
    );
    assert_eq!(fake.in_flight(), 3, "the real URBs are still held");
}

#[test]
fn a_status_of_i32_min_does_not_overflow() {
    let (mut device, fake) = started();
    fake.queue_urb(CraftedUrb {
        status: i32::MIN,
        ..good_urb()
    });
    let mut packets = iso_packets();
    let mut data = [0u8; 15];
    assert_eq!(device.iso_in_next(&mut data, &mut packets, TIMEOUT), Ok(()));
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
        let (mut device, fake) = started();
        fake.queue_urb(CraftedUrb {
            status,
            ..good_urb()
        });
        let mut packets = iso_packets();
        let mut data = [0u8; 15];
        assert_eq!(
            device.iso_in_next(&mut data, &mut packets, TIMEOUT),
            expected,
            "status {status}"
        );
        if expected.is_err() {
            assert!(packets.iter().all(|p| p.received == 0 && !p.ok));
        }
    }
}

#[test]
fn bad_isochronous_requests_never_reach_the_kernel() {
    let (mut device, fake) = device();
    let shapes = [
        (0x81, 0, 3, 5),
        (0x81, 17, 3, 5),
        (0x81, 3, 0, 5),
        (0x81, 3, 65, 5),
        (0x81, 3, 3, 0),
        (0x81, 3, 3, 3073),
        (0x01, 3, 3, 5),
    ];
    for (endpoint, transfers, packets, bytes) in shapes {
        assert_eq!(
            device.iso_in_start(endpoint, transfers, packets, bytes),
            Err(ErrorKind::InvalidInput),
            "{endpoint:#x} {transfers} {packets} {bytes}"
        );
    }
    let mut packets = iso_packets();
    let mut data = [0u8; 15];
    assert_eq!(
        device.iso_in_next(&mut data, &mut packets, TIMEOUT),
        Err(ErrorKind::InvalidInput),
        "no stream"
    );
    assert!(submits(&fake).is_empty());

    device.iso_in_start(0x81, 3, 3, 5).unwrap();
    assert_eq!(device.iso_in_start(0x81, 3, 3, 5), Err(ErrorKind::Busy));
    fake.set_steady_urb(good_urb());
    assert_eq!(
        device.iso_in_next(&mut data[..14], &mut packets, TIMEOUT),
        Err(ErrorKind::InvalidInput)
    );
    assert_eq!(
        device.iso_in_next(&mut data, &mut packets[..2], TIMEOUT),
        Err(ErrorKind::InvalidInput)
    );
    assert_eq!(submits(&fake).len(), 3, "and the stream as it was");
    assert_eq!(fake.in_flight(), 3);
}

#[test]
fn a_transfer_not_done_in_time_is_a_timeout_and_the_stream_goes_on() {
    let (mut device, fake) = started();
    let mut packets = iso_packets();
    let mut data = [0xeeu8; 15];
    assert_eq!(
        device.iso_in_next(&mut data, &mut packets, Duration::from_millis(5)),
        Err(ErrorKind::Timeout)
    );
    assert_eq!(fake.in_flight(), 3, "nothing taken back");
    assert_eq!(data, [0xee; 15]);
    assert!(
        !fake
            .calls()
            .iter()
            .any(|call| matches!(call, Call::Discard(_)))
    );
    fake.queue_urb(good_urb());
    device
        .iso_in_next(&mut data, &mut packets, TIMEOUT)
        .unwrap();
    assert_eq!(packets[1].received, 5);
}

#[test]
fn stopping_takes_every_transfer_back_and_a_stream_can_start_again() {
    let (mut device, fake) = started();
    device.iso_in_stop().unwrap();
    assert_eq!(fake.in_flight(), 0);
    let discards = fake
        .calls()
        .iter()
        .filter(|call| matches!(call, Call::Discard(_)))
        .count();
    assert_eq!(discards, 3);
    device.iso_in_stop().unwrap();
    let mut packets = iso_packets();
    let mut data = [0u8; 15];
    assert_eq!(
        device.iso_in_next(&mut data, &mut packets, TIMEOUT),
        Err(ErrorKind::InvalidInput)
    );
    device.iso_in_start(0x82, 2, 3, 5).unwrap();
    assert_eq!(fake.in_flight(), 2);
    // Dropping the device takes the stream back too.
    drop(device);
    assert_eq!(fake.in_flight(), 0);
}

#[test]
fn a_device_that_leaves_mid_stream_is_unavailable() {
    let (mut device, fake) = started();
    fake.set_gone(true);
    let mut packets = iso_packets();
    let mut data = [0u8; 15];
    assert_eq!(
        device.iso_in_next(&mut data, &mut packets, Duration::from_secs(30)),
        Err(ErrorKind::Unavailable),
        "ENODEV from the reap"
    );
}

#[test]
fn a_timeout_of_any_size_does_not_panic() {
    let (mut device, fake) = started();
    fake.set_steady_urb(good_urb());
    let mut packets = iso_packets();
    let mut data = [0u8; 15];
    for timeout in [Duration::ZERO, Duration::from_nanos(1), Duration::MAX] {
        device
            .iso_in_next(&mut data, &mut packets, timeout)
            .unwrap();
    }
}

#[test]
fn a_timeout_waits_about_that_long() {
    let (mut device, _fake) = started();
    let mut packets = iso_packets();
    let mut data = [0u8; 15];
    let start = Instant::now();
    assert_eq!(
        device.iso_in_next(&mut data, &mut packets, Duration::from_millis(30)),
        Err(ErrorKind::Timeout)
    );
    let elapsed = start.elapsed();
    assert!(elapsed >= Duration::from_millis(30), "{elapsed:?}");
    assert!(elapsed < Duration::from_secs(5), "{elapsed:?}");
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

    let mut data = [0u8; 15];
    let mut packets = iso_packets();
    // Starting the stream makes the ring; the steady state starts after it.
    device
        .iso_in_start(0x81, TRANSFERS, PACKETS, PACKET_BYTES)
        .unwrap();

    let mut moved = 0;
    let start = ALLOCATIONS.with(Cell::get);
    for _ in 0..1000 {
        moved += device
            .control_in(VENDOR_IN, &mut data[..12], TIMEOUT)
            .unwrap();
        moved += device.bulk_in(0x81, &mut data[..12], TIMEOUT).unwrap();
        moved += device.interrupt_in(0x82, &mut data[..12], TIMEOUT).unwrap();
        moved += device.bulk_out(0x01, &data[..12], TIMEOUT).unwrap();
        device
            .iso_in_next(&mut data, &mut packets, TIMEOUT)
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
