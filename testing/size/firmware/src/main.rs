//! A bare-metal image that uses each Lemnos driver the way firmware would:
//! init, then read in a loop. `black_box` stands in for the I2C peripheral so
//! nothing is const-folded away.
//!
//! - default: each driver's own API (`init`, `read_fixed`).
//! - `device-static`: the same drivers through the device-model traits,
//!   statically dispatched (`Device::init`, `Sensor::read`, `Control::set`).
//! - `device`: the same drivers through the device model, as `dyn` sensors
//!   and controls (`lemnos_device::DeviceRef`).
//! - `lite`: the same drivers in a `lemnos_lite::Devices` table, polled.
#![no_std]
#![no_main]
#![allow(unused)]

use core::hint::black_box;
use embedded_hal::delay::DelayNs;
use embedded_hal::i2c::{ErrorKind, ErrorType, I2c, Operation, SevenBitAddress};

struct Bus;
#[derive(Debug)]
struct E;
impl embedded_hal::i2c::Error for E {
    fn kind(&self) -> ErrorKind {
        ErrorKind::Other
    }
}
impl ErrorType for Bus {
    type Error = E;
}
impl I2c<SevenBitAddress> for Bus {
    fn transaction(&mut self, address: u8, ops: &mut [Operation<'_>]) -> Result<(), E> {
        for op in ops {
            match op {
                Operation::Read(buf) => buf.fill(black_box(address)),
                Operation::Write(bytes) => {
                    black_box(bytes);
                }
            }
        }
        if black_box(false) { Err(E) } else { Ok(()) }
    }
}
struct Delay;
impl DelayNs for Delay {
    fn delay_ns(&mut self, ns: u32) {
        black_box(ns);
    }
}

#[cfg(feature = "ina2xx")]
fn ina() -> Option<lemnos_drivers_ina2xx::Ina<Bus>> {
    lemnos_drivers_ina2xx::Ina::new(
        Bus,
        0x40,
        lemnos_drivers_ina2xx::Model::Ina238,
        lemnos_drivers_ina2xx::Config::from_micro(black_box(10_000), black_box(2_000_000)),
    )
    .ok()
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    #[cfg(feature = "regs")]
    {
        use lemnos_hal::register::{AddressWidth, I2cRegisters, RegisterBus};
        let mut regs = I2cRegisters::new(Bus, 0x60, AddressWidth::Bits16);
        let _ = black_box(regs.read(black_box(0x300a), 2));
        let _ = black_box(regs.write(black_box(0x0100), 1, 1));
    }
    #[cfg(any(feature = "device", feature = "lite", feature = "device-static"))]
    model();
    #[cfg(not(any(feature = "device", feature = "lite", feature = "device-static")))]
    direct();
    loop {}
}

/// Each driver's own API.
#[cfg(not(any(feature = "device", feature = "lite", feature = "device-static")))]
fn direct() -> ! {
    #[cfg(feature = "bmi088")]
    let mut imu = {
        let mut imu = lemnos_drivers_bmi088::Bmi088::new(Bus);
        let _ = imu.init(&mut Delay, lemnos_drivers_bmi088::Config::default());
        imu
    };
    #[cfg(feature = "bmm150")]
    let mut mag = {
        let mut mag = lemnos_drivers_bmm150::Bmm150::new(Bus, 0x10);
        let _ = mag.init(&mut Delay, lemnos_drivers_bmm150::Config::default());
        mag
    };
    #[cfg(feature = "ina2xx")]
    let mut ina = {
        let mut ina = ina();
        if let Some(ina) = ina.as_mut() {
            let _ = ina.init();
        }
        ina
    };
    #[cfg(feature = "vcm")]
    let mut lens = {
        let mut lens = lemnos_drivers_vcm::Vcm::dw9817(Bus);
        let _ = lens.power_up(&mut Delay);
        lens
    };
    loop {
        #[cfg(all(feature = "bmi088", feature = "float"))]
        let _ = black_box(imu.read());
        #[cfg(all(feature = "bmi088", not(feature = "float")))]
        let _ = black_box(imu.read_fixed());
        #[cfg(all(feature = "bmm150", feature = "float"))]
        let _ = black_box(mag.read());
        #[cfg(all(feature = "bmm150", not(feature = "float")))]
        let _ = black_box(mag.read_fixed());
        #[cfg(all(feature = "ina2xx", feature = "float"))]
        if let Some(ina) = ina.as_mut() {
            let _ = black_box(ina.read());
        }
        #[cfg(all(feature = "ina2xx", not(feature = "float")))]
        if let Some(ina) = ina.as_mut() {
            let _ = black_box(ina.read_fixed());
        }
        #[cfg(feature = "vcm")]
        let _ = black_box(lens.move_to(black_box(512)));
    }
}

/// The four drivers through the device model: brought up and used only
/// through `dyn` sensors and controls.
#[cfg(any(feature = "device", feature = "lite", feature = "device-static"))]
fn model() -> ! {
    use lemnos_device::{DeviceRef, MAX_CHANNELS};
    let mut imu = lemnos_drivers_bmi088::Bmi088::new(Bus);
    let mut mag = lemnos_drivers_bmm150::Bmm150::new(Bus, 0x10);
    let mut lens = lemnos_drivers_vcm::Vcm::dw9817(Bus);
    let Some(mut power) = ina() else { loop {} };
    let mut buf = [0i32; MAX_CHANNELS];

    #[cfg(all(feature = "device-static", not(feature = "device")))]
    {
        use lemnos_device::{Control, Device, Sensor};
        let _ = black_box(Device::init(&mut imu, &mut Delay));
        let _ = black_box(Device::init(&mut mag, &mut Delay));
        let _ = black_box(Device::init(&mut power, &mut Delay));
        let _ = black_box(Device::init(&mut lens, &mut Delay));
        loop {
            let _ = black_box(Sensor::read(&mut imu, &mut buf));
            let _ = black_box(Sensor::read(&mut mag, &mut buf));
            let _ = black_box(Sensor::read(&mut power, &mut buf));
            let _ = black_box(Control::set(&mut lens, 0, black_box(512)));
            black_box(&buf);
        }
    }
    #[cfg(feature = "device")]
    {
        let mut devices = [
            DeviceRef::sensor(&mut imu),
            DeviceRef::sensor(&mut mag),
            DeviceRef::sensor(&mut power),
            DeviceRef::control(&mut lens),
        ];
        for device in devices.iter_mut() {
            let _ = black_box(device.init(&mut Delay));
        }
        loop {
            for device in devices.iter_mut() {
                if device.is_sensor() {
                    let _ = black_box(device.read(&mut buf));
                } else {
                    let _ = black_box(device.set(0, black_box(512)));
                }
            }
            black_box(&buf);
        }
    }
    #[cfg(all(
        feature = "lite",
        not(feature = "device"),
        not(feature = "device-static")
    ))]
    {
        let mut devices = lemnos_lite::Devices::new([
            lemnos_lite::sensor("imu", &mut imu).every(10),
            lemnos_lite::sensor("mag", &mut mag).every(50),
            lemnos_lite::sensor("power", &mut power).every(100),
            lemnos_lite::control("lens", &mut lens),
        ]);
        devices.init_all(&mut Delay);
        let mut now = 0u64;
        loop {
            now = black_box(now + 1);
            devices.poll(now, &mut Delay, &mut buf, |reading| {
                black_box(reading.values);
            });
            let _ = black_box(devices.set("lens", 0, black_box(512)));
            black_box(devices.status(black_box(2)));
        }
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}
