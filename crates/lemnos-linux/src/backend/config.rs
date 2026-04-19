use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinuxTransportConfig {
    pub uart_default_baud_rate: u32,
    pub uart_timeout_ms: u64,
    pub usb_timeout_ms: u64,
    pub sysfs_export_retries: usize,
    pub sysfs_export_delay_ms: u64,
}

impl Default for LinuxTransportConfig {
    fn default() -> Self {
        Self::new()
    }
}

impl LinuxTransportConfig {
    pub const DEFAULT_UART_BAUD_RATE: u32 = 115_200;
    pub const DEFAULT_UART_TIMEOUT_MS: u64 = 50;
    pub const DEFAULT_USB_TIMEOUT_MS: u64 = 100;
    pub const DEFAULT_SYSFS_EXPORT_RETRIES: usize = 10;
    pub const DEFAULT_SYSFS_EXPORT_DELAY_MS: u64 = 10;

    pub const fn new() -> Self {
        Self {
            uart_default_baud_rate: Self::DEFAULT_UART_BAUD_RATE,
            uart_timeout_ms: Self::DEFAULT_UART_TIMEOUT_MS,
            usb_timeout_ms: Self::DEFAULT_USB_TIMEOUT_MS,
            sysfs_export_retries: Self::DEFAULT_SYSFS_EXPORT_RETRIES,
            sysfs_export_delay_ms: Self::DEFAULT_SYSFS_EXPORT_DELAY_MS,
        }
    }

    pub const fn with_uart_default_baud_rate(mut self, uart_default_baud_rate: u32) -> Self {
        self.uart_default_baud_rate = uart_default_baud_rate;
        self
    }

    pub const fn with_uart_timeout_ms(mut self, uart_timeout_ms: u64) -> Self {
        self.uart_timeout_ms = uart_timeout_ms;
        self
    }

    pub fn with_uart_timeout(mut self, uart_timeout: Duration) -> Self {
        self.uart_timeout_ms = duration_to_millis_u64(uart_timeout);
        self
    }

    pub const fn with_usb_timeout_ms(mut self, usb_timeout_ms: u64) -> Self {
        self.usb_timeout_ms = usb_timeout_ms;
        self
    }

    pub fn with_usb_timeout(mut self, usb_timeout: Duration) -> Self {
        self.usb_timeout_ms = duration_to_millis_u64(usb_timeout);
        self
    }

    pub const fn with_sysfs_export_retries(mut self, sysfs_export_retries: usize) -> Self {
        self.sysfs_export_retries = sysfs_export_retries;
        self
    }

    pub const fn with_sysfs_export_delay_ms(mut self, sysfs_export_delay_ms: u64) -> Self {
        self.sysfs_export_delay_ms = sysfs_export_delay_ms;
        self
    }

    pub fn with_sysfs_export_delay(mut self, sysfs_export_delay: Duration) -> Self {
        self.sysfs_export_delay_ms = duration_to_millis_u64(sysfs_export_delay);
        self
    }

    pub fn uart_timeout(&self) -> Duration {
        Duration::from_millis(self.uart_timeout_ms)
    }

    pub fn usb_timeout(&self) -> Duration {
        Duration::from_millis(self.usb_timeout_ms)
    }

    pub fn sysfs_export_delay(&self) -> Duration {
        Duration::from_millis(self.sysfs_export_delay_ms)
    }

    pub const fn sysfs_export_wait_budget_ms(&self) -> u64 {
        match self
            .sysfs_export_retries
            .checked_mul(self.sysfs_export_delay_ms as usize)
        {
            Some(total) => total as u64,
            None => u64::MAX,
        }
    }
}

fn duration_to_millis_u64(duration: Duration) -> u64 {
    duration.as_millis().try_into().unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::LinuxTransportConfig;
    use std::time::Duration;

    #[test]
    fn default_sysfs_export_wait_budget_is_short_and_explicit() {
        let config = LinuxTransportConfig::new();
        assert_eq!(
            config.sysfs_export_wait_budget_ms(),
            LinuxTransportConfig::DEFAULT_SYSFS_EXPORT_RETRIES as u64
                * LinuxTransportConfig::DEFAULT_SYSFS_EXPORT_DELAY_MS
        );
        assert_eq!(config.sysfs_export_wait_budget_ms(), 100);
    }

    #[test]
    fn sysfs_export_wait_budget_tracks_runtime_overrides() {
        let config = LinuxTransportConfig::new()
            .with_sysfs_export_retries(40)
            .with_sysfs_export_delay_ms(5);
        assert_eq!(config.sysfs_export_wait_budget_ms(), 200);
    }

    #[test]
    fn duration_helpers_round_trip_runtime_timeouts() {
        let config = LinuxTransportConfig::new()
            .with_uart_timeout(Duration::from_millis(125))
            .with_usb_timeout(Duration::from_millis(250))
            .with_sysfs_export_delay(Duration::from_millis(15));

        assert_eq!(config.uart_timeout(), Duration::from_millis(125));
        assert_eq!(config.usb_timeout(), Duration::from_millis(250));
        assert_eq!(config.sysfs_export_delay(), Duration::from_millis(15));
    }
}
