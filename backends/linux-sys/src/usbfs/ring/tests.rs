//! The ring against crafted completions, with the kernel played by the fake.

use super::*;
use crate::test_alloc::ALLOCATIONS;
use crate::usbfs::UsbfsFile;
use crate::usbfs::fake::{Call, CraftedUrb, FakeUsbfs};
use std::cell::Cell;

const CONFIG: RingConfig = RingConfig {
    count: 2,
    buffer_len: 12,
};
const LENGTHS: [u32; 3] = [4, 4, 4];

fn ring() -> (UrbRing, FakeUsbfs) {
    let fake = FakeUsbfs::new();
    let ring = UsbfsFile::fake(fake.clone()).urb_ring(CONFIG).unwrap();
    (ring, fake)
}

/// A completion of `LENGTHS` that fills each packet and has every field true.
fn good() -> CraftedUrb {
    CraftedUrb {
        packets: vec![(0, 4), (0, 4), (0, 4)],
        payload: (0..12).collect(),
        ..CraftedUrb::default()
    }
}

/// What `urb` reaps to: the error, which must be `InvalidData`, and that the URB came back idle
/// and usable.
fn refused(urb: CraftedUrb) {
    let (mut ring, fake) = ring();
    ring.submit(0, 0x81, &LENGTHS).unwrap();
    fake.queue_urb(urb);
    let error = ring.reap().unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert_eq!(ring.in_flight(), 0, "the kernel gave the URB back");

    ring.submit(0, 0x81, &LENGTHS).unwrap();
    fake.queue_urb(good());
    let done = ring.reap().unwrap().unwrap();
    assert_eq!(done.slot, 0);
    assert_eq!(done.actual_length, 12);
}

#[test]
fn a_submission_carries_its_layout() {
    let (mut ring, fake) = ring();
    ring.submit(1, 0x83, &[1, 2, 3]).unwrap();
    let urb = ring.slots.slots[1].storage.as_ptr() as usize;
    let buffer = ring.slots.slots[1].buffer.as_ptr().cast::<u8>() as usize;
    assert_eq!(
        fake.calls(),
        vec![Call::Submit {
            urb,
            kind: URB_TYPE_ISO,
            endpoint: 0x83,
            flags: URB_ISO_ASAP,
            buffer,
            buffer_length: 6,
            lengths: vec![1, 2, 3],
        }]
    );
    assert_eq!(ring.in_flight(), 1);
}

#[test]
fn packets_are_read_at_scheduled_offsets() {
    let (mut ring, fake) = ring();
    ring.submit(0, 0x81, &LENGTHS).unwrap();
    fake.queue_urb(CraftedUrb {
        status: -libc::EXDEV,
        packets: vec![(0, 3), (-libc::EPROTO, 2), (0, 1)],
        payload: (1..=12).collect(),
        ..CraftedUrb::default()
    });
    let done = ring.reap().unwrap().unwrap();
    assert_eq!(done.slot, 0);
    assert_eq!(done.actual_length, 6);
    assert_eq!(done.status, -libc::EXDEV);
    assert_eq!(done.packet_count(), 3);
    let packets: Vec<_> = done.packets().collect();
    assert_eq!(packets[0].data, &[1, 2, 3]);
    assert_eq!(packets[0].status, 0);
    // The short first packet leaves a gap: the second starts at the scheduled offset 4.
    assert_eq!(packets[1].data, &[5, 6]);
    assert_eq!(packets[1].status, -libc::EPROTO);
    assert_eq!(packets[2].data, &[9]);
}

#[test]
fn offsets_follow_varied_lengths() {
    let (mut ring, fake) = ring();
    ring.submit(0, 0x81, &[2, 6, 4]).unwrap();
    fake.queue_urb(CraftedUrb {
        packets: vec![(0, 2), (0, 6), (0, 4)],
        payload: (0..12).collect(),
        ..CraftedUrb::default()
    });
    let done = ring.reap().unwrap().unwrap();
    let packets: Vec<_> = done.packets().collect();
    assert_eq!(packets[0].data, &[0, 1]);
    assert_eq!(packets[1].data, &[2, 3, 4, 5, 6, 7]);
    assert_eq!(packets[2].data, &[8, 9, 10, 11]);
}

#[test]
fn a_full_buffer_is_accepted_at_the_limit() {
    let (mut ring, fake) = ring();
    ring.submit(0, 0x81, &[12]).unwrap();
    fake.queue_urb(CraftedUrb {
        actual_length: Some(12),
        packets: vec![(0, 12)],
        payload: vec![5; 12],
        ..CraftedUrb::default()
    });
    let done = ring.reap().unwrap().unwrap();
    assert_eq!(done.packets().next().unwrap().data, &[5u8; 12][..]);
}

#[test]
fn a_header_claiming_more_than_its_buffer_is_refused() {
    refused(CraftedUrb {
        actual_length: Some(13),
        ..good()
    });
    refused(CraftedUrb {
        actual_length: Some(i32::MAX),
        ..good()
    });
}

/// The buffer is larger than this URB's packets, and a claim is held to the packets.
#[test]
fn a_header_claiming_more_than_was_submitted_is_refused_even_within_the_buffer() {
    let (mut ring, fake) = ring();
    ring.submit(0, 0x81, &[4]).unwrap();
    fake.queue_urb(CraftedUrb {
        actual_length: Some(5),
        packets: vec![(0, 4)],
        ..CraftedUrb::default()
    });
    assert_eq!(ring.reap().unwrap_err().kind(), io::ErrorKind::InvalidData);
}

#[test]
fn a_negative_header_length_is_refused() {
    refused(CraftedUrb {
        actual_length: Some(-1),
        ..good()
    });
    refused(CraftedUrb {
        actual_length: Some(i32::MIN),
        ..good()
    });
}

#[test]
fn a_packet_claiming_more_than_it_was_given_is_refused() {
    // One byte past the packet's own length, though it still fits the buffer.
    refused(CraftedUrb {
        packets: vec![(0, 5), (0, 4), (0, 3)],
        ..good()
    });
    refused(CraftedUrb {
        packets: vec![(0, 4), (0, u32::MAX), (0, 4)],
        ..good()
    });
    refused(CraftedUrb {
        packets: vec![(0, 4), (0, 4), (0, 13)],
        ..good()
    });
}

#[test]
fn a_changed_packet_count_is_refused() {
    for count in [0, 2, 4, 128, -1, i32::MAX] {
        refused(CraftedUrb {
            number_of_packets: Some(count),
            ..good()
        });
    }
}

#[test]
fn a_changed_buffer_or_length_is_refused() {
    refused(CraftedUrb {
        buffer_length: Some(13),
        ..good()
    });
    refused(CraftedUrb {
        buffer: Some(0x1000),
        ..good()
    });
}

/// The kernel has no reason to change `buffer`, but a resubmission must not depend on that.
#[test]
fn a_changed_buffer_pointer_is_not_carried_into_the_next_submit() {
    let (mut ring, fake) = ring();
    let real = ring.slots.slots[0].buffer.as_ptr().cast::<u8>() as usize;
    ring.submit(0, 0x81, &LENGTHS).unwrap();
    fake.queue_urb(CraftedUrb {
        buffer: Some(0x1000),
        ..good()
    });
    assert!(ring.reap().is_err());
    ring.submit(0, 0x81, &LENGTHS).unwrap();
    let submitted = fake.calls().into_iter().rev().find_map(|call| match call {
        Call::Submit { buffer, .. } => Some(buffer),
        _ => None,
    });
    assert_eq!(submitted, Some(real));
}

#[test]
fn a_urb_the_ring_did_not_submit_is_an_error_and_is_not_touched() {
    let (mut ring, fake) = ring();
    ring.submit(0, 0x81, &LENGTHS).unwrap();
    // An address nothing is mapped at: only the address is compared.
    fake.queue_urb(CraftedUrb {
        foreign: Some(0x10),
        ..good()
    });
    assert_eq!(ring.reap().unwrap_err().kind(), io::ErrorKind::InvalidData);
    assert_eq!(ring.in_flight(), 1, "the real URB is still the kernel's");
    fake.queue_urb(good());
    assert_eq!(ring.reap().unwrap().unwrap().actual_length, 12);
}

#[test]
fn a_urb_reported_twice_is_refused_the_second_time() {
    let (mut ring, fake) = ring();
    ring.submit(0, 0x81, &LENGTHS).unwrap();
    fake.queue_urb(CraftedUrb {
        repeat_address: true,
        ..good()
    });
    assert_eq!(ring.reap().unwrap().unwrap().slot, 0);
    assert_eq!(ring.reap().unwrap_err().kind(), io::ErrorKind::InvalidData);
}

#[test]
fn a_bad_submission_never_reaches_the_kernel() {
    let (mut ring, fake) = ring();
    let bad = |ring: &mut UrbRing, slot, endpoint, lengths: &[u32]| {
        assert_eq!(
            ring.submit(slot, endpoint, lengths).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    };
    // The kernel takes the transfer's length from this table and never compares it to the buffer.
    bad(&mut ring, 0, 0x81, &[13]);
    bad(&mut ring, 0, 0x81, &[6, 6, 1]);
    // These wrap a u32 sum to something small.
    bad(&mut ring, 0, 0x81, &[u32::MAX, 2]);
    bad(&mut ring, 0, 0x81, &[1 << 31, 1 << 31, 4]);
    bad(&mut ring, 0, 0x81, &[0; MAX_ISO_PACKETS + 1]);
    bad(&mut ring, 0, 0x81, &[]);
    bad(&mut ring, 2, 0x81, &LENGTHS);
    bad(&mut ring, usize::MAX, 0x81, &LENGTHS);
    bad(&mut ring, 0, 0x01, &LENGTHS);
    bad(&mut ring, 0, 0x80, &LENGTHS);
    bad(&mut ring, 0, 0xa1, &LENGTHS);
    assert_eq!(fake.calls(), vec![]);
    assert_eq!(ring.in_flight(), 0);
}

#[test]
fn a_urb_in_flight_is_not_armed_again() {
    let (mut ring, fake) = ring();
    ring.submit(0, 0x81, &[4]).unwrap();
    assert_eq!(
        ring.submit(0, 0x81, &[8]).unwrap_err().kind(),
        io::ErrorKind::ResourceBusy
    );
    assert_eq!(fake.calls().len(), 1);
    fake.queue_urb(CraftedUrb {
        packets: vec![(0, 4)],
        ..CraftedUrb::default()
    });
    assert_eq!(ring.reap().unwrap().unwrap().actual_length, 4);
}

#[test]
fn a_refused_submit_leaves_the_urb_idle() {
    let (mut ring, fake) = ring();
    fake.fail_next(libc::EINVAL);
    assert_eq!(
        ring.submit(0, 0x81, &LENGTHS).unwrap_err().raw_os_error(),
        Some(libc::EINVAL)
    );
    assert_eq!(ring.in_flight(), 0);
    ring.submit(0, 0x81, &LENGTHS).unwrap();
    assert_eq!(ring.in_flight(), 1);
}

#[test]
fn shapes_are_checked() {
    let check = |count, buffer_len| check_config(&RingConfig { count, buffer_len });
    assert!(check(4, 1024).is_ok());
    assert!(check(1, i32::MAX as usize).is_ok());
    assert!(check(0, 1024).is_err());
    assert!(check(4, 0).is_err());
    assert!(check(1, i32::MAX as usize + 1).is_err());
    assert!(check(2, i32::MAX as usize).is_err());
    assert!(check(usize::MAX, 2).is_err(), "the total would wrap");
}

#[test]
fn a_cancel_discards_and_reaps() {
    let (mut ring, fake) = ring();
    ring.submit(0, 0x81, &LENGTHS).unwrap();
    ring.submit(1, 0x81, &LENGTHS).unwrap();
    assert_eq!(ring.cancel(Duration::from_secs(1)), 0);
    assert_eq!(ring.in_flight(), 0);
    let discards = fake
        .calls()
        .iter()
        .filter(|call| matches!(call, Call::Discard(_)))
        .count();
    assert_eq!(discards, 2);
    assert_eq!(fake.in_flight(), 0);
}

#[test]
fn a_urb_the_kernel_keeps_stays_in_flight_until_it_comes_back() {
    let (mut ring, fake) = ring();
    ring.submit(0, 0x81, &LENGTHS).unwrap();
    fake.set_stuck(true);
    assert_eq!(ring.cancel(Duration::from_millis(1)), 1);
    assert_eq!(ring.in_flight(), 1);
    assert_eq!(
        ring.submit(0, 0x81, &LENGTHS).unwrap_err().kind(),
        io::ErrorKind::ResourceBusy,
        "its memory is still the kernel's"
    );
    fake.set_stuck(false);
    assert_eq!(ring.cancel(Duration::from_secs(1)), 0);
    ring.submit(0, 0x81, &LENGTHS).unwrap();
    assert_eq!(ring.in_flight(), 1);
}

#[test]
fn a_cancel_keeps_draining_past_a_bad_report() {
    let (mut ring, fake) = ring();
    ring.submit(0, 0x81, &LENGTHS).unwrap();
    fake.set_stuck(true);
    fake.queue_urb(CraftedUrb {
        actual_length: Some(9999),
        ..good()
    });
    assert_eq!(ring.cancel(Duration::from_secs(1)), 0);
    assert_eq!(fake.in_flight(), 0);
}

#[test]
fn a_cancel_of_a_device_that_left_gives_up_at_once() {
    let (mut ring, fake) = ring();
    ring.submit(0, 0x81, &LENGTHS).unwrap();
    fake.set_stuck(true);
    fake.set_gone(true);
    let start = Instant::now();
    assert_eq!(ring.cancel(Duration::from_secs(30)), 1);
    assert!(start.elapsed() < Duration::from_secs(5));
    fake.set_stuck(false);
    fake.set_gone(false);
    assert_eq!(ring.cancel(Duration::from_secs(1)), 0);
}

#[test]
fn waiting_reports_a_completion_or_a_departure() {
    let (mut ring, fake) = ring();
    assert_eq!(
        ring.wait(Duration::from_millis(1)).unwrap(),
        Readiness::Idle
    );
    ring.submit(0, 0x81, &LENGTHS).unwrap();
    fake.queue_urb(good());
    assert_eq!(
        ring.wait(Duration::from_millis(1)).unwrap(),
        Readiness::Completion
    );
    assert!(ring.reap().unwrap().is_some());
    fake.set_gone(true);
    assert_eq!(
        ring.wait(Duration::from_millis(1)).unwrap(),
        Readiness::Gone
    );
    assert_eq!(ring.reap().unwrap_err().raw_os_error(), Some(libc::ENODEV));
}

/// Every step of a steady stream through the ring, with the kernel's part played by the fake.
#[test]
fn a_steady_stream_allocates_nothing() {
    let (mut ring, fake) = ring();
    fake.stop_recording();
    fake.set_steady_urb(good());
    let mut bytes = 0;
    let start = ALLOCATIONS.with(Cell::get);
    for round in 0..1000 {
        let slot = round % 2;
        ring.submit(slot, 0x81, &LENGTHS).unwrap();
        let done = ring.reap().unwrap().unwrap();
        assert_eq!(done.slot, slot);
        bytes += done
            .packets()
            .map(|packet| packet.data.len())
            .sum::<usize>();
    }
    assert_eq!(ALLOCATIONS.with(Cell::get) - start, 0);
    assert_eq!(bytes, 12_000);
}

/// Refusals are as cheap as success: nothing is built for them.
#[test]
fn a_refused_completion_allocates_nothing() {
    let (mut ring, fake) = ring();
    fake.stop_recording();
    fake.set_steady_urb(CraftedUrb {
        actual_length: Some(1 << 20),
        ..good()
    });
    let start = ALLOCATIONS.with(Cell::get);
    for _ in 0..1000 {
        ring.submit(0, 0x81, &LENGTHS).unwrap();
        assert_eq!(ring.reap().unwrap_err().kind(), io::ErrorKind::InvalidData);
        assert_eq!(
            ring.submit(0, 0x81, &[13]).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }
    assert_eq!(ALLOCATIONS.with(Cell::get) - start, 0);
}
