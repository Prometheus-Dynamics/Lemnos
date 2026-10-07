//! A small Linux program: the Raze's IMU, magnetometer and power monitor on
//! one I2C bus, through `lemnos-lite` and `lemnos_linux::hal`, with no
//! runtime, discovery or allocation in the device path.
//!
//! ```text
//! cargo run -p lemnos-lite --example linux_sensors -- <bus> [polls]
//! ```
//!
//! Devices that do not answer stay `missing` and are retried on their
//! polling period.
#![allow(clippy::print_stdout)]

use lemnos_drivers_bmi088::Bmi088;
use lemnos_drivers_bmm150::{Bmm150, DEFAULT_ADDRESS as BMM150_ADDRESS};
use lemnos_drivers_ina2xx::{Config, DEFAULT_ADDRESS as INA_ADDRESS, Ina, Model};
use lemnos_linux::hal::{I2cBus, StdDelay};
use lemnos_lite::{Devices, MAX_CHANNELS, sensor};
use std::time::{Duration, Instant};

fn main() -> std::io::Result<()> {
    let mut args = std::env::args().skip(1);
    let bus: u32 = args.next().and_then(|a| a.parse().ok()).unwrap_or(1);
    let polls: u32 = args.next().and_then(|a| a.parse().ok()).unwrap_or(10);

    let mut imu = Bmi088::new(I2cBus::open(bus)?);
    let mut mag = Bmm150::new(I2cBus::open(bus)?, BMM150_ADDRESS);
    let mut power = Ina::new(
        I2cBus::open(bus)?,
        INA_ADDRESS,
        Model::Ina238,
        Config::from_micro(10_000, 4_000_000),
    )
    .map_err(|e| std::io::Error::other(e.to_string()))?;
    let mut devices = Devices::new([
        sensor("imu", &mut imu).every(100),
        sensor("magnetometer", &mut mag).every(100),
        sensor("power", &mut power).every(500),
    ]);

    let start = Instant::now();
    let mut buf = [0i32; MAX_CHANNELS];
    for _ in 0..polls {
        let now = start.elapsed().as_millis() as u64;
        devices.poll(now, &mut StdDelay, &mut buf, |reading| {
            let values: Vec<String> = reading
                .info
                .channels
                .iter()
                .zip(reading.values)
                .map(|(channel, raw)| {
                    let value = f64::from(*raw) * 10f64.powi(channel.exponent.into());
                    format!("{}={value:.4}{}", channel.name, channel.unit().symbol())
                })
                .collect();
            println!("{now:>6} ms {}: {}", reading.name, values.join(" "));
        });
        std::thread::sleep(Duration::from_millis(100));
    }
    for i in 0..devices.len() {
        println!(
            "{}: {} (last error: {:?})",
            devices.name(i).unwrap_or("?"),
            devices.status(i).name(),
            devices.last_error(i)
        );
    }
    Ok(())
}
