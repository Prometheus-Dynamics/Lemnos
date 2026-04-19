use super::{backend_debug, backend_info, backend_warn, *};
#[cfg(feature = "i2c")]
use crate::I2cDiscoveryProbe;
#[cfg(feature = "hotplug")]
use crate::LinuxHotplugWatcher;
#[cfg(feature = "spi")]
use crate::SpiDiscoveryProbe;
#[cfg(feature = "uart")]
use crate::UartDiscoveryProbe;
#[cfg(feature = "usb")]
use crate::UsbDiscoveryProbe;
use crate::{GpioDiscoveryProbe, LedDiscoveryProbe};
#[cfg(feature = "pwm")]
use crate::{HwmonDiscoveryProbe, PwmDiscoveryProbe};
use lemnos_discovery::{
    DiscoveryContext, DiscoveryProbe, DiscoveryResult, DiscoveryRunReport, run_probes,
};

macro_rules! linux_probe_constructor {
    ($(#[$meta:meta])* $method:ident -> $probe_ty:ty) => {
        $(#[$meta])*
        pub fn $method(&self) -> $probe_ty {
            <$probe_ty>::new(self.paths.clone())
        }
    };
}

impl LinuxBackend {
    linux_probe_constructor!(gpio_probe -> GpioDiscoveryProbe);

    linux_probe_constructor!(#[cfg(feature = "i2c")] i2c_probe -> I2cDiscoveryProbe);

    linux_probe_constructor!(led_probe -> LedDiscoveryProbe);

    linux_probe_constructor!(#[cfg(feature = "pwm")] pwm_probe -> PwmDiscoveryProbe);

    linux_probe_constructor!(#[cfg(feature = "pwm")] hwmon_probe -> HwmonDiscoveryProbe);

    linux_probe_constructor!(#[cfg(feature = "spi")] spi_probe -> SpiDiscoveryProbe);

    linux_probe_constructor!(#[cfg(feature = "uart")] uart_probe -> UartDiscoveryProbe);

    linux_probe_constructor!(#[cfg(feature = "usb")] usb_probe -> UsbDiscoveryProbe);

    pub fn with_probes<T>(&self, f: impl FnOnce(Vec<&dyn DiscoveryProbe>) -> T) -> T {
        let gpio = self.gpio_probe();
        let led = self.led_probe();
        #[cfg(feature = "pwm")]
        let pwm = self.pwm_probe();
        #[cfg(feature = "pwm")]
        let hwmon = self.hwmon_probe();
        #[cfg(feature = "i2c")]
        let i2c = self.i2c_probe();
        #[cfg(feature = "spi")]
        let spi = self.spi_probe();
        #[cfg(feature = "uart")]
        let uart = self.uart_probe();
        #[cfg(feature = "usb")]
        let usb = self.usb_probe();

        #[cfg(any(
            feature = "pwm",
            feature = "i2c",
            feature = "spi",
            feature = "uart",
            feature = "usb"
        ))]
        let mut probes = vec![&gpio as &dyn DiscoveryProbe, &led as &dyn DiscoveryProbe];
        #[cfg(not(any(
            feature = "pwm",
            feature = "i2c",
            feature = "spi",
            feature = "uart",
            feature = "usb"
        )))]
        let probes = vec![&gpio as &dyn DiscoveryProbe, &led as &dyn DiscoveryProbe];
        #[cfg(feature = "pwm")]
        {
            probes.push(&pwm as &dyn DiscoveryProbe);
            probes.push(&hwmon as &dyn DiscoveryProbe);
        }
        #[cfg(feature = "i2c")]
        probes.push(&i2c as &dyn DiscoveryProbe);
        #[cfg(feature = "spi")]
        probes.push(&spi as &dyn DiscoveryProbe);
        #[cfg(feature = "uart")]
        probes.push(&uart as &dyn DiscoveryProbe);
        #[cfg(feature = "usb")]
        probes.push(&usb as &dyn DiscoveryProbe);

        f(probes)
    }

    pub fn discover(&self, context: &DiscoveryContext) -> DiscoveryResult<DiscoveryRunReport> {
        backend_debug!(
            requested_interfaces = context.requested_interfaces.len(),
            "linux backend discovery starting"
        );
        let result = self.with_probes(|probes| run_probes(context, &probes));
        match &result {
            Ok(_report) => {
                backend_info!(
                    snapshot_size = _report.snapshot.len(),
                    probe_reports = _report.probe_reports.len(),
                    enrichment_reports = _report.enrichment_reports.len(),
                    "linux backend discovery completed"
                );
            }
            Err(_error) => {
                backend_warn!(error = %_error, "linux backend discovery failed");
            }
        }
        result
    }

    #[cfg(feature = "hotplug")]
    pub fn hotplug_watcher(&self) -> DiscoveryResult<LinuxHotplugWatcher> {
        LinuxHotplugWatcher::new(self.paths.clone())
    }
}
