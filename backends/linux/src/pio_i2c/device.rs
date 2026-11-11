//! The kernel seam of the PIO I2C bus: the RP1 PIO operations the bus needs, as a
//! trait so the protocol tests can run on a fake block, and the real device
//! behind it.

use std::io;
use std::os::fd::AsFd;
use std::path::Path;

use lemnos_linux_sys::gpio;
use lemnos_linux_sys::rp1_pio::{PioDevice, SmConfig};

use super::bus::PioBus;
use super::selector::Pins;

/// The PIO operations a bus runs. Each method is one request to the device; the
/// bus owns the sequencing.
pub(crate) trait Pio: Send + Sync + 'static {
    fn claim_any_sm(&self) -> io::Result<u16>;
    fn unclaim_sms(&self, mask: u16) -> io::Result<()>;
    fn add_program(&self, origin: u16, instrs: &[u16]) -> io::Result<u16>;
    fn remove_program(&self, origin: u16, len: u16) -> io::Result<()>;
    fn sm_init(&self, sm: u16, initial_pc: u16, config: SmConfig) -> io::Result<()>;
    fn sm_set_pindirs(&self, sm: u16, dirs: u32, mask: u32) -> io::Result<()>;
    fn sm_set_enabled(&self, mask: u16, enable: bool) -> io::Result<()>;
    fn sm_clear_fifos(&self, sm: u16) -> io::Result<()>;
    fn config_xfer32(&self, sm: u16, dir: u16, buf_size: u32, buf_count: u32) -> io::Result<()>;
    fn xfer_to_sm(&self, sm: u16, words: &[u32]) -> io::Result<()>;
    fn xfer_from_sm(&self, sm: u16, words: &mut [u32]) -> io::Result<()>;
    fn gpio_init(&self, gpio: u16) -> io::Result<()>;
    fn gpio_set_function(&self, gpio: u16, func: u16) -> io::Result<()>;
    fn gpio_set_input_enabled(&self, gpio: u16, enabled: bool) -> io::Result<()>;
    fn gpio_set_outover(&self, gpio: u16, value: u16) -> io::Result<()>;
}

impl Pio for PioDevice {
    fn claim_any_sm(&self) -> io::Result<u16> {
        PioDevice::claim_any_sm(self)
    }
    fn unclaim_sms(&self, mask: u16) -> io::Result<()> {
        PioDevice::unclaim_sms(self, mask)
    }
    fn add_program(&self, origin: u16, instrs: &[u16]) -> io::Result<u16> {
        PioDevice::add_program(self, origin, instrs)
    }
    fn remove_program(&self, origin: u16, len: u16) -> io::Result<()> {
        PioDevice::remove_program(self, origin, len)
    }
    fn sm_init(&self, sm: u16, initial_pc: u16, config: SmConfig) -> io::Result<()> {
        PioDevice::sm_init(self, sm, initial_pc, config)
    }
    fn sm_set_pindirs(&self, sm: u16, dirs: u32, mask: u32) -> io::Result<()> {
        PioDevice::sm_set_pindirs(self, sm, dirs, mask)
    }
    fn sm_set_enabled(&self, mask: u16, enable: bool) -> io::Result<()> {
        PioDevice::sm_set_enabled(self, mask, enable)
    }
    fn sm_clear_fifos(&self, sm: u16) -> io::Result<()> {
        PioDevice::sm_clear_fifos(self, sm)
    }
    fn config_xfer32(&self, sm: u16, dir: u16, buf_size: u32, buf_count: u32) -> io::Result<()> {
        PioDevice::config_xfer32(self, sm, dir, buf_size, buf_count)
    }
    fn xfer_to_sm(&self, sm: u16, words: &[u32]) -> io::Result<()> {
        PioDevice::xfer_to_sm(self, sm, words)
    }
    fn xfer_from_sm(&self, sm: u16, words: &mut [u32]) -> io::Result<()> {
        PioDevice::xfer_from_sm(self, sm, words)
    }
    fn gpio_init(&self, gpio: u16) -> io::Result<()> {
        PioDevice::gpio_init(self, gpio)
    }
    fn gpio_set_function(&self, gpio: u16, func: u16) -> io::Result<()> {
        PioDevice::gpio_set_function(self, gpio, func)
    }
    fn gpio_set_input_enabled(&self, gpio: u16, enabled: bool) -> io::Result<()> {
        PioDevice::gpio_set_input_enabled(self, gpio, enabled)
    }
    fn gpio_set_outover(&self, gpio: u16, value: u16) -> io::Result<()> {
        PioDevice::gpio_set_outover(self, gpio, value)
    }
}

/// The RP1 PIO character device a board has, under the given `dev` directory.
pub(crate) const DEVNODE: &str = "pio0";

/// Opens the PIO bus for `pins` on `devnode`. Refuses pins a kernel driver has
/// requested (a leftover `i2c-gpio` device, say), since the bus would fight it
/// for them.
pub(crate) fn open_bus(devnode: &Path, pins: Pins) -> io::Result<PioBus<PioDevice>> {
    start_bus(PioDevice::open(devnode)?, pins)
}

/// Starts the bus for `pins` on an open client, either route, with the same check as
/// [`open_bus`].
pub(crate) fn start_bus(device: PioDevice, pins: Pins) -> io::Result<PioBus<PioDevice>> {
    check_pins_free(pins)?;
    PioBus::start(device, pins.sda, pins.scl, pins.hz)
}

/// Fails with `EBUSY` if SDA or SCL is requested from the kernel on the RP1 GPIO
/// chip. Chips that can't be read are skipped.
fn check_pins_free(pins: Pins) -> io::Result<()> {
    for number in 0..16 {
        let path = format!("/dev/gpiochip{number}");
        let Ok(file) = std::fs::File::open(&path) else {
            continue;
        };
        let Ok(info) = gpio::chip_info(file.as_fd()) else {
            continue;
        };
        if !label_of(&info.label).contains("rp1") {
            continue;
        }
        for pin in [pins.sda, pins.scl] {
            let Ok(line) = gpio::line_info(file.as_fd(), u32::from(pin)) else {
                continue;
            };
            if line.flags & gpio::flag::USED != 0 {
                return Err(io::Error::from_raw_os_error(libc::EBUSY));
            }
        }
        return Ok(());
    }
    Ok(())
}

fn label_of(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}
