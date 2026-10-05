//! A bare-metal image that uses each Lemnos driver the way firmware would:
//! init, then read in a loop. `black_box` stands in for the I2C peripheral so
//! nothing is const-folded away.
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

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    #[cfg(feature = "regs")]
    {
        use lemnos_hal::register::{AddressWidth, I2cRegisters, RegisterBus};
        let mut regs = I2cRegisters::new(Bus, 0x60, AddressWidth::Bits16);
        let _ = black_box(regs.read(black_box(0x300a), 2));
        let _ = black_box(regs.write(black_box(0x0100), 1, 1));
    }
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
        let mut ina = lemnos_drivers_ina2xx::Ina::new(
            Bus,
            0x40,
            lemnos_drivers_ina2xx::Model::Ina238,
            lemnos_drivers_ina2xx::Config::from_micro(black_box(10_000), black_box(2_000_000)),
        )
        .ok();
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

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}
