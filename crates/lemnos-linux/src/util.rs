use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
#[cfg(any(feature = "gpio-sysfs", feature = "pwm"))]
use std::thread;
#[cfg(any(feature = "gpio-sysfs", feature = "pwm"))]
use std::time::Duration;

pub fn read_dir_sorted(root: &Path) -> io::Result<Vec<PathBuf>> {
    match fs::read_dir(root) {
        Ok(entries) => {
            let mut paths = entries
                .map(|entry| entry.map(|entry| entry.path()))
                .collect::<io::Result<Vec<_>>>()?;
            paths.sort();
            Ok(paths)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error),
    }
}

pub fn read_trimmed(path: &Path) -> io::Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(contents) => {
            let trimmed = contents.trim();
            if trimmed.is_empty() {
                Ok(None)
            } else {
                Ok(Some(trimmed.to_string()))
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

pub fn read_u32(path: &Path) -> io::Result<Option<u32>> {
    Ok(read_trimmed(path)?.and_then(|value| value.parse::<u32>().ok()))
}

#[cfg(feature = "usb")]
pub fn read_hex_u16(path: &Path) -> io::Result<Option<u16>> {
    Ok(read_trimmed(path)?.and_then(|value| u16::from_str_radix(&value, 16).ok()))
}

#[cfg(feature = "usb")]
pub fn read_hex_u8(path: &Path) -> io::Result<Option<u8>> {
    Ok(read_trimmed(path)?.and_then(|value| u8::from_str_radix(&value, 16).ok()))
}

pub fn read_link_name(path: &Path) -> io::Result<Option<String>> {
    match fs::read_link(path) {
        Ok(target) => Ok(file_name(&target).map(str::to_string)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

pub fn file_name(path: &Path) -> Option<&str> {
    path.file_name().and_then(OsStr::to_str)
}

pub fn existing_path_string(path: &Path) -> Option<String> {
    path.exists().then(|| path.display().to_string())
}

#[cfg(any(feature = "gpio-sysfs", feature = "pwm"))]
pub fn wait_for_path(path: &Path, retries: usize, delay_ms: u64) -> bool {
    for _ in 0..retries {
        if path.exists() {
            return true;
        }
        thread::sleep(Duration::from_millis(delay_ms));
    }
    path.exists()
}

#[cfg(any(feature = "i2c", feature = "pwm"))]
pub fn parse_prefixed_u32(value: &str, prefix: &str) -> Option<u32> {
    value.strip_prefix(prefix)?.parse::<u32>().ok()
}

#[cfg(feature = "i2c")]
pub fn parse_i2c_device_name(value: &str) -> Option<(u32, u16)> {
    let (bus, address) = value.split_once('-')?;
    Some((
        bus.parse::<u32>().ok()?,
        u16::from_str_radix(address, 16).ok()?,
    ))
}

#[cfg(feature = "spi")]
pub fn parse_spi_device_name(value: &str) -> Option<(u32, u16)> {
    let suffix = value.strip_prefix("spi")?;
    let (bus, chip_select) = suffix.split_once('.')?;
    Some((bus.parse::<u32>().ok()?, chip_select.parse::<u16>().ok()?))
}

#[cfg(feature = "usb")]
pub fn parse_usb_bus_name(value: &str) -> Option<u16> {
    value.strip_prefix("usb")?.parse::<u16>().ok()
}

#[cfg(feature = "usb")]
pub fn parse_usb_device_name(value: &str) -> Option<(u16, Vec<u8>)> {
    if value.contains(':') {
        return None;
    }
    let (bus, ports) = value.split_once('-')?;
    let ports = ports
        .split('.')
        .map(|segment| segment.parse::<u8>().ok())
        .collect::<Option<Vec<_>>>()?;
    Some((bus.parse::<u16>().ok()?, ports))
}

#[cfg(feature = "usb")]
pub fn parse_usb_interface_name(value: &str) -> Option<(u16, Vec<u8>, u8, u8)> {
    let (device, suffix) = value.split_once(':')?;
    let (bus, ports) = parse_usb_device_name(device)?;
    let (configuration, interface_number) = suffix.split_once('.')?;
    Some((
        bus,
        ports,
        configuration.parse::<u8>().ok()?,
        interface_number.parse::<u8>().ok()?,
    ))
}
