//! VCMs in the Lemnos device model (`lemnos-device`): a lens with one
//! control, `position`, in device steps.

use crate::{Vcm, VcmError, asynch};
use embedded_hal::delay::DelayNs;
use lemnos_device::{
    Control, ControlInfo, Device, DeviceClass, DeviceError, DeviceInfo, Quantity, check_control,
};

const fn lens(max: i32) -> [ControlInfo; 1] {
    [ControlInfo::new("position", Quantity::Position, 0, 0, max)]
}

static POSITION_8: [ControlInfo; 1] = lens(255);
static POSITION_10: [ControlInfo; 1] = lens(1023);
static POSITION_12: [ControlInfo; 1] = lens(4095);
static POSITION_16: [ControlInfo; 1] = lens(65535);
static INFO_8: DeviceInfo = DeviceInfo::new(DeviceClass::Lens, "VCM", &[], &POSITION_8);
static INFO_10: DeviceInfo = DeviceInfo::new(DeviceClass::Lens, "VCM", &[], &POSITION_10);
static INFO_12: DeviceInfo = DeviceInfo::new(DeviceClass::Lens, "VCM", &[], &POSITION_12);
static INFO_16: DeviceInfo = DeviceInfo::new(DeviceClass::Lens, "VCM", &[], &POSITION_16);

/// The description of a VCM whose positions have `bits` bits: one control,
/// `position`, from 0 to `2^bits - 1` (8, 10 and 12 bits exactly; other
/// widths up to 16 bits accept 0..=65535 and the driver clamps).
pub fn info_for_bits(bits: u8) -> &'static DeviceInfo {
    match bits {
        8 => &INFO_8,
        10 => &INFO_10,
        12 => &INFO_12,
        _ => &INFO_16,
    }
}

impl<I2C: embedded_hal::i2c::I2c> Device for Vcm<'_, I2C> {
    type Error = VcmError<I2C::Error>;

    fn info(&self) -> &'static DeviceInfo {
        info_for_bits(self.state.format.bits)
    }

    /// Runs [`Vcm::power_up`].
    fn init(&mut self, delay: &mut dyn DelayNs) -> Result<(), DeviceError<Self::Error>> {
        self.power_up(&mut &mut *delay).map_err(DeviceError::Driver)
    }
}

impl<I2C: embedded_hal::i2c::I2c> Control for Vcm<'_, I2C> {
    /// Moves the lens; returns the position written.
    fn set(&mut self, index: usize, value: i32) -> Result<i32, DeviceError<Self::Error>> {
        check_control(Device::info(self), index, value)?;
        self.move_to(value).map_err(DeviceError::Driver)
    }

    /// The last position written; `Unsupported` before the first move, since
    /// VCMs do not report their position.
    fn get(&mut self, index: usize) -> Result<i32, DeviceError<Self::Error>> {
        check_control(Device::info(self), index, 0)?;
        self.position().ok_or(DeviceError::Unsupported)
    }
}

impl<I2C: embedded_hal_async::i2c::I2c> lemnos_device::asynch::Device for asynch::Vcm<'_, I2C> {
    type Error = VcmError<I2C::Error>;

    fn info(&self) -> &'static DeviceInfo {
        info_for_bits(self.state.format.bits)
    }

    async fn init(
        &mut self,
        delay: &mut impl embedded_hal_async::delay::DelayNs,
    ) -> Result<(), DeviceError<Self::Error>> {
        self.power_up(delay).await.map_err(DeviceError::Driver)
    }
}

impl<I2C: embedded_hal_async::i2c::I2c> lemnos_device::asynch::Control for asynch::Vcm<'_, I2C> {
    async fn set(&mut self, index: usize, value: i32) -> Result<i32, DeviceError<Self::Error>> {
        check_control(lemnos_device::asynch::Device::info(self), index, value)?;
        self.move_to(value).await.map_err(DeviceError::Driver)
    }

    async fn get(&mut self, index: usize) -> Result<i32, DeviceError<Self::Error>> {
        check_control(lemnos_device::asynch::Device::info(self), index, 0)?;
        self.position().ok_or(DeviceError::Unsupported)
    }
}
