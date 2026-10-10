//! The BMM150 in the Lemnos device model (`lemnos-device`).

use crate::{Bmm150, Error, asynch};
use embedded_hal::delay::DelayNs;
#[cfg(feature = "float")]
use lemnos_device::NO_VALUE;
use lemnos_device::kernel::{KernelBinding, KernelChannel, KernelPart, Subsystem};
use lemnos_device::{Axis, Channel, Device, DeviceClass, DeviceError, DeviceInfo, Quantity};

/// The channels: the raw field (`magnetic_field`, X, Y, Z) in nT (exponent -9)
/// and the calibrated field (`magnetic_field_cal`, the hard- and soft-iron
/// correction applied, see `calibration.rs`), in the same units. An overflowed
/// raw axis reads `NO_VALUE`; the calibrated channels are `NO_VALUE` without
/// the `float` feature, or while the reading has an overflowed axis.
pub static INFO: DeviceInfo = DeviceInfo::new(
    DeviceClass::Magnetometer,
    "BMM150",
    &[
        Channel::new("magnetic_field.x", Quantity::MagneticField, -9).on(Axis::X),
        Channel::new("magnetic_field.y", Quantity::MagneticField, -9).on(Axis::Y),
        Channel::new("magnetic_field.z", Quantity::MagneticField, -9).on(Axis::Z),
        Channel::new("magnetic_field_cal.x", Quantity::MagneticField, -9).on(Axis::X),
        Channel::new("magnetic_field_cal.y", Quantity::MagneticField, -9).on(Axis::Y),
        Channel::new("magnetic_field_cal.z", Quantity::MagneticField, -9).on(Axis::Z),
    ],
    &[],
);

/// The raw channels come first.
const RAW: usize = 3;

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
        // The calibrated channels are the driver's, not the kernel's.
        None,
        None,
        None,
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

/// Rounds a field in µT to nT, half away from zero (no `libm`: `no_std`).
#[cfg(feature = "float")]
fn nt(ut: f32) -> i32 {
    let x = ut * 1_000.0;
    if x >= 0.0 {
        (x + 0.5) as i32
    } else {
        (x - 0.5) as i32
    }
}

impl<I2C: embedded_hal::i2c::I2c> lemnos_device::Sensor for Bmm150<I2C> {
    fn read(&mut self, out: &mut [i32]) -> Result<(), DeviceError<Self::Error>> {
        lemnos_device::check_buffer(&INFO, out)?;
        let field = self.read_fixed().map_err(DeviceError::Driver)?;
        fill(out, field.channels());
        #[cfg(feature = "float")]
        {
            // The calibration needs all three axes; an overflowed one has no
            // value to calibrate.
            let calibrated = match (field.x_ut16, field.y_ut16, field.z_ut16) {
                (Some(x), Some(y), Some(z)) => {
                    let ut = [x as f32 / 16.0, y as f32 / 16.0, z as f32 / 16.0];
                    let period = self.period_us();
                    Some(self.cal.sample(period, ut))
                }
                _ => None,
            };
            for (i, slot) in out.iter_mut().skip(RAW).take(3).enumerate() {
                *slot = match calibrated {
                    Some(c) => nt(c[i]),
                    None => NO_VALUE,
                };
            }
        }
        #[cfg(not(feature = "float"))]
        for slot in out.iter_mut().skip(RAW).take(3) {
            *slot = lemnos_device::NO_VALUE;
        }
        Ok(())
    }

    /// The calibration's status (feature `float`).
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
        // The calibration runs on the blocking driver only.
        for slot in out.iter_mut().skip(RAW).take(3) {
            *slot = lemnos_device::NO_VALUE;
        }
        Ok(())
    }
}
