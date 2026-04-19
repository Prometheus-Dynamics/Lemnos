use super::{backend_debug, backend_info, backend_warn, *};
use crate::transport::gpio;
#[cfg(feature = "i2c")]
use crate::transport::i2c;
#[cfg(feature = "pwm")]
use crate::transport::pwm;
#[cfg(feature = "spi")]
use crate::transport::spi;
#[cfg(feature = "uart")]
use crate::transport::uart;
#[cfg(feature = "usb")]
use crate::transport::usb;
use lemnos_bus::{BusBackend, BusResult, GpioBusBackend, GpioSession, SessionAccess};
#[cfg(feature = "i2c")]
use lemnos_bus::{I2cBusBackend, I2cControllerSession, I2cSession};
#[cfg(feature = "pwm")]
use lemnos_bus::{PwmBusBackend, PwmSession};
#[cfg(feature = "spi")]
use lemnos_bus::{SpiBusBackend, SpiSession};
#[cfg(feature = "uart")]
use lemnos_bus::{UartBusBackend, UartSession};
#[cfg(feature = "usb")]
use lemnos_bus::{UsbBusBackend, UsbSession};
use lemnos_core::DeviceDescriptor;

macro_rules! optional_support {
    ($feature:literal, $expr:expr) => {{
        #[cfg(feature = $feature)]
        {
            $expr
        }
        #[cfg(not(feature = $feature))]
        {
            false
        }
    }};
}

macro_rules! impl_session_backend {
    (
        $(#[$meta:meta])*
        $trait_name:ident::$method_name:ident => $session_trait:ident,
        $interface:literal,
        $open_expr:expr
    ) => {
        $(#[$meta])*
        impl $trait_name for LinuxBackend {
            fn $method_name(
                &self,
                device: &DeviceDescriptor,
                access: SessionAccess,
            ) -> BusResult<Box<dyn $session_trait>> {
                backend_debug!(
                    device_id = ?device.id,
                    access = ?access,
                    "linux {} session open starting",
                    $interface
                );
                let result = $open_expr(self, device, access);
                log_open_result($interface, device, access, &result);
                result
            }
        }
    };
}

impl BusBackend for LinuxBackend {
    fn name(&self) -> &str {
        BACKEND_NAME
    }

    fn supported_interfaces(&self) -> &'static [InterfaceKind] {
        Self::SUPPORTED_INTERFACES
    }

    fn supports_device(&self, device: &DeviceDescriptor) -> bool {
        let mut supported = gpio::supports_descriptor(device);
        supported |= optional_support!("pwm", pwm::supports_descriptor(device));
        supported |= optional_support!("i2c", i2c::supports_descriptor(device));
        supported |= optional_support!("spi", spi::supports_descriptor(device));
        supported |= optional_support!("uart", uart::supports_descriptor(device));
        supported |= optional_support!("usb", usb::supports_descriptor(device));
        supported
    }
}

impl_session_backend!(
    GpioBusBackend::open_gpio => GpioSession,
    "gpio",
    |backend: &LinuxBackend, device: &DeviceDescriptor, access: SessionAccess| {
        gpio::open_session(&backend.paths, &backend.transport_config, device, access)
    }
);

impl_session_backend!(
    #[cfg(feature = "pwm")]
    PwmBusBackend::open_pwm => PwmSession,
    "pwm",
    |backend: &LinuxBackend, device: &DeviceDescriptor, access: SessionAccess| {
        pwm::open_session(&backend.paths, &backend.transport_config, device, access)
    }
);

#[cfg(feature = "i2c")]
impl I2cBusBackend for LinuxBackend {
    fn open_i2c(
        &self,
        device: &DeviceDescriptor,
        access: SessionAccess,
    ) -> BusResult<Box<dyn I2cSession>> {
        backend_debug!(
            device_id = ?device.id,
            access = ?access,
            "linux i2c session open starting"
        );
        let result = i2c::open_session(&self.paths, device, access);
        log_open_result("i2c", device, access, &result);
        result
    }

    fn open_i2c_controller(
        &self,
        owner: &DeviceDescriptor,
        bus: u32,
        access: SessionAccess,
    ) -> BusResult<Box<dyn I2cControllerSession>> {
        backend_debug!(
            device_id = ?owner.id,
            bus = bus,
            access = ?access,
            "linux i2c controller session open starting"
        );
        let result = i2c::open_controller(&self.paths, owner, bus, access);
        match &result {
            Ok(_) => {
                backend_info!(
                    device_id = ?owner.id,
                    bus = bus,
                    access = ?access,
                    "linux i2c controller session opened"
                );
            }
            Err(_error) => {
                backend_warn!(
                    device_id = ?owner.id,
                    bus = bus,
                    access = ?access,
                    error = %_error,
                    "linux i2c controller session open failed"
                );
            }
        }
        result
    }
}

impl_session_backend!(
    #[cfg(feature = "spi")]
    SpiBusBackend::open_spi => SpiSession,
    "spi",
    |backend: &LinuxBackend, device: &DeviceDescriptor, access: SessionAccess| {
        spi::open_session(&backend.paths, device, access)
    }
);

impl_session_backend!(
    #[cfg(feature = "uart")]
    UartBusBackend::open_uart => UartSession,
    "uart",
    |backend: &LinuxBackend, device: &DeviceDescriptor, access: SessionAccess| {
        uart::open_session(&backend.paths, &backend.transport_config, device, access)
    }
);

impl_session_backend!(
    #[cfg(feature = "usb")]
    UsbBusBackend::open_usb => UsbSession,
    "usb",
    |backend: &LinuxBackend, device: &DeviceDescriptor, access: SessionAccess| {
        usb::open_session(&backend.paths, &backend.transport_config, device, access)
    }
);

fn log_open_result<T>(
    _interface: &'static str,
    _device: &DeviceDescriptor,
    _access: SessionAccess,
    result: &BusResult<T>,
) {
    match result {
        Ok(_) => {
            backend_info!(
                interface = _interface,
                device_id = ?_device.id,
                access = ?_access,
                "linux transport session opened"
            );
        }
        Err(_error) => {
            backend_warn!(
                interface = _interface,
                device_id = ?_device.id,
                access = ?_access,
                error = %_error,
                "linux transport session open failed"
            );
        }
    }
}
