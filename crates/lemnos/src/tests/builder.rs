use crate::Lemnos;
use crate::bus::{
    BusBackend, BusError, BusResult, GpioBusBackend, GpioSession, I2cBusBackend, I2cSession,
    PwmBusBackend, PwmSession, SessionAccess, SpiBusBackend, SpiSession, UartBusBackend,
    UartSession, UsbBusBackend, UsbSession,
};
use crate::core::{DeviceDescriptor, InterfaceKind};

struct NonCloneSharedBackend;

impl BusBackend for NonCloneSharedBackend {
    fn name(&self) -> &str {
        "non-clone-shared"
    }

    fn supported_interfaces(&self) -> &'static [InterfaceKind] {
        &[]
    }

    fn supports_device(&self, _device: &DeviceDescriptor) -> bool {
        false
    }
}

macro_rules! impl_non_clone_backend {
    ($(($trait:ident, $method:ident, $session:ident)),+ $(,)?) => {
        $(
            impl $trait for NonCloneSharedBackend {
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

impl_non_clone_backend! {
    (GpioBusBackend, open_gpio, GpioSession),
    (I2cBusBackend, open_i2c, I2cSession),
    (PwmBusBackend, open_pwm, PwmSession),
    (SpiBusBackend, open_spi, SpiSession),
    (UartBusBackend, open_uart, UartSession),
    (UsbBusBackend, open_usb, UsbSession),
}

#[test]
fn builder_accepts_owned_shared_backend_without_clone() {
    let lemnos = Lemnos::builder()
        .with_shared_backend(NonCloneSharedBackend)
        .build();
    assert_eq!(lemnos.inventory_len(), 0);
}
