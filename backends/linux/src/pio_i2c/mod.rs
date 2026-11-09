//! I2C on PIO: a bit-banged I2C master on the RP1 PIO block, for pins that no
//! hardware I2C controller drives. A board file names it `pio-i2c:sda=N,scl=N`.
//!
//! Every handle on the same pins and device node shares one state machine, and
//! transactions on it are serialized. The bus ends when its last handle drops.

mod bus;
mod device;
mod program;
mod protocol;
mod selector;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, Weak};

use lemnos_core::Status;
use lemnos_hal::ErrorKind;
use lemnos_hal::i2c::{ErrorType, I2c, Operation};
use lemnos_linux_sys::rp1_pio::PioDevice;

use crate::open_status;
use bus::PioBus;
use selector::Pins;

/// Whether a board-file name is a PIO I2C bus.
pub(crate) fn names(need: &str) -> bool {
    selector::is_selector(need)
}

/// Whether `need` names a well-formed PIO I2C bus whose device node exists under `dev`.
pub(crate) fn provided(dev: &Path, need: &str) -> bool {
    matches!(selector::parse(need), Some(Ok(_))) && dev.join(device::DEVNODE).exists()
}

type Registry = Mutex<HashMap<(PathBuf, Pins), Weak<PioBus<PioDevice>>>>;

fn registry() -> &'static Registry {
    static BUSES: OnceLock<Registry> = OnceLock::new();
    BUSES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// An I2C master on the RP1 PIO block, as a HAL bus.
pub(crate) struct PioI2c {
    bus: Arc<PioBus<PioDevice>>,
}

impl ErrorType for PioI2c {
    type Error = ErrorKind;
}

impl I2c for PioI2c {
    fn transaction(
        &mut self,
        address: u8,
        operations: &mut [Operation<'_>],
    ) -> Result<(), ErrorKind> {
        self.bus.run_operations(address, operations)
    }
}

/// The PIO bus a board-file name `pio-i2c:sda=N,scl=N[,hz=N]` means, on the PIO device
/// under `dev`. A malformed name is refused as an invalid argument; a name that isn't a
/// PIO bus at all is not found.
pub(crate) fn open(dev: &Path, need: &str) -> Result<PioI2c, Status> {
    let pins = match selector::parse(need) {
        None => return Err(Status::NOT_FOUND),
        Some(Err(_)) => return Err(Status::INVALID_ARGUMENT),
        Some(Ok(pins)) => pins,
    };
    let devnode = dev.join(device::DEVNODE);
    let key = (devnode.clone(), pins);
    let mut buses = registry().lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(bus) = buses.get(&key).and_then(Weak::upgrade) {
        return Ok(PioI2c { bus });
    }
    let bus = Arc::new(device::open_bus(&devnode, pins).map_err(|e| open_status(&e))?);
    buses.insert(key, Arc::downgrade(&bus));
    Ok(PioI2c { bus })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dev(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pio-i2c-test-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_selector_is_provided_only_when_well_formed_and_the_node_exists() {
        let dir = temp_dev("provided");
        assert!(names("pio-i2c:sda=8,scl=7"));
        assert!(!provided(&dir, "pio-i2c:sda=8,scl=7"));
        std::fs::write(dir.join(device::DEVNODE), b"").unwrap();
        assert!(provided(&dir, "pio-i2c:sda=8,scl=7"));
        assert!(!provided(&dir, "pio-i2c:sda=8,scl=8"));
        assert!(!provided(&dir, "pio-i2c:sda=8"));
        assert!(!provided(&dir, "i2c-1"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_missing_device_node_is_no_device_and_a_bad_name_is_refused() {
        let dir = temp_dev("open");
        assert_eq!(
            open(&dir, "pio-i2c:sda=8,scl=7").err(),
            Some(Status::NO_DEVICE)
        );
        assert_eq!(
            open(&dir, "pio-i2c:sda=8,scl=8").err(),
            Some(Status::INVALID_ARGUMENT)
        );
        assert_eq!(open(&dir, "i2c-1").err(), Some(Status::NOT_FOUND));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
