//! A view that maps none of the buffers' memory: it checks the descriptors and the pool page, keeps the
//! descriptors, and gives no bytes. The process holds one descriptor per buffer it was passed, and no
//! mapping of any buffer.

use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::sync::Mutex;

use shm_ring::{RingError, sealed_memfd};
use shm_ring_buffers::{BufferDesc, BufferIndex, BytesError, Pool, PoolView};

/// The tests of this file run in one process. The descriptor count is process-wide, so the tests
/// take turns: a descriptor another test opens must not look like one this test leaked.
static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn open_fds() -> usize {
    std::fs::read_dir("/proc/self/fd")
        .expect("the descriptor table")
        .count()
}

fn inode(fd: BorrowedFd<'_>) -> u64 {
    std::fs::File::from(fd.try_clone_to_owned().unwrap())
        .metadata()
        .map(|meta| std::os::unix::fs::MetadataExt::ino(&meta))
        .unwrap()
}

/// Whether this process maps the file with `ino` anywhere, from `/proc/self/maps`.
fn maps(ino: u64) -> bool {
    std::fs::read_to_string("/proc/self/maps")
        .unwrap()
        .lines()
        .any(|line| {
            line.split_whitespace()
                .nth(4)
                .and_then(|n| n.parse::<u64>().ok())
                == Some(ino)
        })
}

/// The pool's page and descriptors, cloned out of a pool that is then dropped, so that only the
/// descriptors are left to the view.
fn pieces(count: u32, size: usize) -> (OwnedFd, Vec<OwnedFd>, Vec<BufferDesc>) {
    let pool = Pool::in_memory(1, count, size).unwrap();
    let page = pool.page_fd().unwrap();
    let fds: Vec<OwnedFd> = pool
        .descriptors()
        .map(|fd| fd.try_clone_to_owned().unwrap())
        .collect();
    let buffers = pool.buffers().to_vec();
    drop(pool);
    (page, fds, buffers)
}

#[test]
fn an_unmapped_view_maps_no_buffer_and_opens_no_descriptor() {
    let _serial = serial();
    let (page, fds, buffers) = pieces(2, 4096);
    let buffer_ino = inode(fds[0].as_fd());
    let second_ino = inode(fds[usize::from(buffers[1].descriptor)].as_fd());
    assert!(
        !maps(buffer_ino),
        "the publisher's own mapping is gone once the pool is dropped"
    );
    let before = open_fds();
    let view = PoolView::attach_unmapped(page, fds, &buffers).expect("attaches");
    assert!(
        open_fds() <= before,
        "attaching opened no descriptor: before {before}, after {}",
        open_fds()
    );
    assert!(!view.is_mapped());
    assert!(!maps(buffer_ino), "no mapping of the buffer's memory");
    // Each buffer still names its descriptor, so it can be handed on.
    let (fd, desc) = view.descriptor(BufferIndex(1)).expect("buffer 1");
    assert_eq!(
        inode(fd),
        second_ino,
        "buffer 1's descriptor is the one it was passed"
    );
    assert_eq!(desc, buffers[1]);
}

#[test]
fn byte_access_on_an_unmapped_view_is_a_named_error() {
    let _serial = serial();
    let (page, fds, buffers) = pieces(2, 4096);
    let mut view = PoolView::attach_unmapped(page, fds, &buffers).expect("attaches");
    assert_eq!(
        view.read_bytes(BufferIndex(0)).unwrap_err(),
        BytesError::Unmapped
    );
    assert_eq!(
        view.write_bytes(BufferIndex(0)).unwrap_err(),
        BytesError::Unmapped
    );
    assert!(view.bytes(BufferIndex(0)).is_none());
    assert!(view.bytes_mut(BufferIndex(0)).is_none());
    // A buffer outside the pool is still out of the pool, not unmapped.
    assert_eq!(
        view.read_bytes(BufferIndex(9)).unwrap_err(),
        BytesError::Unmapped
    );
}

#[test]
fn a_mapped_view_of_the_same_pool_gives_its_bytes() {
    let _serial = serial();
    let (page, fds, buffers) = pieces(2, 4096);
    let view = PoolView::attach(page, fds, &buffers).expect("attaches");
    assert!(view.is_mapped());
    assert_eq!(view.read_bytes(BufferIndex(0)).expect("bytes").len(), 4096);
    assert_eq!(
        view.read_bytes(BufferIndex(7)).unwrap_err(),
        BytesError::OutOfPool
    );
}

#[test]
fn an_unmapped_view_refuses_the_descriptors_a_mapped_one_refuses() {
    let _serial = serial();
    // A regular file is not sealed against shrinking.
    let path = std::env::temp_dir().join(format!("shm-unmapped-{}", std::process::id()));
    let file = std::fs::File::create(&path).unwrap();
    file.set_len(8192).unwrap();
    let unsealed: OwnedFd = file.into();
    let (page, _, buffers) = pieces(2, 4096);
    let refused = PoolView::attach_unmapped(
        page.try_clone().unwrap(),
        vec![unsealed.try_clone().unwrap(), sealed_memfd(8192).unwrap()],
        &buffers,
    );
    assert!(matches!(refused, Err(RingError::Unsealed)), "{refused:?}");
    let _ = std::fs::remove_file(&path);

    // A buffer past the end of its descriptor.
    let (page, fds, _) = pieces(2, 4096);
    let past = [
        BufferDesc {
            descriptor: 0,
            offset: 4096,
            size: 8192,
        },
        buffers[1],
    ];
    let refused = PoolView::attach_unmapped(page, fds, &past);
    assert!(
        matches!(refused, Err(RingError::Malformed(_))),
        "{refused:?}"
    );

    // The page is for another pool.
    let (page, fds, buffers) = pieces(2, 4096);
    let one = fds.into_iter().next().unwrap();
    let refused = PoolView::attach_unmapped(page, vec![one], &buffers[..1]);
    assert!(
        matches!(refused, Err(RingError::Malformed(_))),
        "{refused:?}"
    );
}
