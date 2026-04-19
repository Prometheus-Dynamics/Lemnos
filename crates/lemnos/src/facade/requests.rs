use super::*;

impl Lemnos {
    pub fn request(&mut self, request: DeviceRequest) -> RuntimeResult<DeviceResponse> {
        self.runtime.request(request)
    }

    pub fn request_standard(
        &mut self,
        device_id: impl Into<DeviceId>,
        request: StandardRequest,
    ) -> RuntimeResult<DeviceResponse> {
        self.runtime.request(DeviceRequest::new(
            device_id.into(),
            InteractionRequest::Standard(request),
        ))
    }

    impl_standard_request_helpers!(sync;
        (request_gpio, GpioRequest, Gpio),
        (request_pwm, PwmRequest, Pwm),
        (request_i2c, I2cRequest, I2c),
        (request_spi, SpiRequest, Spi),
        (request_uart, UartRequest, Uart),
        (request_usb, UsbRequest, Usb),
    );

    impl_simple_standard_request_helpers!(sync;
        (read_gpio, Gpio, GpioRequest::Read),
        (gpio_configuration, Gpio, GpioRequest::GetConfiguration),
        (pwm_configuration, Pwm, PwmRequest::GetConfiguration),
        (spi_configuration, Spi, SpiRequest::GetConfiguration),
        (uart_configuration, Uart, UartRequest::GetConfiguration),
    );

    impl_parameterized_standard_request_helpers!(sync;
        (write_gpio, (level: GpioLevel), Gpio, GpioRequest::Write { level }),
        (
            configure_gpio,
            (configuration: GpioLineConfiguration),
            Gpio,
            GpioRequest::Configure(configuration)
        ),
        (enable_pwm, (enabled: bool), Pwm, PwmRequest::Enable { enabled }),
        (
            configure_pwm,
            (configuration: PwmConfiguration),
            Pwm,
            PwmRequest::Configure(configuration)
        ),
        (
            set_pwm_period,
            (period_ns: u64),
            Pwm,
            PwmRequest::SetPeriod { period_ns }
        ),
        (
            set_pwm_duty_cycle,
            (duty_cycle_ns: u64),
            Pwm,
            PwmRequest::SetDutyCycle { duty_cycle_ns }
        ),
        (
            claim_usb_interface,
            (interface_number: u8, alternate_setting: Option<u8>),
            Usb,
            UsbRequest::ClaimInterface {
                interface_number,
                alternate_setting,
            }
        ),
        (
            release_usb_interface,
            (interface_number: u8),
            Usb,
            UsbRequest::ReleaseInterface { interface_number }
        ),
    );

    pub fn request_custom(
        &mut self,
        device_id: impl Into<DeviceId>,
        interaction_id: impl TryInto<InteractionId, Error = lemnos_core::CoreError>,
    ) -> RuntimeResult<DeviceResponse> {
        self.request_custom_with_input(device_id, interaction_id, None::<Value>)
    }

    pub fn request_custom_value(
        &mut self,
        device_id: impl Into<DeviceId>,
        interaction_id: impl TryInto<InteractionId, Error = lemnos_core::CoreError>,
        input: impl Into<Value>,
    ) -> RuntimeResult<DeviceResponse> {
        self.request_custom_with_input(device_id, interaction_id, Some(input.into()))
    }

    pub fn request_custom_with_input(
        &mut self,
        device_id: impl Into<DeviceId>,
        interaction_id: impl TryInto<InteractionId, Error = lemnos_core::CoreError>,
        input: impl Into<Option<Value>>,
    ) -> RuntimeResult<DeviceResponse> {
        let device_id = device_id.into();
        self.runtime.request(super::build_custom_request(
            device_id,
            interaction_id,
            input.into(),
        )?)
    }
}
