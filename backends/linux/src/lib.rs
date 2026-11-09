//! Linux hardware for Lemnos, by the names a board file uses. This module has what is on `/dev`:
//! `i2c-N` is `/dev/i2c-N` and `spidevB.C` is `/dev/spidevB.C`.

#![cfg(target_os = "linux")]

mod i2c;
mod spi;

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use lemnos_core::{I2c, LocalCaller, Provider, RawSpiBus, Result, Spi, Status, Tag, status_of};
use lemnos_hal::ErrorKind;

pub use i2c::I2cBus;
pub use spi::{SpiMode, Spidev};

/// The hardware of the machine this runs on.
#[derive(Clone)]
pub struct LinuxBoard {
    dev: PathBuf,
    claimed: Claims,
}

type Claims = Arc<Mutex<BTreeSet<String>>>;

/// One device's hold on a node nothing else may share. Dropping it lets go.
pub(crate) struct Claim {
    claimed: Claims,
    node: String,
}

impl Drop for Claim {
    fn drop(&mut self) {
        self.claimed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.node);
    }
}

impl Default for LinuxBoard {
    fn default() -> Self {
        Self::at("/dev")
    }
}

impl LinuxBoard {
    pub fn new() -> Self {
        Self::default()
    }

    fn at(dev: impl Into<PathBuf>) -> Self {
        Self {
            dev: dev.into(),
            claimed: Claims::default(),
        }
    }

    /// The node a board file's name means, if the name is one of ours.
    fn node(&self, need: &str, prefix: &str) -> Option<PathBuf> {
        (need.len() > prefix.len() && need.starts_with(prefix) && !need.contains('/'))
            .then(|| self.dev.join(need))
    }

    fn claim(&self, need: &str) -> Result<Claim> {
        let mut claimed = self.claimed.lock().unwrap_or_else(PoisonError::into_inner);
        if !claimed.insert(need.to_string()) {
            return Err(Status::BUSY);
        }
        Ok(Claim {
            claimed: Arc::clone(&self.claimed),
            node: need.to_string(),
        })
    }

    fn open_spi(&self, need: &str) -> Result<Spidev> {
        let path = self.node(need, SPI).ok_or(Status::NOT_FOUND)?;
        let claim = self.claim(need)?;
        let device = Spidev::open_path(&path).map_err(|e| open_status(&e))?;
        Ok(device.holding(claim))
    }
}

const I2C: &str = "i2c-";
const SPI: &str = "spidev";

fn exists(path: Option<PathBuf>) -> bool {
    path.as_deref().is_some_and(Path::exists)
}

impl Provider for LinuxBoard {
    fn provided(&self, need: &str) -> bool {
        exists(self.node(need, I2C)) || exists(self.node(need, SPI))
    }

    fn device(&mut self, _need: &str, _tag: Tag) -> Result<LocalCaller> {
        // Devices are the host's to wire. A board only has hardware.
        Err(Status::NOT_SUPPORTED)
    }

    fn i2c(&mut self, need: &str) -> Result<I2c> {
        // Every device on a bus opens it: the kernel serialises their transfers.
        let path = self.node(need, I2C).ok_or(Status::NOT_FOUND)?;
        let bus = I2cBus::open_path(&path).map_err(|e| open_status(&e))?;
        Ok(I2c(Box::new(bus)))
    }

    fn spi(&mut self, need: &str) -> Result<Spi> {
        Ok(Spi(Box::new(self.open_spi(need)?)))
    }

    fn raw_spi(&mut self, need: &str) -> Result<RawSpiBus> {
        Ok(Box::new(self.open_spi(need)?))
    }
}

/// What a failed I/O call amounts to.
pub(crate) fn kind_of(error: &io::Error) -> ErrorKind {
    error
        .raw_os_error()
        .map_or_else(|| ErrorKind::from_io(error.kind()), ErrorKind::from_errno)
}

/// Why a node couldn't be opened. `ENXIO` here is a node with no device behind it, where during
/// a transfer it would be a target that didn't answer.
fn open_status(error: &io::Error) -> Status {
    match error.raw_os_error() {
        Some(libc::ENOENT | libc::ENXIO | libc::ENODEV) => Status::NO_DEVICE,
        _ => status_of(kind_of(error)),
    }
}

#[cfg(test)]
#[global_allocator]
static ALLOCATOR: lemnos_measure::test_alloc::Counting = lemnos_measure::test_alloc::Counting;

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_board(name: &str) -> (LinuxBoard, PathBuf) {
        let dir = std::env::temp_dir().join(format!("lemnos-linux-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        (LinuxBoard::at(&dir), dir)
    }

    #[test]
    fn names_stay_inside_dev() {
        let (board, dir) = temp_board("names");
        assert!(board.node("i2c-1", I2C).is_some());
        assert!(board.node("spidev0.1", SPI).is_some());
        assert!(board.node("i2c-", I2C).is_none());
        assert!(board.node("i2c-../../etc/passwd", I2C).is_none());
        assert!(board.node("spi0", SPI).is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_node_is_provided_only_when_it_exists() {
        let (board, dir) = temp_board("provided");
        assert!(!board.provided("spidev0.0"));
        std::fs::write(dir.join("spidev0.0"), b"").unwrap();
        assert!(board.provided("spidev0.0"));
        assert!(!board.provided("pio-i2c:sda=8,scl=7"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_spi_device_has_one_owner_until_it_is_dropped() {
        let (mut board, dir) = temp_board("claim");
        std::fs::write(dir.join("spidev0.0"), b"").unwrap();
        let first = board.open_spi("spidev0.0").unwrap();
        assert_eq!(
            board.raw_spi("spidev0.0").map(|_| ()).unwrap_err(),
            Status::BUSY
        );
        drop(first);
        assert!(board.open_spi("spidev0.0").is_ok());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_missing_node_is_no_device() {
        let (mut board, dir) = temp_board("missing");
        assert_eq!(
            board.i2c("i2c-99").map(|_| ()).unwrap_err(),
            Status::NO_DEVICE
        );
        assert_eq!(
            board.spi("spidev9.9").map(|_| ()).unwrap_err(),
            Status::NO_DEVICE
        );
        // A failed open doesn't keep the claim.
        assert_eq!(
            board.spi("spidev9.9").map(|_| ()).unwrap_err(),
            Status::NO_DEVICE
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn open_errors_say_why() {
        let status = |errno| open_status(&io::Error::from_raw_os_error(errno));
        assert_eq!(status(libc::EBUSY), Status::BUSY);
        assert_eq!(status(libc::EACCES), Status::ACCESS);
        assert_eq!(status(libc::ENXIO), Status::NO_DEVICE);
    }
}
