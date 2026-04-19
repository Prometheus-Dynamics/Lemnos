use crate::transport::transport_error;
use crate::{DriverError, DriverResult};
use lemnos_bus::{
    GpioBusBackend, GpioSession, I2cBusBackend, I2cControllerSession, I2cSession, PwmBusBackend,
    PwmSession, SessionAccess, SpiBusBackend, SpiSession, UartBusBackend, UartSession,
    UsbBusBackend, UsbSession,
};
use lemnos_core::{DeviceDescriptor, InterfaceKind};

macro_rules! driver_bind_interfaces {
    ($macro:ident) => {
        $macro!(
            (
                gpio,
                with_gpio,
                GpioBusBackend,
                InterfaceKind::Gpio,
                open_gpio,
                GpioSession
            ),
            (
                pwm,
                with_pwm,
                PwmBusBackend,
                InterfaceKind::Pwm,
                open_pwm,
                PwmSession
            ),
            (
                i2c,
                with_i2c,
                I2cBusBackend,
                InterfaceKind::I2c,
                open_i2c,
                I2cSession
            ),
            (
                spi,
                with_spi,
                SpiBusBackend,
                InterfaceKind::Spi,
                open_spi,
                SpiSession
            ),
            (
                uart,
                with_uart,
                UartBusBackend,
                InterfaceKind::Uart,
                open_uart,
                UartSession
            ),
            (
                usb,
                with_usb,
                UsbBusBackend,
                InterfaceKind::Usb,
                open_usb,
                UsbSession
            ),
        );
    };
}

macro_rules! driver_bind_accessors {
    ($(($field:ident, $with:ident, $trait:ident, $kind:expr, $open:ident, $session:ident)),+ $(,)?) => {
        $(
            pub fn $with(mut self, backend: &'a dyn $trait) -> Self {
                self.$field = Some(backend);
                self
            }

            pub fn $field(&self, driver_id: &str) -> DriverResult<&'a dyn $trait> {
                self.$field.ok_or_else(|| DriverError::MissingBackend {
                    driver_id: driver_id.to_string(),
                    interface: $kind,
                })
            }

            pub fn $open(
                &self,
                driver_id: &str,
                device: &DeviceDescriptor,
                access: SessionAccess,
            ) -> DriverResult<Box<dyn $session>> {
                self.$field(driver_id)?
                    .$open(device, access)
                    .map_err(|source| transport_error(driver_id, &device.id, source))
            }
        )+
    };
}

/// Drivers bind against runtime-selected backends, so this context intentionally
/// carries trait-object references instead of pushing generic parameters through
/// every driver type and registry surface.
///
/// That dynamic-dispatch cost exists only at the runtime composition boundary.
/// It keeps driver registration and backend swapping practical without forcing
/// the entire public SDK surface into deeply nested generic parameters.
#[derive(Default, Clone, Copy)]
pub struct DriverBindContext<'a> {
    gpio: Option<&'a dyn GpioBusBackend>,
    pwm: Option<&'a dyn PwmBusBackend>,
    i2c: Option<&'a dyn I2cBusBackend>,
    spi: Option<&'a dyn SpiBusBackend>,
    uart: Option<&'a dyn UartBusBackend>,
    usb: Option<&'a dyn UsbBusBackend>,
}

impl<'a> DriverBindContext<'a> {
    driver_bind_interfaces!(driver_bind_accessors);

    pub fn open_i2c_controller(
        &self,
        driver_id: &str,
        owner: &DeviceDescriptor,
        bus: u32,
        access: SessionAccess,
    ) -> DriverResult<Box<dyn I2cControllerSession>> {
        self.i2c(driver_id)?
            .open_i2c_controller(owner, bus, access)
            .map_err(|source| transport_error(driver_id, &owner.id, source))
    }
}
