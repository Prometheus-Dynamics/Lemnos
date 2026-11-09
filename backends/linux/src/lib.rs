//! Linux hardware for Lemnos, by the names a board file uses: `i2c-N`, `spidevB.C`,
//! `gpiochipN:LINE`, `pwmchipN:CHANNEL`, and a tty such as `ttyAMA0`.

#![cfg(target_os = "linux")]

mod gpio;
mod i2c;
mod pwm;
mod spi;
mod uart;

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use lemnos_core::{
    I2c, Line, LocalCaller, Provider, Pwm, RawSpiBus, Result, Spi, Status, Tag, Uart, status_of,
};
use lemnos_hal::ErrorKind;

pub use gpio::GpioLine;
pub use i2c::I2cBus;
pub use pwm::SysfsPwm;
pub use spi::{SpiMode, Spidev};
pub use uart::TtyUart;

/// The hardware of the machine this runs on.
#[derive(Clone)]
pub struct LinuxBoard {
    dev: PathBuf,
    sys: PathBuf,
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
        Self::at("/dev", "/sys")
    }
}

impl LinuxBoard {
    pub fn new() -> Self {
        Self::default()
    }

    fn at(dev: impl Into<PathBuf>, sys: impl Into<PathBuf>) -> Self {
        Self {
            dev: dev.into(),
            sys: sys.into(),
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

    /// The line a name like `gpiochip0:17` means.
    fn line_node(&self, need: &str) -> Option<(PathBuf, u32)> {
        let (chip, offset) = split(need)?;
        Some((self.node(chip, GPIO)?, offset))
    }

    /// The chip directory and channel a name like `pwmchip0:1` means.
    fn pwm_node(&self, need: &str) -> Option<(PathBuf, u32)> {
        let (chip, channel) = split(need)?;
        let valid = chip.len() > PWM.len() && chip.starts_with(PWM) && !chip.contains('/');
        valid.then(|| (self.sys.join("class/pwm").join(chip), channel))
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
const GPIO: &str = "gpiochip";
const PWM: &str = "pwmchip";
const TTY: &str = "tty";

fn split(need: &str) -> Option<(&str, u32)> {
    let (name, number) = need.split_once(':')?;
    Some((name, number.parse().ok()?))
}

fn exists(path: Option<PathBuf>) -> bool {
    path.as_deref().is_some_and(Path::exists)
}

impl Provider for LinuxBoard {
    fn provided(&self, need: &str) -> bool {
        exists(self.node(need, I2C))
            || exists(self.node(need, SPI))
            || exists(self.node(need, TTY))
            || exists(self.line_node(need).map(|(path, _)| path))
            || exists(self.pwm_node(need).map(|(path, _)| path))
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

    fn line(&mut self, need: &str) -> Result<Line> {
        // The kernel refuses a line that's held, so this needs no claim of its own.
        let (path, offset) = self.line_node(need).ok_or(Status::NOT_FOUND)?;
        let line = GpioLine::open(&path, offset).map_err(|e| open_status(&e))?;
        Ok(Box::new(line))
    }

    fn pwm(&mut self, need: &str) -> Result<Pwm> {
        let (chip, channel) = self.pwm_node(need).ok_or(Status::NOT_FOUND)?;
        let claim = self.claim(need)?;
        let pwm = SysfsPwm::open(&chip, channel).map_err(|e| open_status(&e))?;
        Ok(Box::new(pwm.holding(claim)))
    }

    fn uart(&mut self, need: &str) -> Result<Uart> {
        let path = self.node(need, TTY).ok_or(Status::NOT_FOUND)?;
        let claim = self.claim(need)?;
        let uart = TtyUart::open_path(&path).map_err(|e| open_status(&e))?;
        Ok(Box::new(uart.holding(claim)))
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
        (LinuxBoard::at(&dir, &dir), dir)
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
    fn line_and_channel_names_are_split() {
        let (board, dir) = temp_board("split");
        assert_eq!(
            board.line_node("gpiochip0:17"),
            Some((dir.join("gpiochip0"), 17))
        );
        assert_eq!(board.line_node("gpiochip0"), None);
        assert_eq!(board.line_node("gpiochip0:x"), None);
        assert_eq!(board.line_node("gpiochip/../x:1"), None);
        assert_eq!(
            board.pwm_node("pwmchip2:1"),
            Some((dir.join("class/pwm/pwmchip2"), 1))
        );
        assert_eq!(board.pwm_node("pwmchip/x:1"), None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_pwm_channel_has_one_owner_until_it_is_dropped() {
        let (mut board, dir) = temp_board("pwm");
        let root = dir.join("class/pwm/pwmchip0/pwm1");
        std::fs::create_dir_all(&root).unwrap();
        for name in ["period", "duty_cycle", "polarity", "enable"] {
            std::fs::write(root.join(name), "0\n").unwrap();
        }
        assert!(board.provided("pwmchip0:1"));
        let first = board.pwm("pwmchip0:1").map_err(|_| ()).unwrap();
        assert_eq!(
            board.pwm("pwmchip0:1").map(|_| ()).unwrap_err(),
            Status::BUSY
        );
        drop(first);
        assert!(board.pwm("pwmchip0:1").is_ok());
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
