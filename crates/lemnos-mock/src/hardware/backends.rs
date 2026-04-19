use super::*;

impl BusBackend for MockHardware {
    fn name(&self) -> &str {
        MOCK_BACKEND_NAME
    }

    fn supported_interfaces(&self) -> &'static [InterfaceKind] {
        &MOCK_INTERFACES
    }

    fn supports_device(&self, device: &DeviceDescriptor) -> bool {
        let state = self.state();
        match device.interface {
            InterfaceKind::Gpio => state.gpio_lines.contains_key(&device.id),
            InterfaceKind::Pwm => state.pwm_channels.contains_key(&device.id),
            InterfaceKind::I2c => state.i2c_devices.contains_key(&device.id),
            InterfaceKind::Spi => state.spi_devices.contains_key(&device.id),
            InterfaceKind::Uart => state.uart_ports.contains_key(&device.id),
            InterfaceKind::Usb => state.usb_descriptor_owners.contains_key(&device.id),
        }
    }
}

impl GpioBusBackend for MockHardware {
    fn open_gpio(
        &self,
        device: &DeviceDescriptor,
        access: SessionAccess,
    ) -> BusResult<Box<dyn GpioSession>> {
        take_injected_error(&self.state, &device.id, "open")?;
        let line =
            self.clone_supported_entry(&device.id, |state| state.gpio_lines.get(&device.id))?;

        Ok(Box::new(MockGpioSession::new(
            Arc::clone(&self.state),
            line.descriptor,
            access,
        )))
    }
}

impl PwmBusBackend for MockHardware {
    fn open_pwm(
        &self,
        device: &DeviceDescriptor,
        access: SessionAccess,
    ) -> BusResult<Box<dyn PwmSession>> {
        take_injected_error(&self.state, &device.id, "open")?;
        let channel =
            self.clone_supported_entry(&device.id, |state| state.pwm_channels.get(&device.id))?;

        Ok(Box::new(MockPwmSession::new(
            Arc::clone(&self.state),
            channel.descriptor,
            access,
        )))
    }
}

impl I2cBusBackend for MockHardware {
    fn open_i2c(
        &self,
        device: &DeviceDescriptor,
        access: SessionAccess,
    ) -> BusResult<Box<dyn I2cSession>> {
        take_injected_error(&self.state, &device.id, "open")?;
        let i2c_device =
            self.clone_supported_entry(&device.id, |state| state.i2c_devices.get(&device.id))?;

        Ok(Box::new(MockI2cSession::new(
            Arc::clone(&self.state),
            i2c_device.descriptor,
            access,
        )))
    }

    fn open_i2c_controller(
        &self,
        owner: &DeviceDescriptor,
        bus: u32,
        access: SessionAccess,
    ) -> BusResult<Box<dyn I2cControllerSession>> {
        take_injected_error(&self.state, &owner.id, "open")?;
        Ok(Box::new(MockI2cControllerSession::new(
            Arc::clone(&self.state),
            owner.clone(),
            bus,
            access,
        )))
    }
}

impl SpiBusBackend for MockHardware {
    fn open_spi(
        &self,
        device: &DeviceDescriptor,
        access: SessionAccess,
    ) -> BusResult<Box<dyn SpiSession>> {
        take_injected_error(&self.state, &device.id, "open")?;
        let spi_device =
            self.clone_supported_entry(&device.id, |state| state.spi_devices.get(&device.id))?;

        Ok(Box::new(MockSpiSession::new(
            Arc::clone(&self.state),
            spi_device.descriptor,
            access,
        )))
    }
}

impl UartBusBackend for MockHardware {
    fn open_uart(
        &self,
        device: &DeviceDescriptor,
        access: SessionAccess,
    ) -> BusResult<Box<dyn UartSession>> {
        take_injected_error(&self.state, &device.id, "open")?;
        let uart_port =
            self.clone_supported_entry(&device.id, |state| state.uart_ports.get(&device.id))?;

        Ok(Box::new(MockUartSession::new(
            Arc::clone(&self.state),
            uart_port.descriptor,
            access,
        )))
    }
}

impl UsbBusBackend for MockHardware {
    fn open_usb(
        &self,
        device: &DeviceDescriptor,
        access: SessionAccess,
    ) -> BusResult<Box<dyn UsbSession>> {
        take_injected_error(&self.state, &device.id, "open")?;
        let state = self.state();
        let owner_id = state
            .usb_descriptor_owners
            .get(&device.id)
            .cloned()
            .ok_or_else(|| unsupported_device(&device.id))?;
        drop(state);

        Ok(Box::new(MockUsbSession::new(
            Arc::clone(&self.state),
            device.clone(),
            owner_id,
            access,
        )))
    }
}
