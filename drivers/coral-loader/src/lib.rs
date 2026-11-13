//! Downloads a firmware image to a USB device over DFU 1.1, in blocks of the device's own
//! `wTransferSize`.
//!
//! Settings come from the device's `config` in the board file:
//!
//! - `firmware` (string, required): the absolute path of the image.
//!
//! A DFU download ends in a USB reset, which is what starts the firmware. The driver runs the
//! download until the device has manifested, then resets it, once. The device then leaves the
//! bus and comes back under the identity its firmware gives it, and the handle this driver holds
//! is dead from that moment: the driver sends nothing more, and it cannot see what the device
//! came back as. Its health is `Ok` only when the device confirmed the download and the reset
//! took it off the bus. A device that stopped answering while it manifested, or that came back
//! from the reset as the device it was, is reported as `Degraded` with the reason. Binding the
//! device's new identity to another driver is the board file's and the host's business.
//!
//! The image is read once, when the device binds, and held in memory until the download ends. The
//! driver never looks inside it. A file that is missing, unreadable, empty or larger than
//! [`MAX_IMAGE_BYTES`] fails the bind, and the host retries the bind. Binding touches no USB
//! device.
//!
//! The download runs in [`Lifecycle::tick`], at most one control transfer per tick, and the driver
//! never sleeps. Waits the device asks for become the time of the next tick, up to a limit, since
//! `bwPollTimeout` can claim hours.
//!
//! A device that leaves before the download has ended (unplugged, say) leaves the handle dead
//! too. The driver's health is then [`Health::Gone`], which has the host drop it and bind it
//! again under the failure rule: the device is opened afresh by its board name once it is back,
//! and the download starts over.
//!
//! Everything the device says is untrusted. A short reply, a bad descriptor, an error state, a
//! device that stops answering or never stops being busy: each ends this attempt, the driver backs
//! off (at most 8 s) and starts again from block 0. A reset that was not carried out is made
//! again after the same backoff, without another download. It never gives up and never switches
//! itself off. The driver keeps nothing across restarts: each start reads what state the device is in and
//! brings it back to idle first.

mod dfu;
mod loader;

#[cfg(test)]
mod mock;
#[cfg(test)]
mod tests;

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::Path;

use lemnos_core::{
    Config, ConfigError, Health, Key, Kind, Lifecycle, Result, Status, UsbDeviceHandle, driver,
};
use loader::{Ended, GONE, IN_PROGRESS, Loader, UNCHANGED, UNCONFIRMED};

/// The largest image accepted. A DFU image is a boot-time payload, not a filesystem.
pub const MAX_IMAGE_BYTES: u64 = 16 * 1024 * 1024;

/// Every setting the driver reads from the board file.
const KEYS: &[Key] = &[Key::required("firmware", Kind::Str)];

/// The method a description item points at when it only states something. This driver provides no
/// contract, so no method answers to it.

#[driver("coral-loader", provides = [])]
pub struct CoralLoader {
    #[needs]
    usb: UsbDeviceHandle,
    loader: Loader,
}

impl Lifecycle for CoralLoader {
    type Needs = CoralLoaderNeeds;

    const TICKS: bool = true;

    fn check_config<'a>(cfg: &Config<'a>) -> core::result::Result<(), ConfigError<'a>> {
        cfg.check_keys(KEYS)?;
        match cfg.str("firmware") {
            Some(path) if !Path::new(path).is_absolute() => {
                Err(ConfigError::new("firmware", "must be an absolute path"))
            }
            _ => Ok(()),
        }
    }

    fn start(needs: CoralLoaderNeeds, cfg: &Config<'_>) -> Result<Self> {
        let path = cfg.str("firmware").ok_or(Status::INVALID_ARGUMENT)?;
        let image = read_image(Path::new(path))?;
        Ok(Self {
            usb: needs.usb,
            loader: Loader::new(image),
        })
    }

    fn health(&self) -> Health {
        match (self.loader.ended(), self.loader.fault()) {
            (Some(Ended::Loaded), _) => Health::Ok,
            (Some(Ended::Unconfirmed), _) => Health::Degraded(UNCONFIRMED),
            (Some(Ended::Unchanged), _) => Health::Degraded(UNCHANGED),
            // The handle is dead before the download ended: only a new bind opens the device
            // again, and nothing calls this driver, so it has to ask.
            (None, Some(GONE)) => Health::Gone(GONE),
            (None, Some(fault)) => Health::Failed(fault),
            (None, None) => Health::Degraded(IN_PROGRESS),
        }
    }

    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        self.loader.tick(&mut *self.usb, now_ns)
    }
}

/// Reads the image, without ever holding more than [`MAX_IMAGE_BYTES`] of it. The path is
/// absolute by `check_config`.
///
/// The path must be a regular file: opening a FIFO would block this bind forever.
fn read_image(path: &Path) -> Result<Vec<u8>> {
    let metadata = fs::metadata(path).map_err(|error| io_status(&error))?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_IMAGE_BYTES {
        return Err(Status::INVALID_ARGUMENT);
    }
    let file = File::open(path).map_err(|error| io_status(&error))?;
    let mut image = Vec::new();
    file.take(MAX_IMAGE_BYTES + 1)
        .read_to_end(&mut image)
        .map_err(|error| io_status(&error))?;
    // The file can change between the two looks at it.
    if image.is_empty() || image.len() as u64 > MAX_IMAGE_BYTES {
        return Err(Status::INVALID_ARGUMENT);
    }
    Ok(image)
}

fn io_status(error: &io::Error) -> Status {
    match error.kind() {
        io::ErrorKind::NotFound => Status::NOT_FOUND,
        io::ErrorKind::PermissionDenied => Status::ACCESS,
        _ => Status::IO,
    }
}
