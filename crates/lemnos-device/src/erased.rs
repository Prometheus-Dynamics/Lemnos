//! Object-safe forms of the device traits, with errors reduced to
//! [`ErrorKind`], so one table or service can hold many different drivers.
//!
//! Every [`Device`] is a [`DynDevice`], every [`Sensor`] a [`DynSensor`],
//! every [`Control`] a [`DynControl`], every device that is both a
//! [`DynSensorControl`], and every [`Control`] + [`Pixels`] a [`DynLight`]. [`DeviceRef`] (borrowed) and `BoxedDevice` (owned,
//! feature `alloc`) hold whichever of the three a device is and offer every
//! operation, answering [`ErrorKind::Unsupported`] for the ones it lacks.
//!
//! Import these traits only where you work with `dyn` devices: on a concrete
//! driver their method names overlap with [`Device`], [`Sensor`] and
//! [`Control`].

use crate::{Control, Device, DeviceInfo, MAX_CHANNELS, Pixels, Rgbw, Sensor};
#[cfg(feature = "reasons")]
use core::fmt::Write;
use embedded_hal::delay::DelayNs;
use lemnos_hal::{ErrorKind, HalError};

/// The object-safe form of [`Device`].
pub trait DynDevice {
    fn info(&self) -> &'static DeviceInfo;
    fn init(&mut self, delay: &mut dyn DelayNs) -> Result<(), ErrorKind>;

    /// [`init`](Self::init), writing why it failed (the driver's own error)
    /// to `why`, so a host can show more than the error kind.
    #[cfg(feature = "reasons")]
    fn init_why(&mut self, delay: &mut dyn DelayNs, why: &mut dyn Write) -> Result<(), ErrorKind> {
        let _ = why;
        self.init(delay)
    }
}

/// The object-safe form of [`Sensor`].
pub trait DynSensor: DynDevice {
    fn read(&mut self, out: &mut [i32]) -> Result<(), ErrorKind>;

    /// See [`Sensor::read_batch`].
    fn read_batch(&mut self, out: &mut [[i32; MAX_CHANNELS]]) -> Result<usize, ErrorKind>;

    /// [`read_batch`](Self::read_batch), writing why it failed to `why`.
    #[cfg(feature = "reasons")]
    fn read_batch_why(
        &mut self,
        out: &mut [[i32; MAX_CHANNELS]],
        why: &mut dyn Write,
    ) -> Result<usize, ErrorKind> {
        let _ = why;
        self.read_batch(out)
    }

    /// See [`Sensor::sample_period_us`].
    fn sample_period_us(&self) -> Option<u32>;

    /// See [`Sensor::select_channels`].
    fn select_channels(&mut self, mask: u64);

    /// [`read`](Self::read), writing why it failed to `why`.
    #[cfg(feature = "reasons")]
    fn read_why(&mut self, out: &mut [i32], why: &mut dyn Write) -> Result<(), ErrorKind> {
        let _ = why;
        self.read(out)
    }
}

/// The object-safe form of [`Control`].
pub trait DynControl: DynDevice {
    fn set(&mut self, index: usize, value: i32) -> Result<i32, ErrorKind>;
    fn get(&mut self, index: usize) -> Result<i32, ErrorKind>;
}

/// A device that is both a sensor and a control.
pub trait DynSensorControl: DynSensor + DynControl {}

/// The object-safe form of [`Pixels`].
pub trait DynPixels: DynDevice {
    fn pixel_count(&self) -> usize;
    fn show(&mut self, pixels: &[Rgbw]) -> Result<(), ErrorKind>;
}

/// A light: shows frames and has controls (brightness, colour).
pub trait DynLight: DynControl + DynPixels {}

impl<T: Device + ?Sized> DynDevice for T {
    fn info(&self) -> &'static DeviceInfo {
        Device::info(self)
    }

    fn init(&mut self, delay: &mut dyn DelayNs) -> Result<(), ErrorKind> {
        Device::init(self, delay).map_err(|error| error.kind())
    }

    #[cfg(feature = "reasons")]
    fn init_why(&mut self, delay: &mut dyn DelayNs, why: &mut dyn Write) -> Result<(), ErrorKind> {
        Device::init(self, delay).map_err(|error| {
            explain(&error, why);
            error.kind()
        })
    }
}

/// Writes why `error` happened: the driver's own account
/// ([`HalError::describe`]), or the device-model reason.
#[cfg(feature = "reasons")]
fn explain<E: HalError>(error: &crate::DeviceError<E>, why: &mut dyn Write) {
    let _ = match error {
        crate::DeviceError::Driver(driver) => driver.describe(why),
        other => write!(why, "{other}"),
    };
}

impl<T: Sensor + ?Sized> DynSensor for T {
    fn read(&mut self, out: &mut [i32]) -> Result<(), ErrorKind> {
        Sensor::read(self, out).map_err(|error| error.kind())
    }

    fn read_batch(&mut self, out: &mut [[i32; MAX_CHANNELS]]) -> Result<usize, ErrorKind> {
        Sensor::read_batch(self, out).map_err(|error| error.kind())
    }

    #[cfg(feature = "reasons")]
    fn read_batch_why(
        &mut self,
        out: &mut [[i32; MAX_CHANNELS]],
        why: &mut dyn Write,
    ) -> Result<usize, ErrorKind> {
        Sensor::read_batch(self, out).map_err(|error| {
            explain(&error, why);
            error.kind()
        })
    }

    fn sample_period_us(&self) -> Option<u32> {
        Sensor::sample_period_us(self)
    }

    fn select_channels(&mut self, mask: u64) {
        Sensor::select_channels(self, mask);
    }

    #[cfg(feature = "reasons")]
    fn read_why(&mut self, out: &mut [i32], why: &mut dyn Write) -> Result<(), ErrorKind> {
        Sensor::read(self, out).map_err(|error| {
            explain(&error, why);
            error.kind()
        })
    }
}

impl<T: Control + ?Sized> DynControl for T {
    fn set(&mut self, index: usize, value: i32) -> Result<i32, ErrorKind> {
        Control::set(self, index, value).map_err(|error| error.kind())
    }

    fn get(&mut self, index: usize) -> Result<i32, ErrorKind> {
        Control::get(self, index).map_err(|error| error.kind())
    }
}

impl<T: Sensor + Control + ?Sized> DynSensorControl for T {}

impl<T: Pixels + ?Sized> DynPixels for T {
    fn pixel_count(&self) -> usize {
        Pixels::pixel_count(self)
    }

    fn show(&mut self, pixels: &[Rgbw]) -> Result<(), ErrorKind> {
        Pixels::show(self, pixels).map_err(|error| error.kind())
    }
}

impl<T: Control + Pixels + ?Sized> DynLight for T {}

macro_rules! erased_ops {
    () => {
        /// The device's description.
        pub fn info(&self) -> &'static DeviceInfo {
            match self {
                Self::Sensor(d) => d.info(),
                Self::Control(d) => d.info(),
                Self::Both(d) => DynDevice::info(&**d),
                Self::Light(d) => DynDevice::info(&**d),
            }
        }

        /// See [`Device::init`].
        pub fn init(&mut self, delay: &mut dyn DelayNs) -> Result<(), ErrorKind> {
            match self {
                Self::Sensor(d) => d.init(delay),
                Self::Control(d) => d.init(delay),
                Self::Both(d) => DynDevice::init(&mut **d, delay),
                Self::Light(d) => DynDevice::init(&mut **d, delay),
            }
        }

        /// [`init`](Self::init), writing why it failed to `why`.
        #[cfg(feature = "reasons")]
        pub fn init_why(
            &mut self,
            delay: &mut dyn DelayNs,
            why: &mut dyn Write,
        ) -> Result<(), ErrorKind> {
            match self {
                Self::Sensor(d) => d.init_why(delay, why),
                Self::Control(d) => d.init_why(delay, why),
                Self::Both(d) => DynDevice::init_why(&mut **d, delay, why),
                Self::Light(d) => DynDevice::init_why(&mut **d, delay, why),
            }
        }

        /// [`read`](Self::read), writing why it failed to `why`.
        #[cfg(feature = "reasons")]
        pub fn read_why(&mut self, out: &mut [i32], why: &mut dyn Write) -> Result<(), ErrorKind> {
            match self {
                Self::Sensor(d) => d.read_why(out, why),
                Self::Both(d) => DynSensor::read_why(&mut **d, out, why),
                Self::Control(_) | Self::Light(_) => Err(ErrorKind::Unsupported),
            }
        }

        /// See [`Sensor::read`]; `Unsupported` for a control-only device.
        pub fn read(&mut self, out: &mut [i32]) -> Result<(), ErrorKind> {
            match self {
                Self::Sensor(d) => d.read(out),
                Self::Both(d) => DynSensor::read(&mut **d, out),
                Self::Control(_) | Self::Light(_) => Err(ErrorKind::Unsupported),
            }
        }

        /// [`read_batch`](Self::read_batch), writing why it failed to `why`.
        #[cfg(feature = "reasons")]
        pub fn read_batch_why(
            &mut self,
            out: &mut [[i32; MAX_CHANNELS]],
            why: &mut dyn Write,
        ) -> Result<usize, ErrorKind> {
            match self {
                Self::Sensor(d) => d.read_batch_why(out, why),
                Self::Both(d) => DynSensor::read_batch_why(&mut **d, out, why),
                Self::Control(_) | Self::Light(_) => Err(ErrorKind::Unsupported),
            }
        }

        /// See [`Sensor::read_batch`]; `Unsupported` for a control-only device.
        pub fn read_batch(&mut self, out: &mut [[i32; MAX_CHANNELS]]) -> Result<usize, ErrorKind> {
            match self {
                Self::Sensor(d) => d.read_batch(out),
                Self::Both(d) => DynSensor::read_batch(&mut **d, out),
                Self::Control(_) | Self::Light(_) => Err(ErrorKind::Unsupported),
            }
        }

        /// See [`Sensor::select_channels`]; a no-op for a control or light.
        pub fn select_channels(&mut self, mask: u64) {
            match self {
                Self::Sensor(d) => d.select_channels(mask),
                Self::Both(d) => DynSensor::select_channels(&mut **d, mask),
                Self::Control(_) | Self::Light(_) => {}
            }
        }

        /// See [`Sensor::sample_period_us`]; `None` for a control-only device.
        pub fn sample_period_us(&self) -> Option<u32> {
            match self {
                Self::Sensor(d) => d.sample_period_us(),
                Self::Both(d) => DynSensor::sample_period_us(&**d),
                Self::Control(_) | Self::Light(_) => None,
            }
        }

        /// See [`Control::set`]; `Unsupported` for a sensor-only device.
        pub fn set(&mut self, index: usize, value: i32) -> Result<i32, ErrorKind> {
            match self {
                Self::Control(d) => d.set(index, value),
                Self::Both(d) => DynControl::set(&mut **d, index, value),
                Self::Light(d) => DynControl::set(&mut **d, index, value),
                Self::Sensor(_) => Err(ErrorKind::Unsupported),
            }
        }

        /// See [`Control::get`]; `Unsupported` for a sensor-only device.
        pub fn get(&mut self, index: usize) -> Result<i32, ErrorKind> {
            match self {
                Self::Control(d) => d.get(index),
                Self::Both(d) => DynControl::get(&mut **d, index),
                Self::Light(d) => DynControl::get(&mut **d, index),
                Self::Sensor(_) => Err(ErrorKind::Unsupported),
            }
        }

        /// Shows a frame on a light; `Unsupported` for other devices.
        pub fn show(&mut self, pixels: &[Rgbw]) -> Result<(), ErrorKind> {
            match self {
                Self::Light(d) => DynPixels::show(&mut **d, pixels),
                _ => Err(ErrorKind::Unsupported),
            }
        }

        /// How many LEDs a light has (0 for other devices).
        pub fn pixel_count(&self) -> usize {
            match self {
                Self::Light(d) => DynPixels::pixel_count(&**d),
                _ => 0,
            }
        }

        /// Whether the device has channels to read.
        pub fn is_sensor(&self) -> bool {
            matches!(self, Self::Sensor(_) | Self::Both(_))
        }

        /// Whether the device accepts controls.
        pub fn is_control(&self) -> bool {
            !matches!(self, Self::Sensor(_))
        }

        /// Whether the device shows frames.
        pub fn is_light(&self) -> bool {
            matches!(self, Self::Light(_))
        }
    };
}

/// A borrowed device of any kind.
pub enum DeviceRef<'a> {
    Sensor(&'a mut dyn DynSensor),
    Control(&'a mut dyn DynControl),
    Both(&'a mut dyn DynSensorControl),
    Light(&'a mut dyn DynLight),
}

impl<'a> DeviceRef<'a> {
    pub fn sensor(device: &'a mut impl Sensor) -> Self {
        Self::Sensor(device)
    }

    pub fn control(device: &'a mut impl Control) -> Self {
        Self::Control(device)
    }

    pub fn both(device: &'a mut (impl Sensor + Control)) -> Self {
        Self::Both(device)
    }

    pub fn light(device: &'a mut (impl Control + Pixels)) -> Self {
        Self::Light(device)
    }

    erased_ops!();
}

impl core::fmt::Debug for DeviceRef<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_tuple("DeviceRef")
            .field(&self.info().model)
            .finish()
    }
}

#[cfg(feature = "alloc")]
mod boxed {
    use super::*;
    use alloc::boxed::Box;

    /// An owned device of any kind. Devices must be `Send` so a service can
    /// move them to the thread that polls them.
    pub enum BoxedDevice {
        Sensor(Box<dyn DynSensor + Send>),
        Control(Box<dyn DynControl + Send>),
        Both(Box<dyn DynSensorControl + Send>),
        Light(Box<dyn DynLight + Send>),
    }

    impl BoxedDevice {
        pub fn sensor(device: impl Sensor + Send + 'static) -> Self {
            Self::Sensor(Box::new(device))
        }

        pub fn control(device: impl Control + Send + 'static) -> Self {
            Self::Control(Box::new(device))
        }

        pub fn both(device: impl Sensor + Control + Send + 'static) -> Self {
            Self::Both(Box::new(device))
        }

        pub fn light(device: impl Control + Pixels + Send + 'static) -> Self {
            Self::Light(Box::new(device))
        }

        /// Borrows it as a [`DeviceRef`].
        pub fn as_ref(&mut self) -> DeviceRef<'_> {
            match self {
                Self::Sensor(d) => DeviceRef::Sensor(&mut **d),
                Self::Control(d) => DeviceRef::Control(&mut **d),
                Self::Both(d) => DeviceRef::Both(&mut **d),
                Self::Light(d) => DeviceRef::Light(&mut **d),
            }
        }

        erased_ops!();
    }

    impl core::fmt::Debug for BoxedDevice {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            f.debug_tuple("BoxedDevice")
                .field(&self.info().model)
                .finish()
        }
    }
}

#[cfg(feature = "alloc")]
pub use boxed::BoxedDevice;
