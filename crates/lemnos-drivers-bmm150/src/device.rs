//! The BMM150 in the Lemnos device model (`lemnos-device`).

use crate::{Bmm150, Error, asynch};
use embedded_hal::delay::DelayNs;
use lemnos_device::kernel::{KernelBinding, KernelChannel, KernelPart, Subsystem};
use lemnos_device::{Axis, Channel, Device, DeviceClass, DeviceError, DeviceInfo, Quantity};

/// A magnetometer with three channels: the field in nT (exponent -9), X, Y,
/// Z. An overflowed axis reads `NO_VALUE`.
pub static INFO: DeviceInfo = DeviceInfo::new(
    DeviceClass::Magnetometer,
    "BMM150",
    &[
        Channel::new("magnetic_field.x", Quantity::MagneticField, -9).on(Axis::X),
        Channel::new("magnetic_field.y", Quantity::MagneticField, -9).on(Axis::Y),
        Channel::new("magnetic_field.z", Quantity::MagneticField, -9).on(Axis::Z),
    ],
    &[],
);

/// The mainline kernel driver: `bmc150_magn` (IIO, gauss).
pub static KERNEL: KernelBinding = KernelBinding {
    parts: &[KernelPart {
        subsystem: Subsystem::Iio,
        names: &["bmm150", "bmm150_magn", "bmc150_magn"],
    }],
    channels: &[
        Some(KernelChannel::new(0, "in_magn_x", -4)),
        Some(KernelChannel::new(0, "in_magn_y", -4)),
        Some(KernelChannel::new(0, "in_magn_z", -4)),
    ],
};

fn fill(out: &mut [i32], values: [i32; 3]) {
    // Element by element: `copy_from_slice` would link `memcpy`.
    for (slot, value) in out.iter_mut().zip(values) {
        *slot = value;
    }
}

impl<I2C: embedded_hal::i2c::I2c> Device for Bmm150<I2C> {
    type Error = Error<I2C::Error>;

    fn info(&self) -> &'static DeviceInfo {
        &INFO
    }

    /// Runs [`Bmm150::init`] with the configuration from
    /// [`with_config`](Bmm150::with_config).
    fn init(&mut self, delay: &mut dyn DelayNs) -> Result<(), DeviceError<Self::Error>> {
        let settings = self.settings;
        Bmm150::init(self, &mut &mut *delay, settings).map_err(DeviceError::Driver)
    }
}

impl<I2C: embedded_hal::i2c::I2c> lemnos_device::Sensor for Bmm150<I2C> {
    fn read(&mut self, out: &mut [i32]) -> Result<(), DeviceError<Self::Error>> {
        lemnos_device::check_buffer(&INFO, out)?;
        let field = self.read_fixed().map_err(DeviceError::Driver)?;
        fill(out, field.channels());
        Ok(())
    }
}

impl<I2C: embedded_hal_async::i2c::I2c> lemnos_device::asynch::Device for asynch::Bmm150<I2C> {
    type Error = Error<I2C::Error>;

    fn info(&self) -> &'static DeviceInfo {
        &INFO
    }

    async fn init(
        &mut self,
        delay: &mut impl embedded_hal_async::delay::DelayNs,
    ) -> Result<(), DeviceError<Self::Error>> {
        let settings = self.settings;
        asynch::Bmm150::init(self, delay, settings)
            .await
            .map_err(DeviceError::Driver)
    }
}

impl<I2C: embedded_hal_async::i2c::I2c> lemnos_device::asynch::Sensor for asynch::Bmm150<I2C> {
    async fn read(&mut self, out: &mut [i32]) -> Result<(), DeviceError<Self::Error>> {
        lemnos_device::check_buffer(&INFO, out)?;
        let field = self.read_fixed().await.map_err(DeviceError::Driver)?;
        fill(out, field.channels());
        Ok(())
    }
}
