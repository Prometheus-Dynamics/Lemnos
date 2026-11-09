//! Finding one configuration in the raw descriptors a device file serves: the 18-byte device
//! descriptor, then each configuration, one after another. Every length here comes from the device.

use lemnos_hal::ErrorKind;
use lemnos_linux_sys::usbfs::UsbfsFile;

use crate::error::kind_of;

const DEVICE_LEN: usize = 18;
const DEVICE_TYPE: u8 = 1;
const CONFIG_LEN: usize = 9;
const CONFIG_TYPE: u8 = 2;

/// Copies configuration `index` into `out`, as much as fits, and returns the bytes copied.
pub(crate) fn config_descriptor(
    file: &UsbfsFile,
    index: u8,
    out: &mut [u8],
) -> Result<usize, ErrorKind> {
    let mut device = [0u8; DEVICE_LEN];
    read_header(file, 0, &mut device)?;
    if usize::from(device[0]) != DEVICE_LEN || device[1] != DEVICE_TYPE {
        return Err(ErrorKind::Configuration);
    }
    if index >= device[17] {
        return Err(ErrorKind::NotFound);
    }
    let mut position = DEVICE_LEN as u64;
    let mut config = 0u8;
    loop {
        let mut header = [0u8; CONFIG_LEN];
        read_header(file, position, &mut header)?;
        if usize::from(header[0]) < CONFIG_LEN || header[1] != CONFIG_TYPE {
            return Err(ErrorKind::Configuration);
        }
        let total = usize::from(u16::from_le_bytes([header[2], header[3]]));
        if total < CONFIG_LEN {
            return Err(ErrorKind::Configuration);
        }
        if config == index {
            let wanted = total.min(out.len());
            return read_fully(file, position, &mut out[..wanted]);
        }
        // At most 255 steps of at most 65535 bytes: no overflow in a u64.
        position += total as u64;
        config += 1;
    }
}

/// Reads exactly `out.len()` bytes, or the file doesn't hold the header it claims to.
fn read_header(file: &UsbfsFile, offset: u64, out: &mut [u8]) -> Result<(), ErrorKind> {
    if read_fully(file, offset, out)? < out.len() {
        return Err(ErrorKind::Configuration);
    }
    Ok(())
}

/// Reads until `out` is full or the file ends, and returns how many bytes that was.
fn read_fully(file: &UsbfsFile, offset: u64, out: &mut [u8]) -> Result<usize, ErrorKind> {
    let mut filled = 0;
    while filled < out.len() {
        let next = offset + filled as u64;
        match file.read_descriptors(next, &mut out[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(error) => return Err(kind_of(&error)),
        }
    }
    Ok(filled)
}
