//! The INA2xx chips in the Lemnos device model (`lemnos-device`).

use crate::{Error, Ina, Model, asynch};
use embedded_hal::delay::DelayNs;
use lemnos_device::kernel::{KernelBinding, KernelChannel, KernelPart, Subsystem};
use lemnos_device::{Channel, Device, DeviceClass, DeviceError, DeviceInfo, Quantity};

const BUS: Channel = Channel::new("bus_voltage", Quantity::Voltage, -6);
const SHUNT: Channel = Channel::new("shunt_voltage", Quantity::Voltage, -9);
const CURRENT: Channel = Channel::new("current", Quantity::Current, -6);
const POWER: Channel = Channel::new("power", Quantity::Power, -6);
const DIE: Channel = Channel::new("die_temperature", Quantity::Temperature, -3);

/// INA226: bus voltage (µV), shunt voltage (nV), current (µA), power (µW).
pub static INFO_INA226: DeviceInfo = DeviceInfo::new(
    DeviceClass::PowerMonitor,
    "INA226",
    &[BUS, SHUNT, CURRENT, POWER],
    &[],
);
/// INA238: the INA226's channels plus die temperature (m°C).
pub static INFO_INA238: DeviceInfo = DeviceInfo::new(
    DeviceClass::PowerMonitor,
    "INA238",
    &[BUS, SHUNT, CURRENT, POWER, DIE],
    &[],
);
/// INA260: the INA226's channels (integrated shunt).
pub static INFO_INA260: DeviceInfo = DeviceInfo::new(
    DeviceClass::PowerMonitor,
    "INA260",
    &[BUS, SHUNT, CURRENT, POWER],
    &[],
);

// hwmon: in0 is the shunt and in1 the bus (mV), curr1 mA, power1 µW, temp1 m°C.
const K_BUS: Option<KernelChannel> = Some(KernelChannel::new(0, "in1", -3));
const K_SHUNT: Option<KernelChannel> = Some(KernelChannel::new(0, "in0", -3));
const K_CURRENT: Option<KernelChannel> = Some(KernelChannel::new(0, "curr1", -3));
const K_POWER: Option<KernelChannel> = Some(KernelChannel::new(0, "power1", -6));

/// The mainline `ina2xx` hwmon driver.
pub static KERNEL_INA226: KernelBinding = KernelBinding {
    parts: &[KernelPart {
        subsystem: Subsystem::Hwmon,
        names: &["ina226", "ina230", "ina231"],
    }],
    channels: &[K_BUS, K_SHUNT, K_CURRENT, K_POWER],
};
/// The mainline `ina238` hwmon driver.
pub static KERNEL_INA238: KernelBinding = KernelBinding {
    parts: &[KernelPart {
        subsystem: Subsystem::Hwmon,
        names: &["ina238", "ina237"],
    }],
    channels: &[
        K_BUS,
        K_SHUNT,
        K_CURRENT,
        K_POWER,
        Some(KernelChannel::new(0, "temp1", -3)),
    ],
};
/// The mainline `ina2xx` hwmon driver (INA260 support).
pub static KERNEL_INA260: KernelBinding = KernelBinding {
    parts: &[KernelPart {
        subsystem: Subsystem::Hwmon,
        names: &["ina260"],
    }],
    channels: &[K_BUS, K_SHUNT, K_CURRENT, K_POWER],
};

impl Model {
    /// The device-model description of this chip.
    pub fn info(self) -> &'static DeviceInfo {
        match self {
            Self::Ina226 => &INFO_INA226,
            Self::Ina238 => &INFO_INA238,
            Self::Ina260 => &INFO_INA260,
        }
    }

    /// How the mainline kernel driver exposes this chip.
    pub fn kernel(self) -> &'static KernelBinding {
        match self {
            Self::Ina226 => &KERNEL_INA226,
            Self::Ina238 => &KERNEL_INA238,
            Self::Ina260 => &KERNEL_INA260,
        }
    }
}

fn fill(out: &mut [i32], info: &DeviceInfo, values: [i32; 5]) {
    // Element by element: `copy_from_slice` would link `memcpy`.
    for (slot, value) in out.iter_mut().zip(values).take(info.channels.len()) {
        *slot = value;
    }
}

impl<I2C: embedded_hal::i2c::I2c> Device for Ina<I2C> {
    type Error = Error<I2C::Error>;

    fn info(&self) -> &'static DeviceInfo {
        self.setup.model.info()
    }

    /// Runs [`Ina::init`]; the chip needs no delay.
    fn init(&mut self, _delay: &mut dyn DelayNs) -> Result<(), DeviceError<Self::Error>> {
        Ina::init(self).map_err(DeviceError::Driver)
    }
}

impl<I2C: embedded_hal::i2c::I2c> lemnos_device::Sensor for Ina<I2C> {
    fn read(&mut self, out: &mut [i32]) -> Result<(), DeviceError<Self::Error>> {
        let info = self.setup.model.info();
        lemnos_device::check_buffer(info, out)?;
        let reading = self.read_fixed().map_err(DeviceError::Driver)?;
        fill(out, info, reading.channels());
        Ok(())
    }
}

impl<I2C: embedded_hal_async::i2c::I2c> lemnos_device::asynch::Device for asynch::Ina<I2C> {
    type Error = Error<I2C::Error>;

    fn info(&self) -> &'static DeviceInfo {
        self.setup.model.info()
    }

    async fn init(
        &mut self,
        _delay: &mut impl embedded_hal_async::delay::DelayNs,
    ) -> Result<(), DeviceError<Self::Error>> {
        asynch::Ina::init(self).await.map_err(DeviceError::Driver)
    }
}

impl<I2C: embedded_hal_async::i2c::I2c> lemnos_device::asynch::Sensor for asynch::Ina<I2C> {
    async fn read(&mut self, out: &mut [i32]) -> Result<(), DeviceError<Self::Error>> {
        let info = self.setup.model.info();
        lemnos_device::check_buffer(info, out)?;
        let reading = self.read_fixed().await.map_err(DeviceError::Driver)?;
        fill(out, info, reading.channels());
        Ok(())
    }
}
