use std::io::{Seek, SeekFrom};
use std::path::PathBuf;

use super::*;

const INTERFACE: [u8; 9] = [9, 4, 0, 0, 2, 0xff, 0, 0, 1];
const IN_FULL: [u8; 7] = [7, 5, 0x81, 2, 64, 0, 0];
const OUT_FULL: [u8; 7] = [7, 5, 0x02, 2, 64, 0, 0];
const IN_HIGH: [u8; 7] = [7, 5, 0x81, 2, 0, 2, 0];
const OUT_HIGH: [u8; 7] = [7, 5, 0x02, 2, 0, 2, 0];

fn temp(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("lemnos-ffs-{}-{name}", std::process::id()))
}

#[test]
fn numbers_match_the_kernel_header() {
    assert_eq!((STRINGS_MAGIC, DESCRIPTORS_MAGIC_V2), (2, 3));
    assert_eq!(
        [HAS_FS_DESC, HAS_HS_DESC, HAS_SS_DESC, HAS_MS_OS_DESC],
        [1, 2, 4, 8]
    );
    assert_eq!(
        [VIRTUAL_ADDR, EVENTFD, ALL_CTRL_RECIP, CONFIG0_SETUP],
        [16, 32, 64, 128]
    );
    assert_eq!(FIFO_STATUS, 0x6701); // _IO('g', 1)
    assert_eq!(FIFO_FLUSH, 0x6702); // _IO('g', 2)
    assert_eq!(CLEAR_HALT, 0x6703); // _IO('g', 3)
    assert_eq!((STALLED, HALTED, ABANDONED, NO_REQUEST), (51, 74, 43, 3));
}

#[test]
fn an_event_is_a_setup_packet_a_type_and_padding() {
    // bRequestType, bRequest, wValue, wIndex, wLength, type, three bytes of padding.
    let setup = [0xc1, 0x05, 0x34, 0x12, 0x02, 0x00, 0x40, 0x01, 4, 0, 0, 0];
    assert_eq!(
        Event::parse(&setup),
        Some(Event::Setup(Setup {
            request_type: 0xc1,
            request: 5,
            value: 0x1234,
            index: 2,
            length: 0x0140,
        }))
    );
    let kinds = [
        Event::Bind,
        Event::Unbind,
        Event::Enable,
        Event::Disable,
        Event::Suspend,
        Event::Resume,
    ];
    for (kind, number) in kinds.into_iter().zip([0u8, 1, 2, 3, 5, 6]) {
        let mut bytes = [0xaa; EVENT_LEN];
        bytes[8] = number;
        assert_eq!(Event::parse(&bytes), Some(kind), "type {number}");
    }
    let mut unknown = [0; EVENT_LEN];
    unknown[8] = 7;
    assert_eq!(Event::parse(&unknown), None);
}

#[test]
fn the_descriptors_blob_is_the_header_the_counts_then_each_speed() {
    let full = [&INTERFACE[..], &IN_FULL, &OUT_FULL].concat();
    let high = [&INTERFACE[..], &IN_HIGH, &OUT_HIGH].concat();
    let blob = descriptors_blob(VIRTUAL_ADDR, Some(&full), Some(&high)).unwrap();
    let mut expected = Vec::new();
    expected.extend(3u32.to_le_bytes()); // magic
    expected.extend(66u32.to_le_bytes()); // length: 12 + 4 + 4 + 23 + 23
    expected.extend(19u32.to_le_bytes()); // flags: FS | HS | VIRTUAL_ADDR
    expected.extend(3u32.to_le_bytes()); // fs_count
    expected.extend(3u32.to_le_bytes()); // hs_count
    expected.extend(&full);
    expected.extend(&high);
    assert_eq!(blob, expected);

    // One speed: one count, and its flag alone.
    let blob = descriptors_blob(0, Some(&full), None).unwrap();
    assert_eq!(blob.len(), 12 + 4 + 23);
    assert_eq!(blob[4..16], [39, 0, 0, 0, 1, 0, 0, 0, 3, 0, 0, 0]);
    let blob = descriptors_blob(0, None, Some(&high)).unwrap();
    assert_eq!(blob[8..16], [2, 0, 0, 0, 3, 0, 0, 0]);
}

#[test]
fn a_descriptors_blob_is_refused_when_its_parts_do_not_add_up() {
    let refused = |flags, full: Option<&[u8]>, high: Option<&[u8]>| {
        descriptors_blob(flags, full, high).unwrap_err().kind() == io::ErrorKind::InvalidInput
    };
    assert!(refused(0, None, None), "no speed at all");
    assert!(refused(0, Some(&[9, 4, 0]), None), "a descriptor cut short");
    assert!(refused(0, Some(&[1]), None), "a length under 2");
    assert!(refused(0, Some(&[0, 4]), None), "a length of 0");
    assert!(
        refused(0, Some(&INTERFACE), Some(&[7, 5])),
        "the second speed"
    );
    for flag in [
        HAS_FS_DESC,
        HAS_HS_DESC,
        HAS_SS_DESC,
        HAS_MS_OS_DESC,
        EVENTFD,
    ] {
        assert!(refused(flag, Some(&INTERFACE), None), "flag {flag}");
    }
    // No descriptors at one speed is a count of 0.
    let blob = descriptors_blob(0, Some(&[]), None).unwrap();
    assert_eq!(blob[12..], [0, 0, 0, 0]);
}

#[test]
fn the_strings_blob_is_the_header_the_language_then_each_string_and_a_nul() {
    let blob = strings_blob(0x0409, &["ab", "", "\u{e9}"]).unwrap();
    let mut expected = Vec::new();
    expected.extend(2u32.to_le_bytes()); // magic
    expected.extend(25u32.to_le_bytes()); // length: 16 + 2 + 3 + 1 + 3
    expected.extend(3u32.to_le_bytes()); // str_count
    expected.extend(1u32.to_le_bytes()); // lang_count
    expected.extend(0x0409u16.to_le_bytes());
    expected.extend(b"ab\0\0\xc3\xa9\0");
    assert_eq!(blob, expected);

    // No strings: no language either.
    let blob = strings_blob(0x0409, &[]).unwrap();
    assert_eq!(blob, [2, 0, 0, 0, 16, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(
        strings_blob(0x0409, &["a\0b"]).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
}

#[test]
fn ep0_writes_each_blob_whole_and_reads_events_after_them() {
    let path = temp("ep0");
    std::fs::write(&path, b"").unwrap();
    let ep0 = Ep0::open(&path).unwrap();
    ep0.describe(&[1, 2, 3], &[4, 5]).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), [1, 2, 3, 4, 5]);
    assert_eq!(ep0.next_event().unwrap(), None, "nothing after the blobs");

    // The kernel's side of the file: an event, then a setup packet.
    let mut kernel = OpenOptions::new().append(true).open(&path).unwrap();
    kernel
        .write_all(&[0, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0])
        .unwrap();
    kernel
        .write_all(&[0x40, 9, 1, 0, 0, 0, 2, 0, 4, 0, 0, 0])
        .unwrap();
    assert_eq!(ep0.next_event().unwrap(), Some(Event::Enable));
    let Some(Event::Setup(setup)) = ep0.next_event().unwrap() else {
        panic!("not a setup");
    };
    assert_eq!((setup.request, setup.length, setup.is_in()), (9, 2, false));

    // The data stage of that request, as the host would send it.
    kernel.write_all(&[7, 8]).unwrap();
    let mut data = [0u8; 2];
    assert_eq!(ep0.receive(&mut data).unwrap(), 2);
    assert_eq!(data, [7, 8]);
    assert_eq!(ep0.send(&[9]).unwrap(), 1);

    // An event that is cut short or of an unknown type is not guessed at.
    kernel.write_all(&[0; 5]).unwrap();
    assert_eq!(
        ep0.next_event().unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    kernel
        .write_all(&[0, 0, 0, 0, 0, 0, 0, 0, 9, 0, 0, 0])
        .unwrap();
    assert_eq!(
        ep0.next_event().unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    std::fs::remove_file(path).unwrap();
}

#[test]
fn a_file_that_is_not_functionfs_fails_cleanly() {
    let missing = temp("missing");
    assert_eq!(
        Ep0::open(&missing).unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
    assert!(!missing.exists(), "opening never creates the file");
    assert_eq!(
        Endpoint::open(&missing).unwrap_err().kind(),
        io::ErrorKind::NotFound
    );

    let path = temp("plain");
    std::fs::write(&path, b"x").unwrap();
    let ep0 = Ep0::open(&path).unwrap();
    // A plain file carries the byte where FunctionFS would stall.
    assert_eq!(
        ep0.stall(true).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(
        ep0.stall(false).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    let endpoint = Endpoint::open(&path).unwrap();
    (&endpoint.file).seek(SeekFrom::Start(0)).unwrap();
    assert_eq!(
        endpoint.halt(true).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(
        endpoint.halt(false).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    // The ioctl is one a regular file does not have.
    assert_eq!(
        endpoint.clear_halt().unwrap_err().raw_os_error(),
        Some(libc::ENOTTY)
    );
    std::fs::remove_file(path).unwrap();
}

#[test]
fn a_wrong_way_transfer_succeeds_only_on_the_expected_error() {
    // A directory answers a read with EISDIR: stand it in for the two codes.
    let dir = File::open(std::env::temp_dir()).unwrap();
    assert!(wrong_way(&dir, true, libc::EISDIR).is_ok());
    let error = wrong_way(&dir, true, STALLED).unwrap_err();
    assert_eq!(error.raw_os_error(), Some(libc::EISDIR));
}
