//! A serial port through a tty.

use std::fs::{File, OpenOptions};
use std::io::{self, ErrorKind as IoKind, Read, Write};
use std::os::fd::AsFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::time::Duration;

use lemnos_hal::{ErrorKind, Uart};
use lemnos_linux_sys::poll;
use lemnos_linux_sys::serial;

use crate::{Claim, kind_of};

/// The rate a port opens at, until the driver sets one.
const OPEN_BAUD: u32 = 115_200;
/// How long a write waits for room before it reports that none went out.
const WRITE_WAIT: Duration = Duration::from_millis(100);

/// A raw 8N1 port, read and written without blocking the thread for longer than asked.
pub struct TtyUart {
    file: File,
    _claim: Option<Claim>,
}

impl TtyUart {
    pub fn open_path(path: impl AsRef<Path>) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOCTTY | libc::O_NONBLOCK)
            .open(path)?;
        serial::configure(file.as_fd(), OPEN_BAUD)?;
        Ok(Self { file, _claim: None })
    }

    pub(crate) fn holding(mut self, claim: Claim) -> Self {
        self._claim = Some(claim);
        self
    }

    fn write_some(&mut self, data: &[u8]) -> io::Result<usize> {
        match self.file.write(data) {
            Err(e) if e.kind() == IoKind::WouldBlock => {}
            done => return done,
        }
        if poll::poll_one(self.file.as_fd(), poll::POLLOUT, Some(WRITE_WAIT))? == 0 {
            return Ok(0);
        }
        match self.file.write(data) {
            Err(e) if e.kind() == IoKind::WouldBlock => Ok(0),
            done => done,
        }
    }

    fn read_some(&mut self, out: &mut [u8], timeout: Duration) -> io::Result<usize> {
        let ready = poll::poll_one(self.file.as_fd(), poll::POLLIN, Some(timeout))?;
        if ready == 0 {
            return Ok(0);
        }
        match self.file.read(out) {
            Err(e) if e.kind() == IoKind::WouldBlock => Ok(0),
            // Readable with nothing to read, and the far end is gone: an unplugged port.
            Ok(0) if ready & poll::POLLHUP != 0 => Err(IoKind::NotConnected.into()),
            done => done,
        }
    }
}

impl Uart for TtyUart {
    type Error = ErrorKind;

    fn set_baud(&mut self, baud: u32) -> Result<(), ErrorKind> {
        serial::configure(self.file.as_fd(), baud).map_err(|e| kind_of(&e))
    }

    fn write(&mut self, data: &[u8]) -> Result<usize, ErrorKind> {
        self.write_some(data).map_err(|e| kind_of(&e))
    }

    fn read(&mut self, out: &mut [u8], timeout: Duration) -> Result<usize, ErrorKind> {
        self.read_some(out, timeout).map_err(|e| kind_of(&e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lemnos_linux_sys::test_pty::pair;

    const SHORT: Duration = Duration::from_millis(5);
    const LONG: Duration = Duration::from_secs(5);

    #[test]
    fn bytes_go_both_ways() {
        let (mut controller, path) = pair();
        let mut port = TtyUart::open_path(path).unwrap();
        assert_eq!(Uart::write(&mut port, &[0xaa, 0x55]), Ok(2));
        let mut got = [0; 2];
        controller.read_exact(&mut got).unwrap();
        assert_eq!(got, [0xaa, 0x55]);
        controller.write_all(&[0x48, 0x69]).unwrap();
        let mut got = [0; 8];
        let n = Uart::read(&mut port, &mut got, LONG).unwrap();
        assert_eq!(&got[..n], &[0x48, 0x69]);
    }

    #[test]
    fn a_read_with_nothing_there_returns_nothing_when_the_time_is_up() {
        let (_controller, path) = pair();
        let mut port = TtyUart::open_path(path).unwrap();
        assert_eq!(Uart::read(&mut port, &mut [0; 4], SHORT), Ok(0));
    }

    #[test]
    fn the_rate_can_change() {
        let (_controller, path) = pair();
        let mut port = TtyUart::open_path(path).unwrap();
        assert_eq!(port.set_baud(9600), Ok(()));
        assert_eq!(port.set_baud(123_456), Err(ErrorKind::InvalidInput));
    }

    #[test]
    fn opening_a_missing_port_fails_cleanly() {
        let error = TtyUart::open_path("/dev/ttyDoesNotExist").err().unwrap();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn something_that_is_not_a_port_is_refused() {
        assert!(TtyUart::open_path("/dev/null").is_err());
    }
}
