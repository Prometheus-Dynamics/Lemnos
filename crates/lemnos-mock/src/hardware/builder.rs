use super::*;

impl MockHardwareBuilder {
    pub fn with_gpio_line(mut self, line: MockGpioLine) -> Self {
        self.gpio_lines.push(line);
        self
    }

    pub fn with_pwm_channel(mut self, channel: MockPwmChannel) -> Self {
        self.pwm_channels.push(channel);
        self
    }

    pub fn with_i2c_device(mut self, device: MockI2cDevice) -> Self {
        self.i2c_devices.push(device);
        self
    }

    pub fn with_spi_device(mut self, device: MockSpiDevice) -> Self {
        self.spi_devices.push(device);
        self
    }

    pub fn with_uart_port(mut self, port: MockUartPort) -> Self {
        self.uart_ports.push(port);
        self
    }

    pub fn with_usb_device(mut self, device: MockUsbDevice) -> Self {
        self.usb_devices.push(device);
        self
    }

    pub fn build(self) -> MockHardware {
        let gpio_lines = build_state_map(self.gpio_lines, MockGpioLineState::from, |line| {
            &line.descriptor.id
        });
        let pwm_channels =
            build_state_map(self.pwm_channels, MockPwmChannelState::from, |channel| {
                &channel.descriptor.id
            });
        let i2c_devices = build_state_map(self.i2c_devices, MockI2cDeviceState::from, |device| {
            &device.descriptor.id
        });
        let spi_devices = build_state_map(self.spi_devices, MockSpiDeviceState::from, |device| {
            &device.descriptor.id
        });
        let uart_ports = build_state_map(self.uart_ports, MockUartPortState::from, |port| {
            &port.descriptor.id
        });
        let usb_devices = self
            .usb_devices
            .into_iter()
            .map(MockUsbDeviceState::from)
            .map(|device| (device.device_descriptor.id.clone(), device))
            .collect::<BTreeMap<_, _>>();
        let usb_descriptor_owners = usb_devices
            .iter()
            .flat_map(|(device_id, device)| {
                std::iter::once((device_id.clone(), device_id.clone())).chain(
                    device
                        .interface_descriptors
                        .iter()
                        .map(|descriptor| (descriptor.id.clone(), device_id.clone())),
                )
            })
            .collect();

        MockHardware {
            state: Arc::new(Mutex::new(MockHardwareState {
                gpio_lines,
                pwm_channels,
                i2c_devices,
                spi_devices,
                uart_ports,
                usb_devices,
                usb_descriptor_owners,
                faults: MockFaultRegistry::default(),
            })),
        }
    }
}
