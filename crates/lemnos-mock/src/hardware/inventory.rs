use super::*;

impl MockHardware {
    pub fn builder() -> MockHardwareBuilder {
        MockHardwareBuilder::default()
    }

    pub fn attach_gpio_line(&self, line: MockGpioLine) -> DeviceId {
        let line = MockGpioLineState::from(line);
        let device_id = line.descriptor.id.clone();
        self.state().gpio_lines.insert(device_id.clone(), line);
        device_id
    }

    pub fn attach_pwm_channel(&self, channel: MockPwmChannel) -> DeviceId {
        let channel = MockPwmChannelState::from(channel);
        let device_id = channel.descriptor.id.clone();
        self.state().pwm_channels.insert(device_id.clone(), channel);
        device_id
    }

    pub fn attach_i2c_device(&self, device: MockI2cDevice) -> DeviceId {
        let device = MockI2cDeviceState::from(device);
        let device_id = device.descriptor.id.clone();
        self.state().i2c_devices.insert(device_id.clone(), device);
        device_id
    }

    pub fn attach_spi_device(&self, device: MockSpiDevice) -> DeviceId {
        let device = MockSpiDeviceState::from(device);
        let device_id = device.descriptor.id.clone();
        self.state().spi_devices.insert(device_id.clone(), device);
        device_id
    }

    pub fn attach_uart_port(&self, port: MockUartPort) -> DeviceId {
        let port = MockUartPortState::from(port);
        let device_id = port.descriptor.id.clone();
        self.state().uart_ports.insert(device_id.clone(), port);
        device_id
    }

    pub fn attach_usb_device(&self, device: MockUsbDevice) -> DeviceId {
        let device = MockUsbDeviceState::from(device);
        let device_id = device.device_descriptor.id.clone();
        let mut state = self.state();
        for descriptor in
            std::iter::once(&device.device_descriptor).chain(device.interface_descriptors.iter())
        {
            state
                .usb_descriptor_owners
                .insert(descriptor.id.clone(), device_id.clone());
        }
        state.usb_devices.insert(device_id.clone(), device);
        device_id
    }

    pub fn remove_device(&self, device_id: &DeviceId) -> bool {
        let mut state = self.state();

        if state.gpio_lines.remove(device_id).is_some() {
            state.faults.clear_device(device_id);
            return true;
        }
        if state.pwm_channels.remove(device_id).is_some() {
            state.faults.clear_device(device_id);
            return true;
        }
        if state.i2c_devices.remove(device_id).is_some() {
            state.faults.clear_device(device_id);
            return true;
        }
        if state.spi_devices.remove(device_id).is_some() {
            state.faults.clear_device(device_id);
            return true;
        }
        if state.uart_ports.remove(device_id).is_some() {
            state.faults.clear_device(device_id);
            return true;
        }

        let Some(owner_id) = state.usb_descriptor_owners.get(device_id).cloned() else {
            return false;
        };
        let Some(device) = state.usb_devices.remove(&owner_id) else {
            return false;
        };
        for descriptor in
            std::iter::once(&device.device_descriptor).chain(device.interface_descriptors.iter())
        {
            state.usb_descriptor_owners.remove(&descriptor.id);
            state.faults.clear_device(&descriptor.id);
        }
        state.faults.clear_device(&owner_id);
        true
    }

    pub fn queue_error(&self, device_id: &DeviceId, operation: impl Into<String>, error: BusError) {
        self.state()
            .faults
            .push(device_id.clone(), operation, error);
    }

    pub fn queue_timeout(&self, device_id: &DeviceId, operation: &'static str) {
        self.queue_error(
            device_id,
            operation,
            BusError::Timeout {
                device_id: device_id.clone(),
                operation,
            },
        );
    }

    pub fn queue_transport_failure(
        &self,
        device_id: &DeviceId,
        operation: &'static str,
        reason: impl Into<String>,
    ) {
        self.queue_error(
            device_id,
            operation,
            BusError::TransportFailure {
                device_id: device_id.clone(),
                operation,
                reason: reason.into(),
            },
        );
    }

    pub fn queue_disconnect(&self, device_id: &DeviceId, operation: &'static str) {
        self.queue_error(
            device_id,
            operation,
            BusError::Disconnected {
                device_id: device_id.clone(),
            },
        );
    }

    pub fn queue_script(&self, device_id: &DeviceId, script: MockFaultScript) {
        let mut state = self.state();
        for (operation, error) in script.into_entries(device_id) {
            state.faults.push(device_id.clone(), operation, error);
        }
    }

    pub fn clear_faults(&self, device_id: &DeviceId) {
        self.state().faults.clear_device(device_id);
    }

    pub fn inventory(&self) -> DiscoveryResult<InventorySnapshot> {
        InventorySnapshot::new(self.descriptors())
    }

    pub fn descriptors(&self) -> Vec<DeviceDescriptor> {
        let state = self.state();
        let mut descriptors: Vec<_> = state
            .gpio_lines
            .values()
            .map(|line| line.descriptor.clone())
            .chain(
                state
                    .pwm_channels
                    .values()
                    .map(|channel| channel.descriptor.clone()),
            )
            .chain(
                state
                    .i2c_devices
                    .values()
                    .map(|device| device.descriptor.clone()),
            )
            .chain(
                state
                    .spi_devices
                    .values()
                    .map(|device| device.descriptor.clone()),
            )
            .chain(
                state
                    .uart_ports
                    .values()
                    .map(|port| port.descriptor.clone()),
            )
            .chain(state.usb_devices.values().flat_map(|device| {
                std::iter::once(device.device_descriptor.clone())
                    .chain(device.interface_descriptors.iter().cloned())
            }))
            .collect();
        descriptors.sort_by(|left, right| left.id.cmp(&right.id));
        descriptors
    }

    pub fn gpio_level(&self, device_id: &DeviceId) -> Option<GpioLevel> {
        self.state()
            .gpio_lines
            .get(device_id)
            .map(|line| line.level)
    }

    pub fn gpio_configuration(&self, device_id: &DeviceId) -> Option<GpioLineConfiguration> {
        self.state()
            .gpio_lines
            .get(device_id)
            .map(|line| line.configuration.clone())
    }

    pub fn pwm_configuration(&self, device_id: &DeviceId) -> Option<PwmConfiguration> {
        self.state()
            .pwm_channels
            .get(device_id)
            .map(|channel| channel.configuration.clone())
    }

    pub fn i2c_bytes(&self, device_id: &DeviceId, offset: usize, length: usize) -> Option<Vec<u8>> {
        self.state().i2c_devices.get(device_id).map(|device| {
            (0..length)
                .map(|index| device.memory.get(offset + index).copied().unwrap_or(0))
                .collect()
        })
    }

    pub fn spi_configuration(&self, device_id: &DeviceId) -> Option<SpiConfiguration> {
        self.state()
            .spi_devices
            .get(device_id)
            .map(|device| device.configuration.clone())
    }

    pub fn spi_last_write(&self, device_id: &DeviceId) -> Option<Vec<u8>> {
        self.state()
            .spi_devices
            .get(device_id)
            .map(|device| device.last_write.clone())
    }

    pub fn uart_configuration(&self, device_id: &DeviceId) -> Option<UartConfiguration> {
        self.state()
            .uart_ports
            .get(device_id)
            .map(|port| port.configuration.clone())
    }

    pub fn uart_tx_bytes(&self, device_id: &DeviceId) -> Option<Vec<u8>> {
        self.state()
            .uart_ports
            .get(device_id)
            .map(|port| port.tx_buffer.clone())
    }

    pub fn uart_rx_bytes(&self, device_id: &DeviceId) -> Option<Vec<u8>> {
        self.state()
            .uart_ports
            .get(device_id)
            .map(|port| port.rx_buffer.clone())
    }

    pub fn usb_claimed_interfaces(&self, device_id: &DeviceId) -> Option<Vec<u8>> {
        self.state().usb_devices.get(device_id).map(|device| {
            device
                .claimed_interfaces
                .keys()
                .copied()
                .collect::<Vec<_>>()
        })
    }

    pub fn usb_last_bulk_write(&self, device_id: &DeviceId, endpoint: u8) -> Option<Vec<u8>> {
        self.state()
            .usb_devices
            .get(device_id)
            .and_then(|device| device.last_bulk_writes.get(&endpoint).cloned())
    }

    pub fn usb_last_interrupt_write(&self, device_id: &DeviceId, endpoint: u8) -> Option<Vec<u8>> {
        self.state()
            .usb_devices
            .get(device_id)
            .and_then(|device| device.last_interrupt_writes.get(&endpoint).cloned())
    }

    pub fn usb_last_control_out(&self, device_id: &DeviceId) -> Option<UsbControlTransfer> {
        self.state()
            .usb_devices
            .get(device_id)
            .and_then(|device| device.last_control_out.clone())
    }

    pub(crate) fn state(&self) -> MutexGuard<'_, MockHardwareState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub(crate) fn clone_supported_entry<T: Clone>(
        &self,
        device_id: &DeviceId,
        lookup: impl FnOnce(&MockHardwareState) -> Option<&T>,
    ) -> BusResult<T> {
        let state = self.state();
        lookup(&state)
            .cloned()
            .ok_or_else(|| unsupported_device(device_id))
    }
}

impl DiscoveryProbe for MockHardware {
    fn name(&self) -> &'static str {
        MOCK_BACKEND_NAME
    }

    fn interfaces(&self) -> &'static [InterfaceKind] {
        &MOCK_INTERFACES
    }

    fn discover(&self, context: &DiscoveryContext) -> DiscoveryResult<ProbeDiscovery> {
        let devices = self
            .descriptors()
            .into_iter()
            .filter(|device| context.wants(device.interface))
            .collect::<Vec<_>>();
        Ok(ProbeDiscovery::new(devices).with_note("mock hardware inventory"))
    }
}
