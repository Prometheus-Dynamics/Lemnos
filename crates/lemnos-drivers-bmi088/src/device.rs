//! The BMI088 in the Lemnos device model (`lemnos-device`).

use crate::{Bmi088, Error, MAX_SAMPLES, asynch};
use embedded_hal::delay::DelayNs;
use lemnos_device::kernel::{KernelBinding, KernelChannel, KernelPart, Subsystem};
use lemnos_device::{
    Axis, Channel, Device, DeviceClass, DeviceError, DeviceInfo, NO_VALUE, Quantity,
};

/// The channels: six raw (`acceleration`, `angular_rate`) and six calibrated
/// (`acceleration_cal`, `angular_rate_cal`), X, Y, Z each. The raw counts are
/// acceleration in mm/s² (exponent -3) and angular rate in µrad/s (exponent
/// -6); a channel's value (counts × 10^exponent) is in m/s² and rad/s, the
/// quantities' units. The calibrated channels are the same units, with the
/// accelerometer's offset and scale and the gyroscope's zero-rate offset
/// removed (see `calibration.rs`); `NO_VALUE` without the `float` feature.
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
        Channel::new("acceleration_cal.x", Quantity::Acceleration, -3).on(Axis::X),
        Channel::new("acceleration_cal.y", Quantity::Acceleration, -3).on(Axis::Y),
        Channel::new("acceleration_cal.z", Quantity::Acceleration, -3).on(Axis::Z),
        Channel::new("angular_rate_cal.x", Quantity::AngularRate, -6).on(Axis::X),
        Channel::new("angular_rate_cal.y", Quantity::AngularRate, -6).on(Axis::Y),
        Channel::new("angular_rate_cal.z", Quantity::AngularRate, -6).on(Axis::Z),
    ],
    &[],
);

/// The raw channels come first; the calibrated ones follow.
const RAW: usize = 6;
const CHANNELS: usize = 12;

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
        // The calibrated channels are the driver's, not the kernel's.
        None,
        None,
        None,
        None,
        None,
        None,
    ],
};

/// Copies `src` into `dst` element by element (`memcpy` is a library call on
/// MCUs).
fn copy_into(dst: &mut [i32], src: &[i32]) {
    for (slot, value) in dst.iter_mut().zip(src) {
        *slot = *value;
    }
}

fn fill(out: &mut [i32], values: [i32; 6]) {
    // Element by element: `copy_from_slice` would link `memcpy`.
    for (slot, value) in out.iter_mut().zip(values) {
        *slot = value;
    }
}

/// Rounds half away from zero (no `libm`: `no_std`).
#[cfg(feature = "float")]
fn round(x: f32) -> i32 {
    if x >= 0.0 {
        (x + 0.5) as i32
    } else {
        (x - 0.5) as i32
    }
}

/// The calibrated channels in device units: mm/s² and µrad/s.
#[cfg(feature = "float")]
fn cal_counts(accel: [f32; 3], gyro: [f32; 3]) -> [i32; 6] {
    [
        round(accel[0] * 1_000.0),
        round(accel[1] * 1_000.0),
        round(accel[2] * 1_000.0),
        round(gyro[0] * 1_000_000.0),
        round(gyro[1] * 1_000_000.0),
        round(gyro[2] * 1_000_000.0),
    ]
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
        let (accel, gyro) = self
            .read_axes(self.selection() & 0b111, self.selection() >> 3)
            .map_err(DeviceError::Driver)?;
        let mut values = [NO_VALUE; CHANNELS];
        let raw = config.channels(accel, gyro);
        copy_into(&mut values[..RAW], &raw);
        // A read of every axis of both dies is a full sample: it feeds the
        // calibration and gives the calibrated channels a value.
        #[cfg(feature = "float")]
        if self.selection() == crate::ALL_AXES {
            let si = config.sample(accel, gyro);
            let period = u64::from(config.accel_rate.period_us());
            let (a, g) = self.cal.sample(period, si.accel_mps2, si.gyro_radps);
            copy_into(&mut values[RAW..], &cal_counts(a, g));
        }
        let mask = self.channel_mask();
        for (i, slot) in out.iter_mut().enumerate().take(CHANNELS) {
            *slot = if mask & (1 << i) != 0 {
                values[i]
            } else {
                NO_VALUE
            };
        }
        Ok(())
    }

    /// With the FIFOs on, every sample since the last read, oldest first (up
    /// to [`MAX_SAMPLES`]). The accelerometer and gyroscope samples are paired
    /// from the newest end: when one FIFO holds a sample more than the other
    /// (a sample came between the two reads), its oldest is not returned.
    /// Every sample feeds the calibration. Without the FIFOs, one sample, as
    /// [`read`](Self::read).
    fn read_batch(
        &mut self,
        out: &mut [[i32; lemnos_device::MAX_CHANNELS]],
    ) -> Result<usize, DeviceError<Self::Error>> {
        if !self.fifo() {
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
        #[cfg(feature = "float")]
        let period = u64::from(config.accel_rate.period_us());
        for i in 0..n {
            let (a, g) = (accel[na - n + i], gyro[ng - n + i]);
            let mut row = [NO_VALUE; CHANNELS];
            copy_into(&mut row[..RAW], &config.channels(a, g));
            #[cfg(feature = "float")]
            {
                let si = config.sample(a, g);
                let (ca, cg) = self.cal.sample(period, si.accel_mps2, si.gyro_radps);
                copy_into(&mut row[RAW..], &cal_counts(ca, cg));
            }
            for (slot, value) in out[i].iter_mut().zip(row) {
                *slot = value;
            }
        }
        Ok(n)
    }

    fn sample_period_us(&self) -> Option<u32> {
        if !self.fifo() {
            return None;
        }
        self.config().map(|config| config.accel_rate.period_us())
    }

    /// The channels (bit i: channel i of [`INFO`]). The FIFO batch reads
    /// return every channel (both FIFOs hold both dies); the selection applies
    /// to plain reads.
    fn select_channels(&mut self, mask: u64) {
        self.select_channel_mask((mask & 0x0fff) as u16);
    }

    /// The calibration's status (feature `float`; `None` without it).
    #[cfg(feature = "float")]
    fn calibration_status(&self) -> Option<lemnos_device::CalibrationStatus> {
        Some(self.calibration_status())
    }

    #[cfg(feature = "float")]
    fn calibration_command(
        &mut self,
        command: lemnos_device::CalibrationCommand,
    ) -> Result<(), DeviceError<Self::Error>> {
        self.cal
            .command(command)
            .map_err(|_| DeviceError::Unsupported)
    }

    #[cfg(feature = "float")]
    fn calibration_words(&self, out: &mut [i32]) -> usize {
        self.cal.words(out)
    }

    #[cfg(feature = "float")]
    fn load_calibration(&mut self, words: &[i32]) -> Result<(), DeviceError<Self::Error>> {
        self.cal
            .load(words)
            .map_err(|()| DeviceError::Driver(Error::CalibrationWords))
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
        // The calibration runs on the blocking driver only.
        for slot in out.iter_mut().skip(RAW).take(CHANNELS - RAW) {
            *slot = NO_VALUE;
        }
        Ok(())
    }
}
