#![allow(clippy::print_stdout)]

//! Exercises the pure-Rust Linux layer on real hardware.
//!
//! ```text
//! linux_hal_probe discover                       # inventory by interface
//! linux_hal_probe gpio-info [/dev/gpiochipN]     # chips, or one chip's lines
//! linux_hal_probe gpio-read /dev/gpiochipN OFF   # read one line without changing it
//! linux_hal_probe i2c-read BUS ADDR REG LEN [8|16] # register read (claims ADDR, never forces)
//! linux_hal_probe hotplug SECONDS                # print hotplug watch events
//! ```

use lemnos::core::{DeviceKind, InterfaceKind};
use lemnos::discovery::InventoryWatcher;
use lemnos::hal::{AddressWidth, HalError, I2cRegisters, RegisterBus};
use lemnos::linux::hal::{GpioChip, I2cBus, LineDirection, LineSettings};
use lemnos::linux::{LinuxBackend, LinuxHotplugWatcher, LinuxPaths};
use lemnos::prelude::*;
use std::time::{Duration, Instant};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn parse_u32(text: &str) -> Result<u32> {
    Ok(match text.strip_prefix("0x") {
        Some(hex) => u32::from_str_radix(hex, 16)?,
        None => text.parse()?,
    })
}

fn discover() -> Result<()> {
    let backend = LinuxBackend::new();
    let mut lemnos = Lemnos::builder().with_linux_backend_ref(&backend).build();
    let report = lemnos.refresh_with_linux_default(&backend)?;
    let inventory = lemnos.inventory();
    println!("devices: {}", inventory.len());
    for interface in [
        InterfaceKind::Gpio,
        InterfaceKind::Pwm,
        InterfaceKind::I2c,
        InterfaceKind::Spi,
        InterfaceKind::Uart,
        InterfaceKind::Usb,
    ] {
        println!("  {interface}: {}", inventory.count_for(interface));
    }
    for kind in [
        DeviceKind::GpioChip,
        DeviceKind::I2cBus,
        DeviceKind::PwmChip,
    ] {
        for device in inventory.by_kind(kind) {
            println!(
                "  {kind}: {} ({})",
                device.id,
                device.display_name.as_deref().unwrap_or("")
            );
        }
    }
    for device in inventory.by_kind(DeviceKind::I2cDevice) {
        let driver = device
            .properties
            .get("driver")
            .and_then(|v| v.to_label_string())
            .unwrap_or_default();
        println!("  i2c device: {} driver={driver}", device.id);
    }
    for device in inventory.by_interface(InterfaceKind::Pwm) {
        if device.id.as_str().contains("hwmon") {
            println!(
                "  hwmon: {} ({})",
                device.id,
                device.display_name.as_deref().unwrap_or("")
            );
        }
    }
    println!("refresh: {:?}", report.discovery.probe_reports.len());
    Ok(())
}

fn gpio_info(path: Option<&str>) -> Result<()> {
    let Some(path) = path else {
        for path in GpioChip::paths()? {
            match GpioChip::open(&path) {
                Ok(chip) => {
                    let info = chip.info();
                    println!(
                        "{}: {} label={} lines={}",
                        path.display(),
                        info.name,
                        info.label,
                        info.lines
                    );
                }
                Err(error) => println!("{}: {error}", path.display()),
            }
        }
        return Ok(());
    };
    let chip = GpioChip::open(path)?;
    println!("{:?}", chip.info());
    for offset in 0..chip.num_lines() {
        let line = chip.line_info(offset)?;
        if !line.name.is_empty() || line.used {
            println!(
                "  {offset:3} {:<20} used={} consumer={:<12} {:?} active_low={} bias={:?}",
                line.name,
                line.used,
                line.consumer,
                line.settings.direction,
                line.settings.active_low,
                line.settings.bias
            );
        }
    }
    Ok(())
}

fn gpio_read(path: &str, offset: u32) -> Result<()> {
    let chip = GpioChip::open(path)?;
    let before = chip.line_info(offset)?;
    let settings = LineSettings {
        direction: LineDirection::AsIs,
        active_low: before.settings.active_low,
        ..LineSettings::default()
    };
    let line = chip.request_line("lemnos-probe", offset, settings)?;
    println!(
        "{path} line {offset} ({}) {:?}: value={}",
        before.name,
        before.settings.direction,
        line.get()?
    );
    Ok(())
}

fn i2c_read(bus: u32, address: u8, register: u16, len: usize, width: AddressWidth) -> Result<()> {
    let i2c = I2cBus::open(bus)?;
    println!("/dev/i2c-{bus}: plain I2C={}", i2c.supports_i2c());
    let mut regs = I2cRegisters::new(i2c, address, width);
    let mut buf = vec![0u8; len];
    match regs.read_burst(register, &mut buf) {
        Ok(()) => println!("{address:#04x} reg {register:#06x}: {buf:02x?}"),
        Err(error) => println!(
            "{address:#04x} reg {register:#06x}: {} ({error})",
            error.kind()
        ),
    }
    Ok(())
}

fn hotplug(seconds: u64) -> Result<()> {
    let mut watcher = LinuxHotplugWatcher::new(LinuxPaths::default())?;
    println!("hotplug source: {:?}", watcher.source());
    let deadline = Instant::now() + Duration::from_secs(seconds);
    while Instant::now() < deadline {
        for event in watcher.poll()? {
            println!(
                "event: interfaces={:?} paths={:?}",
                event.interfaces, event.paths
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let arg = |i: usize| args.get(i).map(String::as_str);
    match arg(0) {
        Some("discover") | None => discover(),
        Some("gpio-info") => gpio_info(arg(1)),
        Some("gpio-read") => gpio_read(
            arg(1).ok_or("chip path")?,
            parse_u32(arg(2).ok_or("offset")?)?,
        ),
        Some("i2c-read") => {
            let width = match arg(5) {
                Some("8") => AddressWidth::Bits8,
                _ => AddressWidth::Bits16,
            };
            i2c_read(
                parse_u32(arg(1).ok_or("bus")?)?,
                u8::try_from(parse_u32(arg(2).ok_or("address")?)?)?,
                u16::try_from(parse_u32(arg(3).ok_or("register")?)?)?,
                parse_u32(arg(4).unwrap_or("1"))? as usize,
                width,
            )
        }
        Some("hotplug") => hotplug(parse_u32(arg(1).unwrap_or("10"))?.into()),
        Some(other) => Err(format!("unknown command {other}").into()),
    }
}
