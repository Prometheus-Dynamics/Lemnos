//! Serial ports through termios: raw mode, 8 data bits, no flow control.

use std::io;
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, BorrowedFd};

fn check(result: libc::c_int) -> io::Result<()> {
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// The termios constant for a standard rate.
fn speed(baud: u32) -> io::Result<libc::speed_t> {
    Ok(match baud {
        1200 => libc::B1200,
        2400 => libc::B2400,
        4800 => libc::B4800,
        9600 => libc::B9600,
        19_200 => libc::B19200,
        38_400 => libc::B38400,
        57_600 => libc::B57600,
        115_200 => libc::B115200,
        230_400 => libc::B230400,
        460_800 => libc::B460800,
        500_000 => libc::B500000,
        576_000 => libc::B576000,
        921_600 => libc::B921600,
        1_000_000 => libc::B1000000,
        1_152_000 => libc::B1152000,
        1_500_000 => libc::B1500000,
        2_000_000 => libc::B2000000,
        2_500_000 => libc::B2500000,
        3_000_000 => libc::B3000000,
        3_500_000 => libc::B3500000,
        4_000_000 => libc::B4000000,
        _ => return Err(io::ErrorKind::InvalidInput.into()),
    })
}

/// A frame's parity bit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Parity {
    /// No parity bit.
    None,
    /// The parity bit makes the count of ones even.
    Even,
    /// The parity bit makes the count of ones odd.
    Odd,
}

/// Puts the port in raw mode at `baud`, 8 data bits with `parity` and one stop bit, or two with
/// `two_stop_bits`: reads return what's there without waiting.
pub fn configure(
    fd: BorrowedFd<'_>,
    baud: u32,
    parity: Parity,
    two_stop_bits: bool,
) -> io::Result<()> {
    let speed = speed(baud)?;
    let raw = fd.as_raw_fd();
    let mut settings = MaybeUninit::<libc::termios>::zeroed();
    // SAFETY: `settings` is a live, writable `termios`, and `raw` is open for the borrow of `fd`.
    check(unsafe { libc::tcgetattr(raw, settings.as_mut_ptr()) })?;
    // SAFETY: `tcgetattr` succeeded, so it filled in `settings`.
    let mut settings = unsafe { settings.assume_init() };
    // SAFETY: `settings` is a live `termios` that this only rewrites.
    unsafe { libc::cfmakeraw(&mut settings) };
    settings.c_cflag |= libc::CLOCAL | libc::CREAD;
    settings.c_cflag &= !(libc::CSTOPB | libc::PARENB | libc::PARODD | libc::CRTSCTS);
    settings.c_cflag |= match parity {
        Parity::None => 0,
        Parity::Even => libc::PARENB,
        Parity::Odd => libc::PARENB | libc::PARODD,
    };
    if two_stop_bits {
        settings.c_cflag |= libc::CSTOPB;
    }
    settings.c_cc[libc::VMIN] = 0;
    settings.c_cc[libc::VTIME] = 0;
    // SAFETY: `settings` is a live `termios` and `speed` came from the constants above.
    check(unsafe { libc::cfsetspeed(&mut settings, speed) })?;
    // SAFETY: `settings` is a live, initialised `termios` the call only reads.
    check(unsafe { libc::tcsetattr(raw, libc::TCSANOW, &settings) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_pty::pair;
    use std::os::fd::AsFd;

    #[test]
    fn a_standard_rate_is_accepted_and_others_are_not() {
        let (_controller, path) = pair();
        let port = std::fs::File::open(path).unwrap();
        configure(port.as_fd(), 115_200, Parity::None, false).unwrap();
        configure(port.as_fd(), 1_000_000, Parity::None, false).unwrap();
        let error = configure(port.as_fd(), 123_456, Parity::None, false).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn something_that_is_not_a_tty_is_refused() {
        let file = std::fs::File::open("/dev/null").unwrap();
        assert!(configure(file.as_fd(), 9600, Parity::None, false).is_err());
    }
}
