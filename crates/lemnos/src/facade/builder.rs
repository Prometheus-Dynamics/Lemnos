use super::*;

impl LemnosBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_config(mut self, config: RuntimeConfig) -> Self {
        self.runtime.set_config(config);
        self
    }

    pub fn with_backends(mut self, backends: RuntimeBackends) -> Self {
        self.runtime.set_backends(backends);
        self
    }

    pub fn with_driver<D>(mut self, driver: D) -> RuntimeResult<Self>
    where
        D: Driver + 'static,
    {
        self.runtime.register_driver(driver)?;
        Ok(self)
    }

    #[cfg(feature = "builtin-drivers")]
    pub fn with_builtin_drivers(mut self) -> RuntimeResult<Self> {
        BuiltInDriverBundle::register_into(&mut self.runtime)?;
        Ok(self)
    }

    impl_builder_backend_methods!(
        (with_gpio_backend, set_gpio_backend, GpioBusBackend),
        (with_pwm_backend, set_pwm_backend, PwmBusBackend),
        (with_i2c_backend, set_i2c_backend, I2cBusBackend),
        (with_spi_backend, set_spi_backend, SpiBusBackend),
        (with_uart_backend, set_uart_backend, UartBusBackend),
        (with_usb_backend, set_usb_backend, UsbBusBackend),
    );

    impl_shared_backend_methods!(builder; with_backends);
    impl_mock_backend_methods!(builder);
    impl_linux_backend_methods!(builder);

    pub fn build(self) -> Lemnos {
        Lemnos {
            runtime: self.runtime,
        }
    }

    #[cfg(feature = "tokio")]
    /// Build the additive Tokio-backed facade over the same synchronous runtime.
    ///
    /// Read-side queries remain directly callable without async waiting, while
    /// mutating operations still offload blocking runtime work onto Tokio's
    /// blocking pool.
    pub fn build_async(self) -> AsyncLemnos {
        self.build().into_async()
    }
}
