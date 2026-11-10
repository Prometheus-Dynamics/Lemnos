//! Session tests on a node that plays a device back: queue types, index and plane checks, the
//! order of calls at open, deferred capture and source changes, drains, numbers from the device
//! that do not fit, and the steady state's allocations.

use super::*;
use crate::mock::{Call, Mock, MockNode, fill_byte};
use lemnos_linux_sys::v4l2::{PixFormat, PixFormatMplane, PlaneFormat};
use lemnos_measure::test_alloc::allocations;

fn multi(fourcc: &[u8; 4], planes: u32, size: u32) -> Format {
    let mut p = [PlaneFormat::default(); MAX_PLANES];
    for plane in p.iter_mut().take(planes as usize) {
        plane.size_image = size;
    }
    Format::Multi(PixFormatMplane {
        width: 64,
        height: 48,
        pixelformat: sys::fourcc(fourcc),
        planes: p,
        num_planes: planes,
        ..PixFormatMplane::default()
    })
}

fn setup(format: Format, buffers: u32) -> QueueSetup {
    QueueSetup {
        format,
        memory: Memory::Mmap,
        buffers,
        crop: None,
    }
}

fn mmap(n: usize) -> Vec<QueuePlane<'static>> {
    vec![
        QueuePlane {
            memory: PlaneMemory::Mmap,
            length: 0,
            bytes_used: 0,
            data_offset: 0,
        };
        n
    ]
}

fn session(mock: &Mock, capture_buffers: u32) -> io::Result<M2mSession<MockNode>> {
    M2mSession::new(
        mock.open()?,
        setup(multi(b"S265", 1, 1 << 16), 2),
        setup(multi(b"NV12", 2, 0), capture_buffers),
    )
}

#[test]
fn queue_types_follow_the_format_layout_and_direction() {
    let single = Format::Single(PixFormat::default());
    let multi = multi(b"NV12", 2, 0);
    assert_eq!(queue_type(&single, false), BufType::VideoCapture);
    assert_eq!(queue_type(&single, true), BufType::VideoOutput);
    assert_eq!(queue_type(&multi, false), BufType::VideoCaptureMplane);
    assert_eq!(queue_type(&multi, true), BufType::VideoOutputMplane);
}

#[test]
fn the_source_format_is_set_before_the_capture_format_and_buffers_come_last() {
    let mock = Mock::new(true);
    let s = session(&mock, 4).unwrap();
    let calls: Vec<Call> = mock.with(|d| d.calls().collect());
    assert_eq!(
        calls,
        [
            Call::SetFormat(BufType::VideoOutputMplane),
            Call::SetFormat(BufType::VideoCaptureMplane),
            Call::Format(BufType::VideoCaptureMplane),
            Call::RequestBuffers(BufType::VideoCaptureMplane, 4),
            Call::RequestBuffers(BufType::VideoOutputMplane, 2),
            Call::Subscribe(sys::EVENT_SOURCE_CHANGE),
        ]
    );
    assert_eq!((s.output_buffers(), s.capture_buffers()), (2, 4));
    assert_eq!((s.output_planes(), s.capture_planes()), (1, 2));
    // Each buffer's planes are mapped: capture buffer 3, plane 1 is its own memory.
    assert!(
        s.capture_plane(3, 1)
            .unwrap()
            .iter()
            .all(|&b| b == fill_byte(true, 3, 1))
    );
    assert_eq!(s.capture_plane(4, 0), None);
    assert_eq!(s.capture_plane(0, 2), None);
}

#[test]
fn a_session_refuses_a_node_that_is_not_m2m() {
    let mock = Mock::new(true);
    mock.with(|d| d.m2m = false);
    let e = session(&mock, 4).err().unwrap();
    assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
}

#[test]
fn indices_and_planes_are_checked_before_the_device_sees_them() {
    let mock = Mock::new(true);
    let mut s = session(&mock, 4).unwrap();
    let two = mmap(2);
    let one = mmap(1);
    // Past the queue, a plane count other than the queue's.
    assert!(s.queue_capture(4, &two).is_err());
    assert!(s.queue_capture(0, &one).is_err());
    assert!(s.queue_output(0, &two, Duration::ZERO).is_err());
    // Memory that disagrees with the queue's.
    let fd = std::fs::File::open(std::env::temp_dir()).unwrap();
    let dmabuf = [QueuePlane {
        memory: PlaneMemory::Dmabuf(std::os::fd::AsFd::as_fd(&fd)),
        length: 0,
        bytes_used: 0,
        data_offset: 0,
    }];
    assert!(s.queue_output(0, &dmabuf, Duration::ZERO).is_err());
    // Twice is refused; a queued buffer is not lent.
    s.queue_capture(1, &two).unwrap();
    assert!(s.queue_capture(1, &two).is_err());
    assert_eq!(s.capture_plane(1, 0), None);
    assert_eq!(s.free_capture(), Some(0));
    // User-pointer memory is refused at open.
    let user = QueueSetup {
        memory: Memory::UserPtr,
        ..setup(multi(b"S265", 1, 1 << 16), 2)
    };
    assert!(M2mSession::new(mock.open().unwrap(), user, setup(multi(b"NV12", 2, 0), 4)).is_err());
}

#[test]
fn numbers_from_the_device_that_do_not_fit_are_refused() {
    // The driver changes the plane count.
    let mock = Mock::new(true);
    mock.with(|d| d.faults.planes = Some(3));
    assert_eq!(
        session(&mock, 4).err().unwrap().kind(),
        io::ErrorKind::InvalidData
    );
    // No buffers granted, or more than the mask holds.
    for grant in [0, 65] {
        let mock = Mock::new(true);
        mock.with(|d| d.faults.grant = Some(grant));
        assert_eq!(
            session(&mock, 4).err().unwrap().kind(),
            io::ErrorKind::InvalidData
        );
    }
    // A dequeued index past the queue is an error, not a panic, and changes nothing.
    let mock = Mock::new(true);
    let mut s = session(&mock, 4).unwrap();
    s.queue_capture(0, &mmap(2)).unwrap();
    mock.with(|d| {
        d.produce(0);
        d.faults.index = Some(200);
    });
    assert_eq!(
        s.dequeue_capture().err().unwrap().kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(s.free_capture(), Some(1));
}

#[test]
fn a_deferred_capture_queue_waits_for_the_source_change() {
    let mock = Mock::new(true);
    let mut s = session(&mock, 0).unwrap();
    assert!(s.capture_pending());
    assert_eq!(s.capture_buffers(), 0);
    mock.with(|d| d.clear_log());
    s.stream_on().unwrap();
    assert_eq!(
        mock.with(|d| d.calls().collect::<Vec<_>>()),
        [Call::StreamOn(BufType::VideoOutputMplane)]
    );
    assert!(s.start_capture().is_err());

    let change = Event {
        event_type: sys::EVENT_SOURCE_CHANGE,
        id: 0,
        pending: 0,
        sequence: 0,
        changes: sys::SRC_CH_RESOLUTION,
        timestamp: Duration::ZERO,
    };
    mock.with(|d| d.events.push(change));
    let ready = s
        .wait(
            Ready {
                event: true,
                ..Ready::default()
            },
            None,
        )
        .unwrap();
    assert!(ready.event);
    assert!(s.take_source_change().unwrap().unwrap().resolution());
    assert_eq!(s.take_source_change().unwrap(), None);
    s.adopt_capture(3).unwrap();
    assert!(!s.capture_pending());
    assert_eq!(s.capture_buffers(), 3);
    assert!(s.adopt_capture(3).is_err());
    for i in 0..3 {
        s.queue_capture(i, &mmap(2)).unwrap();
    }
    s.start_capture().unwrap();
}

#[test]
fn a_drain_ends_with_last_and_only_then_reconfigures() {
    let mock = Mock::new(true);
    let mut s = session(&mock, 2).unwrap();
    for i in 0..2 {
        s.queue_capture(i, &mmap(2)).unwrap();
    }
    s.stream_on().unwrap();
    assert!(s.reconfigure_capture(2).is_err(), "not drained yet");
    s.drain(Drain::Decoder).unwrap();
    assert!(
        s.drain(Drain::Encoder).is_err(),
        "a decoder has no encoder stop"
    );
    mock.with(|d| d.produce(0));
    let last = s.dequeue_capture().unwrap().unwrap();
    assert_ne!(last.flags & sys::BUF_FLAG_LAST, 0);
    assert!(s.capture_drained());
    mock.with(|d| d.clear_log());
    s.reconfigure_capture(3).unwrap();
    assert_eq!(
        mock.with(|d| d.calls().collect::<Vec<_>>()),
        [
            Call::StreamOff(BufType::VideoCaptureMplane),
            Call::RequestBuffers(BufType::VideoCaptureMplane, 0),
            Call::Format(BufType::VideoCaptureMplane),
            Call::RequestBuffers(BufType::VideoCaptureMplane, 3),
        ]
    );
    assert_eq!(s.capture_buffers(), 3);
    assert!(!s.capture_drained());
}

#[test]
fn waiting_reports_what_is_ready_and_device_errors() {
    let mock = Mock::new(true);
    let mut s = session(&mock, 2).unwrap();
    let all = Ready {
        output: true,
        capture: true,
        event: true,
    };
    let r = s.wait(all, Some(Duration::ZERO)).unwrap();
    assert!(r.output && !r.capture && !r.event);
    s.queue_capture(0, &mmap(2)).unwrap();
    mock.with(|d| d.produce(0));
    assert!(s.wait(all, None).unwrap().capture);
    mock.with(|d| d.faults.poll_error = true);
    assert!(s.wait(all, None).is_err());
}

#[test]
fn a_crop_the_driver_refuses_fails_the_open() {
    let mock = Mock::new(true);
    let cropped = QueueSetup {
        crop: Some(Rect::new(0, 0, 32, 32)),
        ..setup(multi(b"S265", 1, 1 << 16), 2)
    };
    assert!(
        M2mSession::new(
            mock.open().unwrap(),
            cropped,
            setup(multi(b"NV12", 2, 0), 4)
        )
        .is_err()
    );
    let mut s = session(&mock, 2).unwrap();
    assert!(s.set_output_crop(Rect::new(0, 0, 32, 32)).is_err());
    assert_eq!(s.output_crop(), None);
}

#[test]
fn queueing_and_dequeueing_allocate_nothing() {
    let mock = Mock::new(true);
    let mut s = session(&mock, 4).unwrap();
    let planes = mmap(2);
    for i in 0..4 {
        s.queue_capture(i, &planes).unwrap();
    }
    s.stream_on().unwrap();
    let want = Ready {
        capture: true,
        ..Ready::default()
    };
    let ((), allocated) = allocations(|| {
        for n in 0..1000u32 {
            mock.with(|d| d.produce(n % 4));
            assert!(s.wait(want, None).unwrap().capture);
            let done = s.dequeue_capture().unwrap().unwrap();
            assert_eq!(done.index, n % 4);
            assert!(s.capture_plane(done.index, 1).is_some());
            s.queue_capture(done.index, &planes).unwrap();
        }
    });
    assert_eq!(allocated, 0, "wait, dequeue, read and queue");
}
