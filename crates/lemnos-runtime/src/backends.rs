use lemnos_bus::{
    GpioBusBackend, I2cBusBackend, PwmBusBackend, SpiBusBackend, UartBusBackend, UsbBusBackend,
};
use lemnos_driver_sdk::DriverBindContext;
use std::sync::Arc;

macro_rules! runtime_backend_interfaces {
    ($macro:ident) => {
        $macro!(
            (gpio, with_gpio, set_gpio, GpioBusBackend),
            (pwm, with_pwm, set_pwm, PwmBusBackend),
            (i2c, with_i2c, set_i2c, I2cBusBackend),
            (spi, with_spi, set_spi, SpiBusBackend),
            (uart, with_uart, set_uart, UartBusBackend),
            (usb, with_usb, set_usb, UsbBusBackend),
        );
    };
}

macro_rules! runtime_backend_setters {
    ($(($field:ident, $with:ident, $set:ident, $trait:ident)),+ $(,)?) => {
        $(
            pub fn $with<B>(mut self, backend: B) -> Self
            where
                B: $trait + 'static,
            {
                self.$set(backend);
                self
            }

            pub fn $set<B>(&mut self, backend: B)
            where
                B: $trait + 'static,
            {
                self.$field = Some(Arc::new(backend));
            }
        )+
    };
}

/// The runtime keeps boxed backend trait objects only at the composition
/// boundary, where callers need to swap in concrete Linux, mock, or custom
/// backends per interface at runtime.
///
/// This is an intentional runtime-flexibility tradeoff rather than an attempt
/// to keep backend composition fully monomorphized across the whole public API.
#[derive(Default, Clone)]
pub struct RuntimeBackends {
    gpio: Option<Arc<dyn GpioBusBackend>>,
    pwm: Option<Arc<dyn PwmBusBackend>>,
    i2c: Option<Arc<dyn I2cBusBackend>>,
    spi: Option<Arc<dyn SpiBusBackend>>,
    uart: Option<Arc<dyn UartBusBackend>>,
    usb: Option<Arc<dyn UsbBusBackend>>,
}

impl RuntimeBackends {
    runtime_backend_interfaces!(runtime_backend_setters);

    pub fn with_shared_backend<B>(mut self, backend: B) -> Self
    where
        B: GpioBusBackend
            + PwmBusBackend
            + I2cBusBackend
            + SpiBusBackend
            + UartBusBackend
            + UsbBusBackend
            + 'static,
    {
        let backend = Arc::new(backend);
        self.gpio = Some(Arc::clone(&backend) as Arc<dyn GpioBusBackend>);
        self.pwm = Some(Arc::clone(&backend) as Arc<dyn PwmBusBackend>);
        self.i2c = Some(Arc::clone(&backend) as Arc<dyn I2cBusBackend>);
        self.spi = Some(Arc::clone(&backend) as Arc<dyn SpiBusBackend>);
        self.uart = Some(Arc::clone(&backend) as Arc<dyn UartBusBackend>);
        self.usb = Some(backend as Arc<dyn UsbBusBackend>);
        self
    }

    pub(crate) fn bind_context(&self) -> DriverBindContext<'_> {
        let mut context = DriverBindContext::default();
        if let Some(backend) = self.gpio.as_deref() {
            context = context.with_gpio(backend);
        }
        if let Some(backend) = self.pwm.as_deref() {
            context = context.with_pwm(backend);
        }
        if let Some(backend) = self.i2c.as_deref() {
            context = context.with_i2c(backend);
        }
        if let Some(backend) = self.spi.as_deref() {
            context = context.with_spi(backend);
        }
        if let Some(backend) = self.uart.as_deref() {
            context = context.with_uart(backend);
        }
        if let Some(backend) = self.usb.as_deref() {
            context = context.with_usb(backend);
        }
        context
    }
}

#[cfg(test)]
mod tests {
    use super::RuntimeBackends;
    use lemnos_bus::{
        BusBackend, BusError, BusResult, GpioBusBackend, GpioSession, I2cBusBackend,
        I2cControllerSession, I2cSession, PwmBusBackend, PwmSession, SessionAccess, SpiBusBackend,
        SpiSession, UartBusBackend, UartSession, UsbBusBackend, UsbSession,
    };
    use lemnos_core::{DeviceDescriptor, InterfaceKind};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    struct CloneCountingBackend {
        clone_count: Arc<AtomicUsize>,
    }

    impl Clone for CloneCountingBackend {
        fn clone(&self) -> Self {
            self.clone_count.fetch_add(1, Ordering::SeqCst);
            Self {
                clone_count: Arc::clone(&self.clone_count),
            }
        }
    }

    impl BusBackend for CloneCountingBackend {
        fn name(&self) -> &str {
            "clone-counting"
        }

        fn supported_interfaces(&self) -> &'static [InterfaceKind] {
            &[]
        }

        fn supports_device(&self, _device: &DeviceDescriptor) -> bool {
            false
        }
    }

    macro_rules! impl_backend {
        ($(($trait:ident, $method:ident, $session:ident)),+ $(,)?) => {
            $(
                impl $trait for CloneCountingBackend {
                    fn $method(
                        &self,
                        device: &DeviceDescriptor,
                        _access: SessionAccess,
                    ) -> BusResult<Box<dyn $session>> {
                        Err(BusError::UnsupportedDevice {
                            backend: self.name().to_string(),
                            device_id: device.id.clone(),
                        })
                    }
                }
            )+
        };
    }

    impl_backend! {
        (GpioBusBackend, open_gpio, GpioSession),
        (PwmBusBackend, open_pwm, PwmSession),
        (SpiBusBackend, open_spi, SpiSession),
        (UartBusBackend, open_uart, UartSession),
        (UsbBusBackend, open_usb, UsbSession),
    }

    impl I2cBusBackend for CloneCountingBackend {
        fn open_i2c(
            &self,
            device: &DeviceDescriptor,
            _access: SessionAccess,
        ) -> BusResult<Box<dyn I2cSession>> {
            Err(BusError::UnsupportedDevice {
                backend: self.name().to_string(),
                device_id: device.id.clone(),
            })
        }

        fn open_i2c_controller(
            &self,
            owner: &DeviceDescriptor,
            _bus: u32,
            _access: SessionAccess,
        ) -> BusResult<Box<dyn I2cControllerSession>> {
            Err(BusError::UnsupportedDevice {
                backend: self.name().to_string(),
                device_id: owner.id.clone(),
            })
        }
    }

    #[test]
    fn with_shared_backend_reuses_one_backend_instance_without_cloning() {
        let clone_count = Arc::new(AtomicUsize::new(0));
        let backends = RuntimeBackends::default().with_shared_backend(CloneCountingBackend {
            clone_count: Arc::clone(&clone_count),
        });

        let _ = backends.bind_context();
        assert_eq!(clone_count.load(Ordering::SeqCst), 0);
    }
}
