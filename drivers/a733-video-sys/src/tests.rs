//! The layouts and numbers, and the node on a plain file: a file takes none of the requests
//! (the kernel answers `ENOTTY`), which shows a request reached the kernel, and it maps, which
//! shows what a read sees. No device node is opened.

use std::io::Write;
use std::mem::{align_of, offset_of, size_of};
use std::os::fd::AsFd;

use super::*;

fn file(name: &str, bytes: &[u8]) -> File {
    let path = std::env::temp_dir().join(format!(
        "lemnos-a733-video-sys-{}-{name}",
        std::process::id()
    ));
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .unwrap();
    file.write_all(bytes).unwrap();
    std::fs::remove_file(path).unwrap();
    file
}

fn window() -> Vec<u8> {
    (0..WINDOW_BYTES).map(|i| (i / 4) as u8).collect()
}

#[test]
fn the_layout_and_the_numbers_are_the_kernels() {
    assert_eq!(size_of::<DmaBufParam>(), 8);
    assert_eq!(align_of::<DmaBufParam>(), 4);
    assert_eq!(offset_of!(DmaBufParam, fd), 0);
    assert_eq!(offset_of!(DmaBufParam, phy_addr), 4);
    assert_eq!((MAP_DMA_BUF, UNMAP_DMA_BUF), (0x504, 0x505));
    let numbers = [
        (Plain::WaitDecoder, 0x102),
        (Plain::WaitEncoder, 0x103),
        (Plain::Reset, 0x104),
        (Plain::EngineRequest, 0x206),
        (Plain::EngineRelease, 0x207),
        (Plain::GetLock, 0x310),
        (Plain::ReleaseLock, 0x311),
        (Plain::Mode, 0x800),
        (Plain::WaitVcuEncoder, 0x801),
        (Plain::ClearEncoderInterrupt, 0x803),
        (Plain::TopRegisterOffset, 0x804),
        (Plain::WaitVcuDecoder, 0x805),
    ];
    for (request, number) in numbers {
        assert_eq!(request as u32, number, "{request:?}");
    }
    assert_eq!(WINDOW_BYTES, 0x1000);
}

#[test]
fn a_request_takes_only_the_values_the_kernel_does() {
    for request in [
        Plain::Reset,
        Plain::EngineRequest,
        Plain::EngineRelease,
        Plain::ClearEncoderInterrupt,
        Plain::TopRegisterOffset,
    ] {
        assert!(request.takes(0) && !request.takes(1), "{request:?}");
    }
    for request in [Plain::GetLock, Plain::ReleaseLock] {
        for value in 0..=8 {
            assert_eq!(request.takes(value), matches!(value, 1 | 2 | 4), "{value}");
        }
        assert!(!request.takes(u32::MAX));
    }
    assert!(Plain::Mode.takes(4) && !Plain::Mode.takes(5));
    for request in [
        Plain::WaitDecoder,
        Plain::WaitEncoder,
        Plain::WaitVcuEncoder,
        Plain::WaitVcuDecoder,
    ] {
        assert!(request.takes(0) && request.takes(MAX_WAIT_S), "{request:?}");
        assert!(!request.takes(MAX_WAIT_S + 1) && !request.takes(u32::MAX));
    }
}

#[test]
fn a_value_a_request_does_not_take_never_reaches_the_kernel() {
    let node = Node::on(file("values", &window())).unwrap();
    // Refused here: the error is this crate's, not the kernel's.
    for (request, value) in [
        (Plain::GetLock, 3),
        (Plain::GetLock, 0),
        (Plain::Mode, 5),
        (Plain::Reset, 1),
        (Plain::WaitEncoder, MAX_WAIT_S + 1),
    ] {
        let error = node.plain(request, value).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{request:?}");
        assert_eq!(error.raw_os_error(), None, "{request:?}");
    }
    // Taken here and issued: a plain file answers that it has no such request.
    for (request, value) in [
        (Plain::GetLock, 2),
        (Plain::EngineRequest, 0),
        (Plain::Mode, 1),
    ] {
        let error = node.plain(request, value).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::ENOTTY), "{request:?}");
    }
    let error = node.map_dma_buf(node.file.as_fd()).unwrap_err();
    assert_eq!(error.raw_os_error(), Some(libc::ENOTTY));
    let error = node.unmap_dma_buf(node.file.as_fd()).unwrap_err();
    assert_eq!(error.raw_os_error(), Some(libc::ENOTTY));
}

#[test]
fn a_read_is_one_aligned_register_inside_the_window() {
    let node = Node::on(file("window", &window())).unwrap();
    let at = |offset: usize| u32::from_ne_bytes([(offset / 4) as u8; 4]);
    assert_eq!(node.read(0), Some(at(0)));
    assert_eq!(node.read(0x800), Some(at(0x800)));
    assert_eq!(node.read(WINDOW_BYTES - 4), Some(at(WINDOW_BYTES - 4)));
    for offset in [
        1,
        2,
        3,
        WINDOW_BYTES - 3,
        WINDOW_BYTES,
        WINDOW_BYTES + 4,
        usize::MAX - 3,
        usize::MAX,
    ] {
        assert_eq!(node.read(offset), None, "{offset:#x}");
    }
    // A longer file changes nothing: only the window is mapped.
    let long: Vec<u8> = window().into_iter().chain([0xff; 4096]).collect();
    let node = Node::on(file("long", &long)).unwrap();
    assert_eq!(node.read(WINDOW_BYTES), None);
}

#[test]
fn a_node_that_is_not_there_is_an_error() {
    let error = Node::open(Path::new("/nonexistent/cedar_dev_ve2")).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::NotFound);
}
