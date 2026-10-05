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
#[cfg(feature = "i2c")]
use lemnos_bus::I2cControllerSession;
use lemnos_bus::{BusBackend, BusResult, GpioBusBackend, GpioSession, SessionAccess};
use lemnos_bus::{I2cBusBackend, I2cSession};
use lemnos_bus::{PwmBusBackend, PwmSession};
use lemnos_bus::{SpiBusBackend, SpiSession};
use lemnos_bus::{UartBusBackend, UartSession};
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

/// Implements a bus backend trait for a bus compiled out of this build: every
/// open fails with `UnsupportedInterface`, so `LinuxBackend` still satisfies
/// callers that need all six traits (such as the `lemnos` facade).
macro_rules! impl_disabled_backend {
    ($feature:literal, $trait_name:ident::$method_name:ident => $session_trait:ident, $interface:ident) => {
        #[cfg(not(feature = $feature))]
        impl $trait_name for LinuxBackend {
            fn $method_name(
                &self,
                _device: &DeviceDescriptor,
                _access: SessionAccess,
            ) -> BusResult<Box<dyn $session_trait>> {
                Err(lemnos_bus::BusError::UnsupportedInterface {
                    backend: BACKEND_NAME.to_string(),
                    interface: lemnos_core::InterfaceKind::$interface,
                })
            }
        }
    };
}

impl_disabled_backend!("pwm", PwmBusBackend::open_pwm => PwmSession, Pwm);
impl_disabled_backend!("i2c", I2cBusBackend::open_i2c => I2cSession, I2c);
impl_disabled_backend!("spi", SpiBusBackend::open_spi => SpiSession, Spi);
impl_disabled_backend!("uart", UartBusBackend::open_uart => UartSession, Uart);
impl_disabled_backend!("usb", UsbBusBackend::open_usb => UsbSession, Usb);

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

impl GpioBusBackend for LinuxBackend {
    fn open_gpio(
        &self,
        device: &DeviceDescriptor,
        access: SessionAccess,
    ) -> BusResult<Box<dyn GpioSession>> {
        backend_debug!(
            device_id = ?device.id,
            access = ?access,
            "linux gpio session open starting"
        );
        let result = gpio::open_session(&self.paths, &self.transport_config, device, access);
        log_open_result("gpio", device, access, &result);
        result
    }

    fn open_gpio_edge_stream(
        &self,
        device: &DeviceDescriptor,
        access: SessionAccess,
    ) -> BusResult<Box<dyn lemnos_bus::GpioEdgeStreamSession>> {
        let result = gpio::open_edge_stream(&self.paths, device, access);
        log_open_result("gpio-edge", device, access, &result);
        result
    }
}

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
