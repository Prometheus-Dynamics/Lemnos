//! Host tests: no device is needed. The descriptor blob is built by hand, the
//! completion path is driven directly, and the listing only reads the bus.

use crate::config::configurations_from_blob;
use crate::device::check_endpoint;
use crate::error::{Error, ErrorKind};
use crate::transfer::{IsoPacket, Shared, Transfer, TransferOutcome, TransferStatus};
use crate::types::{Direction, TransferType};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};
use std::time::Duration;

/// A device descriptor, then one configuration (value 1) with interface 0 and
/// a bulk IN endpoint 0x81 and a bulk OUT endpoint 0x01, both 512 bytes.
fn sample_blob() -> Vec<u8> {
    let mut blob = vec![
        18, 1, 0x00, 0x02, 0, 0, 0, 64, 0x34, 0x12, 0x78, 0x56, 0x00, 0x01, 1, 2, 3, 1,
    ];
    let config_total: u16 = 9 + 9 + 7 + 7;
    blob.extend_from_slice(&[9, 2]);
    blob.extend_from_slice(&config_total.to_le_bytes());
    blob.extend_from_slice(&[1, 1, 0, 0x80, 50]);
    blob.extend_from_slice(&[9, 4, 0, 0, 2, 0xff, 0, 0, 0]);
    blob.extend_from_slice(&[7, 5, 0x81, 0x02, 0x00, 0x02, 0]);
    blob.extend_from_slice(&[7, 5, 0x01, 0x02, 0x00, 0x02, 0]);
    blob
}

#[test]
fn parses_configurations_from_the_raw_descriptors() {
    let configurations = configurations_from_blob(&sample_blob()).unwrap();
    assert_eq!(configurations.len(), 1);
    let config = &configurations[0];
    assert_eq!(config.value, 1);
    let interface = config.interface(0).expect("interface 0");
    assert_eq!(interface.alternates.len(), 1);
    assert_eq!(interface.alternates[0].class, 0xff);
    assert_eq!(interface.alternates[0].endpoints.len(), 2);
}

#[test]
fn finds_endpoints_by_direction_and_type() {
    let config = configurations_from_blob(&sample_blob()).unwrap().remove(0);
    let bulk_in = config
        .find_endpoint(0, None, Direction::In, TransferType::Bulk)
        .expect("bulk IN");
    assert_eq!(bulk_in.address, 0x81);
    assert_eq!(bulk_in.max_packet_size, 512);
    let bulk_out = config
        .find_endpoint(0, Some(0), Direction::Out, TransferType::Bulk)
        .expect("bulk OUT");
    assert_eq!(bulk_out.address, 0x01);
    assert!(
        config
            .find_endpoint(0, None, Direction::In, TransferType::Interrupt)
            .is_none()
    );
    assert!(
        config
            .find_endpoint(0, Some(3), Direction::In, TransferType::Bulk)
            .is_none()
    );
    assert!(
        config
            .find_endpoint(9, None, Direction::In, TransferType::Bulk)
            .is_none()
    );
}

#[test]
fn rejects_a_truncated_configuration() {
    let mut blob = sample_blob();
    blob.truncate(blob.len() - 3);
    let error = configurations_from_blob(&blob).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Other);
}

#[cfg(target_os = "linux")]
#[test]
fn classifies_errno_failures() {
    let denied = Error::from_io("open", &std::io::Error::from_raw_os_error(libc::EACCES));
    assert_eq!(denied.kind(), ErrorKind::PermissionDenied);
    assert_eq!(denied.os_code(), Some(libc::EACCES));
    let busy = Error::from_io("claim", &std::io::Error::from_raw_os_error(libc::EBUSY));
    assert_eq!(busy.kind(), ErrorKind::Busy);
    let gone = Error::from_io("bulk", &std::io::Error::from_raw_os_error(libc::ENODEV));
    assert_eq!(gone.kind(), ErrorKind::Disconnected);
    let stalled = Error::from_status("bulk", -libc::EPIPE);
    assert_eq!(stalled.kind(), ErrorKind::Stall);
    let discarded = Error::from_status("bulk", -libc::ENOENT);
    assert_eq!(discarded.kind(), ErrorKind::Cancelled);
    let unsupported = Error::from_io("iso", &std::io::Error::from_raw_os_error(libc::ENOSYS));
    assert_eq!(unsupported.kind(), ErrorKind::Unsupported);
    assert!(denied.to_string().starts_with("permission denied: open"));
}

#[test]
fn outcome_reports_the_bytes_that_moved() {
    let outcome = TransferOutcome {
        buffer: crate::TransferBuffer::from_vec(vec![1, 2, 3, 4]),
        actual_length: 2,
        packets: Vec::new(),
        status: TransferStatus::Ok,
        reaped: std::time::Instant::now(),
    };
    assert_eq!(outcome.data(), &[1, 2]);
    assert_eq!(outcome.into_buffer().into_vec(), vec![1, 2, 3, 4]);
}

#[test]
fn a_transfer_resolves_when_completed_from_another_thread() {
    let shared = Shared::new();
    let transfer = Transfer::new(Arc::clone(&shared), None);
    let completer = Arc::clone(&shared);
    let handle = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(10));
        completer.complete(Ok(TransferOutcome {
            buffer: crate::TransferBuffer::from_vec(vec![7; 3]),
            actual_length: 3,
            packets: Vec::new(),
            status: TransferStatus::Ok,
            reaped: std::time::Instant::now(),
        }));
    });
    let outcome = transfer.wait().unwrap();
    assert_eq!(outcome.data(), &[7, 7, 7]);
    handle.join().unwrap();
}

#[test]
fn a_transfer_is_a_future_that_wakes_its_task() {
    struct Flag(std::sync::atomic::AtomicBool);
    impl Wake for Flag {
        fn wake(self: Arc<Self>) {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }
    let shared = Shared::new();
    let mut transfer = Transfer::new(Arc::clone(&shared), None);
    let flag = Arc::new(Flag(std::sync::atomic::AtomicBool::new(false)));
    let waker = Waker::from(Arc::clone(&flag));
    let mut context = Context::from_waker(&waker);
    assert!(matches!(
        Pin::new(&mut transfer).poll(&mut context),
        Poll::Pending
    ));
    shared.complete(Err(Error::new(ErrorKind::Cancelled, "cancelled")));
    assert!(flag.0.load(std::sync::atomic::Ordering::SeqCst));
    match Pin::new(&mut transfer).poll(&mut context) {
        Poll::Ready(Err(error)) => assert_eq!(error.kind(), ErrorKind::Cancelled),
        Poll::Ready(Ok(_)) => panic!("the transfer failed, not succeeded"),
        Poll::Pending => panic!("the transfer completed, so it is ready"),
    }
}

#[test]
fn a_timed_out_wait_returns_the_transfer_still_in_flight() {
    let shared = Shared::new();
    let transfer = Transfer::new(Arc::clone(&shared), None);
    let transfer = match transfer.wait_timeout(Duration::from_millis(5)) {
        Err(transfer) => transfer,
        Ok(_) => panic!("nothing completed the transfer"),
    };
    assert!(!transfer.is_complete());
    shared.complete(Ok(TransferOutcome {
        buffer: crate::TransferBuffer::from_vec(Vec::new()),
        actual_length: 0,
        packets: Vec::new(),
        status: TransferStatus::Ok,
        reaped: std::time::Instant::now(),
    }));
    assert!(transfer.is_complete());
}

#[test]
fn iso_packets_report_their_own_status() {
    let packet = IsoPacket {
        length: 1024,
        actual_length: 0,
        status: TransferStatus::Error(libc::EPROTO),
    };
    assert_eq!(packet.length, 1024);
    assert_eq!(packet.status, TransferStatus::Error(libc::EPROTO));
}

#[test]
fn sync_transfers_check_their_direction_and_endpoint_before_the_bus() {
    // These checks run before any backend call, so no device is needed.
    let error = check_endpoint(0x00, Direction::In).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::InvalidInput);
    let error = check_endpoint(0x81, Direction::Out).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::InvalidInput);
    assert!(check_endpoint(0x81, Direction::In).is_ok());
}

#[test]
fn lists_the_bus_without_claiming_anything() {
    // Read-only: enumerates the bus on this host, opens and claims nothing.
    let devices = crate::list_devices().expect("listing the bus");
    for device in &devices {
        assert!(device.location().ports().iter().all(|port| *port > 0));
    }
}

#[test]
fn a_hotplug_watch_starts() {
    // Starting only: a quiet bus has no event to wait for in a test.
    let _watch = crate::watch().expect("starting the hotplug watch");
}

#[test]
fn a_zero_packet_is_needed_exactly_when_the_total_ends_on_a_packet_boundary() {
    use crate::stream::needs_zero_packet;
    // High-speed bulk (512) and SuperSpeed bulk (1024).
    assert!(
        !needs_zero_packet(0, 512),
        "nothing written, nothing to end"
    );
    assert!(!needs_zero_packet(511, 512));
    assert!(needs_zero_packet(512, 512));
    assert!(!needs_zero_packet(513, 512));
    assert!(needs_zero_packet(1024, 512), "two whole packets");
    assert!(!needs_zero_packet(1536 + 1, 512));
    assert!(!needs_zero_packet(1024, 1024 + 512), "short of one packet");
    assert!(needs_zero_packet(1024, 1024), "SuperSpeed boundary");
    assert!(
        !needs_zero_packet(4096, 0),
        "an unknown packet size sends nothing"
    );
}

#[test]
fn a_worker_transfer_resolves_with_the_outcome_of_its_work() {
    let transfer = Transfer::from_worker(|| {
        Ok(TransferOutcome {
            buffer: crate::TransferBuffer::from_vec(vec![9, 8, 7]),
            actual_length: 2,
            packets: Vec::new(),
            status: TransferStatus::Ok,
            reaped: std::time::Instant::now(),
        })
    })
    .unwrap();
    let outcome = transfer.wait().unwrap();
    assert_eq!(outcome.data(), &[9, 8]);

    let failed = Transfer::from_worker(|| Err(Error::new(ErrorKind::Timeout, "timed out")))
        .unwrap()
        .wait();
    assert_eq!(failed.unwrap_err().kind(), ErrorKind::Timeout);
}

#[test]
fn a_worker_transfer_is_awaited_as_a_future() {
    let mut transfer = Transfer::from_worker(|| {
        std::thread::sleep(Duration::from_millis(10));
        Ok(TransferOutcome {
            buffer: crate::TransferBuffer::from_vec(vec![1]),
            actual_length: 1,
            packets: Vec::new(),
            status: TransferStatus::Ok,
            reaped: std::time::Instant::now(),
        })
    })
    .unwrap();
    let mut context = Context::from_waker(Waker::noop());
    loop {
        match Pin::new(&mut transfer).poll(&mut context) {
            Poll::Ready(result) => {
                assert_eq!(result.unwrap().data(), &[1]);
                break;
            }
            Poll::Pending => std::thread::sleep(Duration::from_millis(1)),
        }
    }
}

/// The recorded device descriptor is the first 18 bytes of the blob the
/// device returns, so the device-level view and the blob agree.
#[test]
fn the_device_descriptor_comes_from_the_blob_without_a_request() {
    let blob = sample_blob();
    let descriptor = crate::DeviceDescriptor::from_bytes(&blob).unwrap();
    assert_eq!(descriptor.num_configurations, 1);
    assert_eq!(configurations_from_blob(&blob).unwrap().len(), 1);
}

#[test]
fn iso_packets_split_a_transfer_with_a_shorter_last_packet() {
    use crate::device::iso_packet_lengths;
    assert_eq!(iso_packet_lengths(1000, 300), vec![300, 300, 300, 100]);
    assert_eq!(iso_packet_lengths(900, 300), vec![300, 300, 300]);
    assert_eq!(iso_packet_lengths(7, 1024), vec![7], "one short packet");
    assert_eq!(iso_packet_lengths(3072, 1024).iter().sum::<u32>(), 3072);
}

#[test]
fn a_transfer_buffer_holds_its_bytes_and_grows_and_shrinks_in_place() {
    use crate::TransferBuffer;
    let mut buffer = TransferBuffer::from_vec(vec![1, 2, 3, 4]);
    assert_eq!(buffer.len(), 4);
    assert!(!buffer.is_mapped());
    buffer.set_len(2);
    assert_eq!(buffer.as_slice(), &[1, 2]);
    buffer.set_len(4);
    assert_eq!(buffer.as_slice(), &[1, 2, 0, 0], "growing zero-fills");
    buffer.as_mut_slice()[3] = 9;
    assert_eq!(buffer.into_vec(), vec![1, 2, 0, 9]);
}

/// usbdevfs places isochronous IN packet `i` at the sum of the scheduled lengths
/// of the packets before it. A short packet in the middle leaves a gap after its
/// bytes; the packets after it stay where they were scheduled.
#[test]
#[cfg(target_os = "linux")]
fn an_iso_block_places_each_packet_at_its_scheduled_offset() {
    use crate::stream::IsoBlock;
    let buffer: Vec<u8> = (1..=12).collect();
    let packets = [
        IsoPacket {
            length: 4,
            actual_length: 4,
            status: TransferStatus::Ok,
        },
        IsoPacket {
            length: 4,
            actual_length: 2,
            status: TransferStatus::Ok,
        },
        IsoPacket {
            length: 4,
            actual_length: 4,
            status: TransferStatus::Ok,
        },
    ];
    let block = IsoBlock::new(&buffer, &packets);
    let got: Vec<&[u8]> = block.packets().map(|(_, bytes)| bytes).collect();
    assert_eq!(got, [&[1, 2, 3, 4][..], &[5, 6][..], &[9, 10, 11, 12][..]]);
    assert_eq!(block.packets().len(), 3);
}

/// A packet that moved nothing is an empty slice at its offset, and a packet
/// that moved less than it was scheduled keeps its own start.
#[test]
#[cfg(target_os = "linux")]
fn an_empty_packet_keeps_the_offsets_of_the_packets_after_it() {
    use crate::stream::IsoBlock;
    let buffer: Vec<u8> = (1..=9).collect();
    let packets = [
        IsoPacket {
            length: 3,
            actual_length: 0,
            status: TransferStatus::Ok,
        },
        IsoPacket {
            length: 3,
            actual_length: 3,
            status: TransferStatus::Ok,
        },
        IsoPacket {
            length: 3,
            actual_length: 1,
            status: TransferStatus::Ok,
        },
    ];
    let block = IsoBlock::new(&buffer, &packets);
    let got: Vec<&[u8]> = block.packets().map(|(_, bytes)| bytes).collect();
    assert_eq!(got, [&[][..], &[4, 5, 6][..], &[7][..]]);
}

/// usbdevfs reports the device speed as `enum usb_device_speed`: unknown and
/// wireless have no `Speed`.
#[test]
#[cfg(target_os = "linux")]
fn usbdevfs_speeds_map_to_the_listing_speeds() {
    use crate::info::Speed;
    assert_eq!(Speed::from_usbdevfs(0), None, "unknown");
    assert_eq!(Speed::from_usbdevfs(1), Some(Speed::Low));
    assert_eq!(Speed::from_usbdevfs(2), Some(Speed::Full));
    assert_eq!(Speed::from_usbdevfs(3), Some(Speed::High));
    assert_eq!(Speed::from_usbdevfs(4), None, "wireless");
    assert_eq!(Speed::from_usbdevfs(5), Some(Speed::Super));
    assert_eq!(Speed::from_usbdevfs(6), Some(Speed::SuperPlus));
}

/// A transfer and its buffer can be moved to another thread (a frame backing).
#[test]
fn a_transfer_buffer_is_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<crate::TransferBuffer>();
    assert_send_sync::<crate::TransferOutcome>();
}

/// Whether `/proc/self/maps` has a mapping that contains `address`.
#[cfg(target_os = "linux")]
fn mapped(address: usize) -> bool {
    let maps = std::fs::read_to_string("/proc/self/maps").unwrap();
    maps.lines().any(|line| {
        let range = line.split_whitespace().next().unwrap_or("");
        let Some((start, end)) = range.split_once('-') else {
            return false;
        };
        let start = usize::from_str_radix(start, 16).unwrap_or(usize::MAX);
        let end = usize::from_str_radix(end, 16).unwrap_or(0);
        (start..end).contains(&address)
    })
}

/// A usbdevfs-style mapping is unmapped when its transfer buffer is dropped, not
/// when the descriptor it came from closes: the mapping keeps the file open. A
/// regular file stands in for the device node (no USB device is opened).
#[cfg(target_os = "linux")]
#[test]
fn a_mapped_buffer_unmaps_on_drop_and_outlives_its_descriptor() {
    use lemnos_linux_sys::mmap::Mapping;
    use lemnos_linux_sys::usbfs::UrbBuffer;
    use std::os::fd::AsFd;

    let path = std::env::temp_dir().join(format!("lemnos-usb-map-{}", std::process::id()));
    std::fs::write(&path, vec![0x5a; 4096]).unwrap();
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    let mapping = Mapping::new(file.as_fd(), 4096, 0, true).unwrap();
    let address = mapping.as_slice().as_ptr() as usize;
    let buffer = crate::buffer::wrap(UrbBuffer::Mapped { mapping, len: 4096 });
    assert!(mapped(address), "mapped while the buffer lives");

    // The descriptor closes first: the buffer's bytes stay readable, and the
    // mapping stays where it is.
    drop(file);
    assert!(mapped(address), "the mapping outlives its descriptor");
    assert_eq!(buffer.as_slice()[0], 0x5a, "the bytes are still there");

    // The buffer goes last: its drop is the munmap.
    drop(buffer);
    assert!(!mapped(address), "the drop of the buffer unmaps it");
    std::fs::remove_file(&path).unwrap();
}

/// The outcome carries the time the completion was reaped, as given (the reaper
/// takes it before it wakes the waiter).
#[cfg(target_os = "linux")]
#[test]
fn an_outcome_carries_the_time_it_was_reaped() {
    let reaped = std::time::Instant::now();
    let outcome = TransferOutcome {
        buffer: crate::TransferBuffer::from_vec(vec![1]),
        actual_length: 1,
        packets: Vec::new(),
        status: TransferStatus::Ok,
        reaped,
    };
    assert_eq!(outcome.reaped, reaped);
}
