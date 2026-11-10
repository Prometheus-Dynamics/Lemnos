//! The pairing of requests and structures, the node check against a sysfs tree made for the
//! test, and the session's refusals against a regular file (where every request answers
//! `ENOTTY`). No device node is opened.

use std::io;
use std::mem::size_of;
use std::path::PathBuf;

use lemnos_driver_rknpu::uapi::{self, Action, MemCreate, MemDestroy, MemMap, Submit};
use lemnos_driver_rknpu::{Create, Device, Domain, Kind, Object, Query, Run, Session};

use crate::session::Request;
use crate::{NpuNode, NpuSession};

#[test]
fn each_structure_goes_with_its_request() {
    assert_eq!(<Action as Request>::NR, 0xc008_6440);
    assert_eq!(<Submit as Request>::NR, 0xc068_6441);
    assert_eq!(<MemCreate as Request>::NR, 0xc030_6442);
    assert_eq!(<MemMap as Request>::NR, 0xc010_6443);
    assert_eq!(<MemDestroy as Request>::NR, 0xc010_6444);
    // The size field of each number is its structure's size.
    for (nr, size) in [
        (<Action as Request>::NR, size_of::<Action>()),
        (<Submit as Request>::NR, size_of::<Submit>()),
        (<MemCreate as Request>::NR, size_of::<MemCreate>()),
        (<MemMap as Request>::NR, size_of::<MemMap>()),
        (<MemDestroy as Request>::NR, size_of::<MemDestroy>()),
    ] {
        assert_eq!(((nr >> 16) & 0x3fff) as usize, size);
    }
}

fn scratch(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("rknpu-sys-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    path
}

/// A sysfs tree in which `/dev/null`'s device (1:3) is bound to `driver`.
fn sysfs(name: &str, driver: &str) -> PathBuf {
    let root = scratch(name);
    let drivers = root.join("bus/platform/drivers").join(driver);
    std::fs::create_dir_all(&drivers).unwrap();
    let device = root.join("dev/char/1:3/device");
    std::fs::create_dir_all(&device).unwrap();
    std::os::unix::fs::symlink(&drivers, device.join("driver")).unwrap();
    root
}

#[test]
fn only_the_npu_drivers_character_device_is_taken() {
    // Stat only: /dev/null is never opened, because the check refuses it first or the node is
    // never asked to open.
    let other = sysfs("other", "rockchip-drm");
    let node = NpuNode::with_sysfs("/dev/null", &other);
    assert_eq!(node.check().unwrap_err().kind(), io::ErrorKind::Unsupported);
    let npu = sysfs("npu", uapi::DRIVER_NAME);
    assert!(NpuNode::with_sysfs("/dev/null", &npu).check().is_ok());
    let empty = scratch("empty");
    assert_eq!(
        NpuNode::with_sysfs("/dev/null", &empty)
            .check()
            .unwrap_err()
            .kind(),
        io::ErrorKind::Unsupported
    );
    // A regular file is no character device, and is refused before it is opened.
    let file = scratch("file").join("node");
    std::fs::write(&file, b"").unwrap();
    let mut node = NpuNode::with_sysfs(&file, &npu);
    assert_eq!(node.open().unwrap_err().kind(), io::ErrorKind::Unsupported);
    for dir in [other, npu, empty] {
        let _ = std::fs::remove_dir_all(dir);
    }
}

fn session() -> NpuSession {
    let file = scratch("session").join("file");
    std::fs::write(&file, b"").unwrap();
    NpuSession::open(&file).unwrap()
}

#[test]
fn a_file_that_is_not_the_node_answers_every_request_with_an_error() {
    let mut session = session();
    let enotty = Some(libc::ENOTTY);
    assert_eq!(
        session
            .query(Query::IommuEnabled)
            .unwrap_err()
            .raw_os_error(),
        enotty
    );
    let create = Create::new(Domain::new(1).unwrap(), 64, Kind::Body).unwrap();
    assert_eq!(session.create(create).unwrap_err().raw_os_error(), enotty);
    assert_eq!(session.objects(), 0);
}

#[test]
fn only_objects_of_the_session_are_named_to_the_kernel() {
    let mut session = session();
    // A body this session never made: refused before any call, so the kernel never follows an
    // address for it.
    let forged = Object {
        handle: 5,
        iova: 0x1000,
        bytes: 4096,
        size: 4096,
        domain: Domain::new(1).unwrap(),
        kind: Kind::Body,
    };
    let run = Run::new(&forged, 1, 0, false).unwrap();
    let error = session.submit(&run).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(error.raw_os_error(), None);
    assert_eq!(
        session.destroy(5).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    assert!(session.memory(5).is_none());
}
