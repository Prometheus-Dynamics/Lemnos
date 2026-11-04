use super::*;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::fs::File;

/// Counts this thread's allocations, so parallel tests do not disturb a count.
struct Counting;

thread_local! {
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

fn count() {
    let _ = ALLOCATIONS.try_with(|n| n.set(n.get() + 1));
}

// SAFETY: every call forwards to `System` unchanged.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count();
        // SAFETY: the caller's layout, passed on.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: the caller's pointer and layout, passed on.
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        count();
        // SAFETY: the caller's arguments, passed on.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

const ISO: RingConfig = RingConfig {
    endpoint: 0x81,
    kind: RingKind::Iso {
        packets: 3,
        packet_size: 4,
    },
    count: 2,
    mmap: false,
};

const BULK: RingConfig = RingConfig {
    endpoint: 0x82,
    kind: RingKind::Bulk { size: 8 },
    count: 2,
    mmap: false,
};

/// The table of a ring, with no device.
fn slots(config: &RingConfig) -> Slots {
    Slots::new(config, buffer_len(config).unwrap(), None)
}

/// The URB behind `urb`, for the test to play the kernel with.
fn urb_mut<'a>(urb: *mut UrbStorage) -> &'a mut UrbStorage {
    // SAFETY: a live URB of a table, touched by nothing else during the test.
    unsafe { &mut *urb }
}

/// The URB's buffer, `len` bytes of it.
fn buffer_mut<'a>(urb: &UrbStorage, len: usize) -> &'a mut [u8] {
    // SAFETY: as in `urb_mut`; every test's `len` fits the buffer.
    unsafe { std::slice::from_raw_parts_mut(urb.header.buffer.cast(), len) }
}

/// What the kernel does for a bulk URB: writes the data and the header.
fn finish_bulk(slots: &mut Slots, slot: usize, data: &[u8]) -> *mut c_void {
    let raw = slots.prepare(slot).unwrap().unwrap();
    slots.slots[slot].in_flight = true;
    let urb = urb_mut(raw);
    buffer_mut(urb, data.len()).copy_from_slice(data);
    urb.header.actual_length = data.len() as i32;
    raw.cast()
}

/// What the kernel does for an isochronous URB: one `(status, bytes)` per packet.
fn finish_iso(slots: &mut Slots, slot: usize, packets: &[(i32, &[u8])]) -> *mut c_void {
    let raw = slots.prepare(slot).unwrap().unwrap();
    slots.slots[slot].in_flight = true;
    let urb = urb_mut(raw);
    let buffer = buffer_mut(urb, packets.len() * 4);
    let mut total = 0;
    for (index, (status, data)) in packets.iter().enumerate() {
        buffer[index * 4..index * 4 + data.len()].copy_from_slice(data);
        urb.packets[index].actual_length = data.len() as u32;
        urb.packets[index].status = *status as u32;
        total += data.len();
    }
    urb.header.actual_length = total as i32;
    raw.cast()
}

#[test]
fn iso_urbs_carry_their_packet_table() {
    let mut table = slots(&ISO);
    let urb = urb_mut(table.prepare(1).unwrap().unwrap());
    assert_eq!(urb.header.kind, URB_TYPE_ISO);
    assert_eq!(urb.header.flags, URB_ISO_ASAP);
    assert_eq!(urb.header.endpoint, 0x81);
    assert_eq!(urb.header.buffer_length, 12);
    assert_eq!(urb.header.number_of_packets, 3);
    assert_eq!(urb.packets[2].length, 4);
    assert_eq!(urb.packets[3].length, 0);
}

#[test]
fn packets_are_read_at_fixed_strides() {
    let mut table = slots(&ISO);
    let urb = finish_iso(
        &mut table,
        0,
        &[(0, &[1, 2, 3]), (-libc::EPROTO, &[4, 5]), (0, &[6])],
    );
    let done = table.complete(urb, Instant::now()).unwrap();
    assert_eq!(
        (done.slot, done.actual_length, done.packet_count()),
        (0, 6, 3)
    );
    let packets: Vec<_> = done.packets().collect();
    assert_eq!(packets[0].data, &[1, 2, 3]);
    assert_eq!(packets[1].data, &[4, 5]);
    assert_eq!(packets[1].status, -libc::EPROTO);
    assert_eq!(packets[2].data, &[6]);
}

#[test]
fn a_bulk_completion_gives_its_bytes() {
    let mut table = slots(&BULK);
    let urb = finish_bulk(&mut table, 1, &[9, 8, 7]);
    let done = table.complete(urb, Instant::now()).unwrap();
    assert_eq!((done.slot, done.data()), (1, &[9u8, 8, 7][..]));
    assert_eq!(done.packet_count(), 0);
    assert_eq!(table.idle_data(1, 99), &[9, 8, 7]);
    assert_eq!(table.idle_data(1, 2), &[9, 8]);
}

#[test]
fn an_unknown_or_idle_urb_is_an_error() {
    let mut table = slots(&BULK);
    let mut other = 0u64;
    let foreign = (&raw mut other).cast::<c_void>();
    assert!(table.complete(foreign, Instant::now()).is_err());
    let idle = table.slots[0].storage.as_ptr().cast();
    assert!(table.complete(idle, Instant::now()).is_err());
}

#[test]
fn a_urb_in_flight_is_not_prepared_again() {
    let mut table = slots(&BULK);
    table.slots[0].in_flight = true;
    assert!(table.prepare(0).unwrap().is_none());
    assert!(table.prepare(2).is_err());
    assert_eq!(table.idle_data(0, 4), &[] as &[u8]);
    table.slots[0].in_flight = false;
}

#[test]
fn exchanging_a_buffer_moves_it_without_a_copy() {
    let mut table = slots(&BULK);
    let urb = finish_bulk(&mut table, 0, &[1, 2, 3, 4]);
    table.complete(urb, Instant::now()).unwrap();
    let before = table.slots[0].buffer.as_ptr().cast::<u8>().cast_const();

    let empty = Vec::with_capacity(8);
    let fresh = empty.as_ptr();
    let full = table.exchange_buffer(0, 3, empty).unwrap();
    assert_eq!((full.as_ptr(), full.as_slice()), (before, &[1u8, 2, 3][..]));
    assert_eq!(full.capacity(), 8);
    let header_buffer = urb_mut(table.slots[0].storage.as_ptr()).header.buffer;
    assert_eq!(header_buffer.cast::<u8>().cast_const(), fresh);
    // Nothing was written to the new buffer, so nothing of it is readable.
    assert_eq!(table.idle_data(0, 8), &[] as &[u8]);

    let urb = finish_bulk(&mut table, 0, &[5, 6]);
    table.complete(urb, Instant::now()).unwrap();
    assert_eq!(table.idle_data(0, 8), &[5, 6]);
}

#[test]
fn exchanges_are_refused_with_the_buffer_back() {
    let mut table = slots(&BULK);
    let urb = finish_bulk(&mut table, 0, &[1, 2]);
    table.complete(urb, Instant::now()).unwrap();
    assert!(table.exchange_buffer(0, 3, Vec::with_capacity(8)).is_err());
    assert!(table.exchange_buffer(0, 2, Vec::with_capacity(9)).is_err());
    assert!(table.exchange_buffer(5, 0, Vec::with_capacity(8)).is_err());
    table.slots[0].in_flight = true;
    let back = table
        .exchange_buffer(0, 0, Vec::with_capacity(8))
        .unwrap_err();
    assert_eq!(back.capacity(), 8);
    table.slots[0].in_flight = false;

    let mut iso = slots(&ISO);
    assert!(iso.exchange_buffer(0, 0, Vec::with_capacity(12)).is_err());
}

#[test]
fn configurations_are_checked() {
    let check = |endpoint, kind, count| {
        buffer_len(&RingConfig {
            endpoint,
            kind,
            count,
            mmap: false,
        })
    };
    let bulk = RingKind::Bulk { size: 8 };
    assert_eq!(check(0x81, bulk, 1).unwrap(), 8);
    assert!(check(0x01, bulk, 1).is_err());
    assert!(check(0x81, bulk, 0).is_err());
    assert!(check(0x81, RingKind::Bulk { size: 0 }, 1).is_err());
    let iso = |packets, packet_size| RingKind::Iso {
        packets,
        packet_size,
    };
    assert_eq!(check(0x81, iso(128, 1024), 1).unwrap(), 128 * 1024);
    assert!(check(0x81, iso(129, 1024), 1).is_err());
    assert!(check(0x81, iso(0, 1024), 1).is_err());
    assert!(check(0x81, iso(1, 0), 1).is_err());
}

/// Every step of a steady stream except the kernel's own part of the ioctls.
#[test]
fn a_steady_stream_allocates_nothing() {
    let mut iso = slots(&ISO);
    let mut bulk = slots(&BULK);
    let start = ALLOCATIONS.with(Cell::get);
    let mut bytes = 0;
    for round in 0..1000 {
        let slot = round % 2;
        let urb = finish_iso(&mut iso, slot, &[(0, &[1, 2, 3, 4]), (0, &[5]), (0, &[])]);
        let done = iso.complete(urb, Instant::now()).unwrap();
        bytes += done.packets().map(|p| p.data.len()).sum::<usize>();

        let urb = finish_bulk(&mut bulk, slot, &[1, 2, 3]);
        let done = bulk.complete(urb, Instant::now()).unwrap();
        bytes += done.data().len();
    }
    assert_eq!(ALLOCATIONS.with(Cell::get) - start, 0);
    assert_eq!(bytes, 8000);
}

/// The ring's own calls allocate nothing either, even when the kernel refuses
/// them: `/dev/null` answers every usbdevfs request with `ENOTTY`.
#[test]
fn the_ring_calls_allocate_nothing() {
    let file = File::open("/dev/null").unwrap();
    let mut ring = UrbRing::new(file.as_fd(), ISO).unwrap();
    assert!(!ring.is_mapped());
    let start = ALLOCATIONS.with(Cell::get);
    for _ in 0..1000 {
        assert_eq!(
            ring.submit(0).unwrap_err().raw_os_error(),
            Some(libc::ENOTTY)
        );
        assert_eq!(ring.in_flight(), 0);
        assert!(ring.reap().is_err());
    }
    assert_eq!(ALLOCATIONS.with(Cell::get) - start, 0);
}

#[test]
fn a_mapping_that_fails_falls_back_to_the_heap() {
    let file = File::open("/dev/null").unwrap();
    let ring = UrbRing::new(file.as_fd(), RingConfig { mmap: true, ..BULK }).unwrap();
    assert!(!ring.is_mapped());
}

#[test]
fn an_empty_ring_cancels_at_once() {
    let file = File::open("/dev/null").unwrap();
    let mut ring = UrbRing::new(file.as_fd(), BULK).unwrap();
    assert_eq!(ring.cancel(Duration::from_secs(5)), 0);
}
