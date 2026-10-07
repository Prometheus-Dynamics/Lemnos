//! The BMI088 in the Lemnos device model (`lemnos-device`).

use crate::{Bmi088, Error, asynch};
use embedded_hal::delay::DelayNs;
use lemnos_device::kernel::{KernelBinding, KernelChannel, KernelPart, Subsystem};
use lemnos_device::{Axis, Channel, Device, DeviceClass, DeviceError, DeviceInfo, Quantity};

/// An IMU with six channels: acceleration in mm/s² (exponent -3) and angular
/// rate in µrad/s (exponent -6), X, Y, Z each.
pub static INFO: DeviceInfo = DeviceInfo::new(
    DeviceClass::Imu,
    "BMI088",
    &[
        Channel::new("acceleration.x", Quantity::Acceleration, -3).on(Axis::X),
        Channel::new("acceleration.y", Quantity::Acceleration, -3).on(Axis::Y),
        Channel::new("acceleration.z", Quantity::Acceleration, -3).on(Axis::Z),
        Channel::new("angular_rate.x", Quantity::AngularRate, -6).on(Axis::X),
        Channel::new("angular_rate.y", Quantity::AngularRate, -6).on(Axis::Y),
        Channel::new("angular_rate.z", Quantity::AngularRate, -6).on(Axis::Z),
    ],
    &[],
);

/// The mainline kernel drivers: `bmi088-accel` (IIO, m/s²) for the
/// accelerometer and `bmg160` (IIO, rad/s) for the gyroscope.
pub static KERNEL: KernelBinding = KernelBinding {
    parts: &[
        KernelPart {
            subsystem: Subsystem::Iio,
            names: &["bmi088-accel", "bmi088_accel", "bmi088a"],
        },
        KernelPart {
            subsystem: Subsystem::Iio,
            names: &["bmi088_gyro", "bmi088-gyro", "bmi088g"],
        },
    ],
    channels: &[
        Some(KernelChannel::new(0, "in_accel_x", 0)),
        Some(KernelChannel::new(0, "in_accel_y", 0)),
        Some(KernelChannel::new(0, "in_accel_z", 0)),
        Some(KernelChannel::new(1, "in_anglvel_x", 0)),
        Some(KernelChannel::new(1, "in_anglvel_y", 0)),
        Some(KernelChannel::new(1, "in_anglvel_z", 0)),
    ],
};

fn fill(out: &mut [i32], values: [i32; 6]) {
    // Element by element: `copy_from_slice` would link `memcpy`.
    for (slot, value) in out.iter_mut().zip(values) {
        *slot = value;
    }
}

impl<I2C: embedded_hal::i2c::I2c> Device for Bmi088<I2C> {
    type Error = Error<I2C::Error>;

    fn info(&self) -> &'static DeviceInfo {
        &INFO
    }

    /// Runs [`Bmi088::init`] with the configuration from
    /// [`with_config`](Bmi088::with_config) (or the last one applied).
    fn init(&mut self, delay: &mut dyn DelayNs) -> Result<(), DeviceError<Self::Error>> {
        let settings = self.settings;
        Bmi088::init(self, &mut &mut *delay, settings).map_err(DeviceError::Driver)
    }
}

impl<I2C: embedded_hal::i2c::I2c> lemnos_device::Sensor for Bmi088<I2C> {
    fn read(&mut self, out: &mut [i32]) -> Result<(), DeviceError<Self::Error>> {
        lemnos_device::check_buffer(&INFO, out)?;
        let config = self
            .config
            .ok_or(DeviceError::Driver(Error::NotInitialized))?;
        let accel = self.read_accel_raw().map_err(DeviceError::Driver)?;
        let gyro = self.read_gyro_raw().map_err(DeviceError::Driver)?;
        fill(out, config.channels(accel, gyro));
        Ok(())
    }
}

impl<I2C: embedded_hal_async::i2c::I2c> lemnos_device::asynch::Device for asynch::Bmi088<I2C> {
    type Error = Error<I2C::Error>;

    fn info(&self) -> &'static DeviceInfo {
        &INFO
    }

    async fn init(
        &mut self,
        delay: &mut impl embedded_hal_async::delay::DelayNs,
    ) -> Result<(), DeviceError<Self::Error>> {
        let settings = self.settings;
        asynch::Bmi088::init(self, delay, settings)
            .await
            .map_err(DeviceError::Driver)
    }
}

impl<I2C: embedded_hal_async::i2c::I2c> lemnos_device::asynch::Sensor for asynch::Bmi088<I2C> {
    async fn read(&mut self, out: &mut [i32]) -> Result<(), DeviceError<Self::Error>> {
        lemnos_device::check_buffer(&INFO, out)?;
        let config = self
            .config
            .ok_or(DeviceError::Driver(Error::NotInitialized))?;
        let accel = self.read_accel_raw().await.map_err(DeviceError::Driver)?;
        let gyro = self.read_gyro_raw().await.map_err(DeviceError::Driver)?;
        fill(out, config.channels(accel, gyro));
        Ok(())
    }
}
