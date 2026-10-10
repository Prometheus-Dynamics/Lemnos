//! The BMI088 in the Lemnos device model (`lemnos-device`).

use crate::{Bmi088, Error, MAX_SAMPLES, asynch};
use embedded_hal::delay::DelayNs;
use lemnos_device::kernel::{KernelBinding, KernelChannel, KernelPart, Subsystem};
use lemnos_device::{Axis, Channel, Device, DeviceClass, DeviceError, DeviceInfo, Quantity};

/// An IMU with six channels, X, Y, Z each. The raw counts are acceleration in
/// mm/s² (exponent -3) and angular rate in µrad/s (exponent -6); a channel's
/// value (counts × 10^exponent) is in m/s² and rad/s, the quantities' units.
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

    /// With the FIFOs on, every sample since the last read, oldest first (up
    /// to [`MAX_SAMPLES`]). The accelerometer and gyroscope samples are paired
    /// from the newest end: when one FIFO holds a sample more than the other
    /// (a sample came between the two reads), its oldest is not returned.
    /// Without them, one sample, as [`read`](Self::read).
    fn read_batch(
        &mut self,
        out: &mut [[i32; lemnos_device::MAX_CHANNELS]],
    ) -> Result<usize, DeviceError<Self::Error>> {
        if !self.fifo {
            let Some(row) = out.first_mut() else {
                return Ok(0);
            };
            lemnos_device::Sensor::read(self, &mut row[..])?;
            return Ok(1);
        }
        let config = self
            .config
            .ok_or(DeviceError::Driver(Error::NotInitialized))?;
        let cap = out.len().min(MAX_SAMPLES);
        let mut accel = [[0i16; 3]; MAX_SAMPLES];
        let (na, _) = self
            .read_accel_fifo(&mut accel[..cap])
            .map_err(DeviceError::Driver)?;
        if na == 0 {
            return Ok(0);
        }
        let mut gyro = [[0i16; 3]; MAX_SAMPLES];
        // The whole gyroscope FIFO, so the newest frames are the ones paired.
        let mut ng = self
            .read_gyro_fifo(&mut gyro)
            .map_err(DeviceError::Driver)?;
        if ng == 0 {
            // The gyroscope FIFO is empty: hold its latest value for these.
            gyro[..na].fill(self.read_gyro_raw().map_err(DeviceError::Driver)?);
            ng = na;
        }
        let n = na.min(ng);
        for i in 0..n {
            fill(
                &mut out[i],
                config.channels(accel[na - n + i], gyro[ng - n + i]),
            );
        }
        Ok(n)
    }

    fn sample_period_us(&self) -> Option<u32> {
        if !self.fifo {
            return None;
        }
        self.config.map(|config| config.accel_rate.period_us())
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
