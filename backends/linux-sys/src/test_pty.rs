//! A pseudo-terminal pair, so serial code can be tested with no port. Test support only.

use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::path::PathBuf;

/// The controlling side and the path of the port side.
pub fn pair() -> (File, PathBuf) {
    let controller = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/ptmx")
        .unwrap();
    let raw = controller.as_raw_fd();
    let mut unlock: libc::c_int = 0;
    // SAFETY: TIOCSPTLCK reads one int through the pointer to a live local.
    let unlocked = unsafe { libc::ioctl(raw, libc::TIOCSPTLCK, &mut unlock) };
    assert_eq!(unlocked, 0);
    let mut number: libc::c_uint = 0;
    // SAFETY: TIOCGPTN writes one unsigned int through the pointer to a live local.
    let named = unsafe { libc::ioctl(raw, libc::TIOCGPTN, &mut number) };
    assert_eq!(named, 0);
    (controller, PathBuf::from(format!("/dev/pts/{number}")))
}
