//! BMI088 + BMM150 + INA238 on `/dev/i2c-N` through `lemnos_linux::hal`:
//! with each driver's own API (the "no runtime" floor), or with feature
//! `lite`, in a `lemnos-lite` table polled through the device model.
#![allow(clippy::print_stdout)]

use lemnos_drivers_bmi088::Bmi088;
use lemnos_drivers_bmm150::Bmm150;
use lemnos_drivers_ina2xx::{Config, Ina, Model};
use lemnos_linux::hal::{I2cBus, StdDelay};

fn main() -> std::io::Result<()> {
    let bus: u32 = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(1);
    let mut imu = Bmi088::new(I2cBus::open(bus)?);
    let mut mag = Bmm150::new(I2cBus::open(bus)?, 0x10);
    let mut power = Ina::new(
        I2cBus::open(bus)?,
        0x40,
        Model::Ina238,
        Config::from_micro(10_000, 4_000_000),
    )
    .map_err(|_| std::io::ErrorKind::InvalidInput)?;

    #[cfg(not(feature = "lite"))]
    {
        let _ = imu.init(&mut StdDelay, Default::default());
        let _ = mag.init(&mut StdDelay, Default::default());
        let _ = power.init();
        for _ in 0..10 {
            println!(
                "{:?} {:?} {:?}",
                imu.read_fixed().ok(),
                mag.read_fixed().ok(),
                power.read_fixed().ok()
            );
        }
    }
    #[cfg(feature = "lite")]
    {
        use lemnos_lite::{Devices, MAX_CHANNELS, sensor};
        let mut devices = Devices::new([
            sensor("imu", &mut imu).every(1),
            sensor("mag", &mut mag).every(1),
            sensor("power", &mut power).every(1),
        ]);
        devices.init_all(&mut StdDelay);
        let mut buf = [0; MAX_CHANNELS];
        for now in 0..10 {
            devices.poll(now, &mut StdDelay, &mut buf, |reading| {
                println!("{} {:?}", reading.name, reading.values);
            });
        }
        for i in 0..devices.len() {
            println!("{:?}", devices.status(i));
        }
    }
    Ok(())
}
