//! The calls as the kernel sees them. No test opens a USB device: the fake answers the ioctls, and
//! the descriptor read runs on a temporary file.

use super::fake::{Call, FakeUsbfs, Reply};
use super::*;
use std::os::unix::ffi::OsStrExt;

const VENDOR_IN: ControlSetup = ControlSetup {
    request_type: 0xc0,
    request: 7,
    value: 0x1234,
    index: 0x5678,
};

const VENDOR_OUT: ControlSetup = ControlSetup {
    request_type: 0x40,
    request: 9,
    value: 1,
    index: 2,
};

fn file() -> (UsbfsFile, FakeUsbfs) {
    let fake = FakeUsbfs::new();
    (UsbfsFile::fake(fake.clone()), fake)
}

fn invalid_input<T: std::fmt::Debug>(result: io::Result<T>) {
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidInput);
}

fn invalid_data<T: std::fmt::Debug>(result: io::Result<T>) {
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
}

#[test]
fn request_numbers_match_the_kernel() {
    assert_eq!(CONTROL, 0xc018_5500);
    assert_eq!(BULK, 0xc018_5502);
    assert_eq!(SUBMITURB, 0x8038_550a);
    assert_eq!(DISCARDURB, 0x0000_550b);
    assert_eq!(REAPURBNDELAY, 0x4008_550d);
    assert_eq!(CLAIMINTERFACE, 0x8004_550f);
    assert_eq!(RELEASEINTERFACE, 0x8004_5510);
    assert_eq!(SETINTERFACE, 0x8008_5504);
    assert_eq!(GETDRIVER, 0x4104_5508);
    assert_eq!(RESET, 0x0000_5514);
    assert_eq!(DISCONNECT, 0x0000_5516);
}

#[cfg(target_pointer_width = "64")]
#[test]
fn the_passed_on_request_number_matches_the_kernel_on_64_bit() {
    assert_eq!(IOCTL, 0xc010_5512);
}

#[cfg(target_pointer_width = "64")]
#[test]
fn structures_match_the_kernel_layout_on_64_bit() {
    assert_eq!(size_of::<CtrlTransfer>(), 24);
    assert_eq!(size_of::<BulkTransfer>(), 24);
    assert_eq!(size_of::<SetInterface>(), 8);
    assert_eq!(size_of::<IsoPacketDesc>(), 12);
    assert_eq!(size_of::<UrbHeader>(), 56);
    assert_eq!(size_of::<InterfaceIoctl>(), 16);
    assert_eq!(std::mem::offset_of!(InterfaceIoctl, ioctl_code), 4);
    assert_eq!(std::mem::offset_of!(InterfaceIoctl, data), 8);
}

#[test]
fn the_driver_query_matches_the_kernel_layout() {
    assert_eq!(DRIVER_NAME_BYTES, 255 + 1);
    assert_eq!(size_of::<GetDriver>(), 260);
    assert_eq!(std::mem::offset_of!(GetDriver, driver), 4);
}

#[test]
fn detaching_asks_who_holds_the_interface_and_disconnects_a_kernel_driver() {
    let (file, fake) = file();
    fake.bind_driver(2, "uvcvideo");
    assert_eq!(
        file.claim_interface(2).unwrap_err().raw_os_error(),
        Some(libc::EBUSY)
    );
    assert!(file.detach_kernel_driver(2).unwrap());
    assert_eq!(fake.driver(2), None);
    file.claim_interface(2).unwrap();
    assert_eq!(
        fake.calls(),
        vec![
            Call::Claim(2),
            Call::GetDriver(2),
            Call::InterfaceIoctl {
                interface: 2,
                code: 0x5516,
                data: 0,
            },
            Call::Claim(2),
        ]
    );
}

#[test]
fn detaching_with_nothing_bound_sends_no_disconnect() {
    let (file, fake) = file();
    assert!(!file.detach_kernel_driver(1).unwrap());
    assert!(!file.detach_kernel_driver(1).unwrap());
    assert_eq!(fake.calls(), vec![Call::GetDriver(1), Call::GetDriver(1)]);
}

#[test]
fn detaching_leaves_a_claim_made_through_usbdevfs_alone() {
    let (file, fake) = file();
    fake.bind_driver(0, "usbfs");
    assert!(!file.detach_kernel_driver(0).unwrap());
    assert_eq!(fake.driver(0).as_deref(), Some("usbfs"));
    assert_eq!(fake.calls(), vec![Call::GetDriver(0)]);
}

#[test]
fn a_driver_name_is_read_up_to_its_terminator_or_its_end() {
    let mut driver = [0u8; DRIVER_NAME_BYTES];
    assert_eq!(driver_name(&driver), b"");
    driver[..5].copy_from_slice(b"usbfs");
    assert_eq!(driver_name(&driver), b"usbfs");
    driver[5] = b'2';
    assert_eq!(driver_name(&driver), b"usbfs2");
    // A name the kernel didn't terminate is all of the field, never past it.
    assert_eq!(
        driver_name(&[b'a'; DRIVER_NAME_BYTES]).len(),
        DRIVER_NAME_BYTES
    );
}

#[test]
fn a_failed_detach_comes_back_with_its_errno_and_detaches_nothing() {
    let (file, fake) = file();
    fake.bind_driver(0, "cdc_acm");
    fake.fail_next(libc::EACCES);
    assert_eq!(
        file.detach_kernel_driver(0).unwrap_err().raw_os_error(),
        Some(libc::EACCES)
    );
    assert_eq!(fake.driver(0).as_deref(), Some("cdc_acm"));
}

#[test]
fn a_reset_that_keeps_the_device_is_one_request_and_the_file_stays_good() {
    let (file, fake) = file();
    file.reset().unwrap();
    file.claim_interface(0).unwrap();
    assert_eq!(fake.calls(), vec![Call::Reset, Call::Claim(0)]);
    fake.fail_next(libc::EBUSY);
    assert_eq!(file.reset().unwrap_err().raw_os_error(), Some(libc::EBUSY));
    file.reset().unwrap();
}

#[test]
fn a_reset_that_re_enumerates_is_enodev_and_so_is_everything_after_it() {
    let (file, fake) = file();
    fake.set_reset_reenumerates(true);
    let gone = Some(libc::ENODEV);
    assert_eq!(file.reset().unwrap_err().raw_os_error(), gone);
    assert_eq!(file.reset().unwrap_err().raw_os_error(), gone);
    assert_eq!(file.claim_interface(0).unwrap_err().raw_os_error(), gone);
    assert_eq!(
        file.detach_kernel_driver(0).unwrap_err().raw_os_error(),
        gone
    );
    let mut data = [0u8; 2];
    assert_eq!(
        file.control_in(VENDOR_IN, &mut data, 10)
            .unwrap_err()
            .raw_os_error(),
        gone
    );
}

#[test]
fn a_control_in_hands_the_kernel_the_slice_it_was_given() {
    let (file, fake) = file();
    fake.queue_reply(Reply {
        data: vec![1, 2, 3],
        returned: None,
    });
    let mut data = [0u8; 8];
    assert_eq!(file.control_in(VENDOR_IN, &mut data, 250).unwrap(), 3);
    assert_eq!(data, [1, 2, 3, 0, 0, 0, 0, 0]);
    assert_eq!(
        fake.calls(),
        vec![Call::Control {
            request_type: 0xc0,
            request: 7,
            value: 0x1234,
            index: 0x5678,
            length: 8,
            timeout: 250,
            data: data.as_ptr() as usize,
        }]
    );
}

#[test]
fn a_control_out_hands_the_kernel_the_slice_it_was_given() {
    let (file, fake) = file();
    let data = [9u8; 5];
    assert_eq!(file.control_out(VENDOR_OUT, &data, 100).unwrap(), 5);
    assert_eq!(
        fake.calls(),
        vec![Call::Control {
            request_type: 0x40,
            request: 9,
            value: 1,
            index: 2,
            length: 5,
            timeout: 100,
            data: data.as_ptr() as usize,
        }]
    );
}

#[test]
fn a_control_call_in_the_wrong_direction_never_reaches_the_kernel() {
    let (file, fake) = file();
    let mut data = [0u8; 4];
    // An OUT call with the IN bit set would have the kernel write through a `&[u8]`.
    invalid_input(file.control_out(VENDOR_IN, &data, 10));
    invalid_input(file.control_in(VENDOR_OUT, &mut data, 10));
    assert_eq!(fake.calls(), vec![]);
}

#[test]
fn control_data_over_the_wlength_range_never_reaches_the_kernel() {
    let (file, fake) = file();
    let mut data = vec![0u8; 65_536];
    invalid_input(file.control_in(VENDOR_IN, &mut data, 10));
    invalid_input(file.control_out(VENDOR_OUT, &data, 10));
    assert_eq!(fake.calls(), vec![]);
}

#[test]
fn a_bulk_transfer_hands_the_kernel_the_slice_it_was_given() {
    let (file, fake) = file();
    fake.queue_reply(Reply {
        data: vec![7; 4],
        returned: None,
    });
    let mut read = [0u8; 16];
    assert_eq!(file.bulk_in(0x81, &mut read, 30).unwrap(), 4);
    assert_eq!(&read[..5], &[7, 7, 7, 7, 0]);
    let write = [1u8; 3];
    assert_eq!(file.bulk_out(0x02, &write, 40).unwrap(), 3);
    assert_eq!(
        fake.calls(),
        vec![
            Call::Bulk {
                endpoint: 0x81,
                length: 16,
                timeout: 30,
                data: read.as_ptr() as usize,
            },
            Call::Bulk {
                endpoint: 0x02,
                length: 3,
                timeout: 40,
                data: write.as_ptr() as usize,
            },
        ]
    );
}

#[test]
fn a_bulk_call_on_a_bad_endpoint_never_reaches_the_kernel() {
    let (file, fake) = file();
    let mut data = [0u8; 4];
    // An OUT call on an IN endpoint would have the kernel write through a `&[u8]`.
    invalid_input(file.bulk_out(0x81, &data, 10));
    invalid_input(file.bulk_in(0x01, &mut data, 10));
    invalid_input(file.bulk_in(0x80, &mut data, 10));
    invalid_input(file.bulk_out(0x00, &data, 10));
    invalid_input(file.bulk_in(0xc1, &mut data, 10));
    assert_eq!(fake.calls(), vec![]);
}

#[test]
fn a_count_larger_than_the_buffer_is_an_error() {
    let (file, fake) = file();
    let mut data = [0u8; 4];
    for returned in [5, 4096, i32::MAX] {
        fake.queue_reply(Reply {
            data: vec![0; 4],
            returned: Some(returned),
        });
        invalid_data(file.control_in(VENDOR_IN, &mut data, 10));
        fake.queue_reply(Reply {
            data: vec![0; 4],
            returned: Some(returned),
        });
        invalid_data(file.bulk_in(0x81, &mut data, 10));
    }
    // A full transfer is the limit, and is fine.
    fake.queue_reply(Reply {
        data: vec![6; 4],
        returned: Some(4),
    });
    assert_eq!(file.bulk_in(0x81, &mut data, 10).unwrap(), 4);
}

#[test]
fn interface_calls_carry_their_numbers() {
    let (file, fake) = file();
    file.claim_interface(3).unwrap();
    file.set_interface(3, 2).unwrap();
    file.release_interface(3).unwrap();
    assert_eq!(
        fake.calls(),
        vec![
            Call::Claim(3),
            Call::SetInterface {
                interface: 3,
                alternate: 2
            },
            Call::Release(3),
        ]
    );
}

#[test]
fn a_kernel_error_comes_back_with_its_errno() {
    let (file, fake) = file();
    fake.fail_next(libc::ENODEV);
    assert_eq!(
        file.claim_interface(0).unwrap_err().raw_os_error(),
        Some(libc::ENODEV)
    );
    fake.fail_next(libc::EPIPE);
    let mut data = [0u8; 2];
    assert_eq!(
        file.control_in(VENDOR_IN, &mut data, 10)
            .unwrap_err()
            .raw_os_error(),
        Some(libc::EPIPE)
    );
}

/// A regular file stands in for the device node: the descriptor read is positional, so the same
/// offset gives the same bytes. No USB device is opened.
#[test]
fn reading_the_descriptors_is_positional() {
    let dir = std::env::temp_dir();
    let name = format!("lemnos-usbfs-read-{}", std::process::id());
    let path = dir.join(name);
    assert!(
        !path.as_os_str().as_bytes().starts_with(b"/dev"),
        "the temporary directory must not be under /dev"
    );
    let bytes: Vec<u8> = (0..40u8).collect();
    std::fs::write(&path, &bytes).unwrap();
    let file = UsbfsFile::open(&path).unwrap();
    let (mut first, mut second) = ([0u8; 10], [0u8; 10]);
    assert_eq!(file.read_descriptors(4, &mut first).unwrap(), 10);
    assert_eq!(file.read_descriptors(4, &mut second).unwrap(), 10);
    assert_eq!(first, second);
    assert_eq!(first[0], 4);
    assert_eq!(file.read_descriptors(39, &mut first).unwrap(), 1);
    assert_eq!(file.read_descriptors(40, &mut first).unwrap(), 0);
    std::fs::remove_file(&path).unwrap();
}

/// The usbdevfs requests on a regular file are refused by the kernel, which is a real ioctl path
/// that can't touch a USB device.
#[test]
fn a_device_file_that_refuses_the_request_reports_it() {
    let path = std::env::temp_dir().join(format!("lemnos-usbfs-ioctl-{}", std::process::id()));
    std::fs::write(&path, b"").unwrap();
    let file = UsbfsFile::open(&path).unwrap();
    let mut data = [0u8; 4];
    assert_eq!(
        file.control_in(VENDOR_IN, &mut data, 10)
            .unwrap_err()
            .raw_os_error(),
        Some(libc::ENOTTY)
    );
    assert_eq!(file.reset().unwrap_err().raw_os_error(), Some(libc::ENOTTY));
    assert_eq!(
        file.detach_kernel_driver(0).unwrap_err().raw_os_error(),
        Some(libc::ENOTTY)
    );
    std::fs::remove_file(&path).unwrap();
}
