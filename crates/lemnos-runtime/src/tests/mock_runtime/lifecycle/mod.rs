use super::super::support::{output_config, pwm_config, spi_config, uart_config};
use crate::{
    Runtime, RuntimeConfig, RuntimeError, RuntimeFailureCategory, RuntimeFailureOperation,
};
use lemnos_core::{
    DeviceAddress, DeviceDescriptor, DeviceKind, DeviceStateSnapshot, GpioLevel,
    InteractionRequest, InterfaceKind, PwmPolarity, StandardRequest,
};
use lemnos_discovery::DiscoveryContext;
use lemnos_driver_manifest::{DriverManifest, DriverPriority};
use lemnos_driver_sdk::{BoundDevice, Driver, DriverBindContext, DriverResult};
use std::borrow::Cow;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

mod inventory;
mod teardown;
mod transports;

struct FailingGpioProbe;

impl lemnos_discovery::DiscoveryProbe for FailingGpioProbe {
    fn name(&self) -> &'static str {
        "failing-gpio"
    }

    fn interfaces(&self) -> &'static [InterfaceKind] {
        &[InterfaceKind::Gpio]
    }

    fn discover(
        &self,
        _context: &DiscoveryContext,
    ) -> lemnos_discovery::DiscoveryResult<lemnos_discovery::ProbeDiscovery> {
        Err(lemnos_discovery::DiscoveryError::ProbeFailed {
            probe: self.name().to_string(),
            message: "temporary probe failure".to_string(),
        })
    }
}

struct MetadataChangeProbe {
    generation: Arc<AtomicUsize>,
}

impl lemnos_discovery::DiscoveryProbe for MetadataChangeProbe {
    fn name(&self) -> &'static str {
        "metadata-change"
    }

    fn interfaces(&self) -> &'static [InterfaceKind] {
        &[InterfaceKind::Gpio]
    }

    fn discover(
        &self,
        _context: &DiscoveryContext,
    ) -> lemnos_discovery::DiscoveryResult<lemnos_discovery::ProbeDiscovery> {
        let generation = self.generation.load(Ordering::SeqCst);
        let descriptor =
            DeviceDescriptor::builder_for_kind("gpiochip0-line-21", DeviceKind::GpioLine)
                .expect("descriptor builder")
                .address(DeviceAddress::GpioLine {
                    chip_name: "gpiochip0".to_string(),
                    offset: 21,
                })
                .display_name(format!("metadata generation {generation}"))
                .label("generation", generation.to_string())
                .build()
                .expect("descriptor");
        Ok(lemnos_discovery::ProbeDiscovery::new(vec![descriptor]))
    }
}

struct CountingDriver {
    bind_count: Arc<AtomicUsize>,
    close_count: Arc<AtomicUsize>,
}

impl Driver for CountingDriver {
    fn id(&self) -> &str {
        "test.gpio.counting"
    }

    fn interface(&self) -> InterfaceKind {
        InterfaceKind::Gpio
    }

    fn manifest_ref(&self) -> Cow<'static, DriverManifest> {
        Cow::Owned(
            DriverManifest::new(self.id(), "Counting GPIO", vec![InterfaceKind::Gpio])
                .with_priority(DriverPriority::Preferred)
                .with_kind(DeviceKind::GpioLine),
        )
    }

    fn bind(
        &self,
        device: &DeviceDescriptor,
        _context: &DriverBindContext<'_>,
    ) -> DriverResult<Box<dyn BoundDevice>> {
        self.bind_count.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(CountingBoundDevice {
            driver_id: self.id().to_string(),
            device: device.clone(),
            close_count: Arc::clone(&self.close_count),
        }))
    }
}

struct CountingBoundDevice {
    driver_id: String,
    device: DeviceDescriptor,
    close_count: Arc<AtomicUsize>,
}

impl BoundDevice for CountingBoundDevice {
    fn device(&self) -> &DeviceDescriptor {
        &self.device
    }

    fn driver_id(&self) -> &str {
        self.driver_id.as_str()
    }

    fn close(&mut self) -> DriverResult<()> {
        self.close_count.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
