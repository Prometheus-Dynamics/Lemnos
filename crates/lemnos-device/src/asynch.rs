//! The async device traits, for drivers over embedded-hal-async. Same
//! contracts as [`crate::Device`], [`crate::Sensor`] and [`crate::Control`].

use crate::{DeviceError, DeviceInfo};
use embedded_hal_async::delay::DelayNs;
use lemnos_hal::HalError;

/// See [`crate::Device`].
#[allow(async_fn_in_trait)]
pub trait Device {
    type Error: HalError;

    fn info(&self) -> &'static DeviceInfo;

    async fn init(&mut self, delay: &mut impl DelayNs) -> Result<(), DeviceError<Self::Error>> {
        let _ = delay;
        Ok(())
    }
}

/// See [`crate::Sensor`].
#[allow(async_fn_in_trait)]
pub trait Sensor: Device {
    async fn read(&mut self, out: &mut [i32]) -> Result<(), DeviceError<Self::Error>>;
}

/// See [`crate::Control`].
#[allow(async_fn_in_trait)]
pub trait Control: Device {
    async fn set(&mut self, index: usize, value: i32) -> Result<i32, DeviceError<Self::Error>>;
    async fn get(&mut self, index: usize) -> Result<i32, DeviceError<Self::Error>>;
}
