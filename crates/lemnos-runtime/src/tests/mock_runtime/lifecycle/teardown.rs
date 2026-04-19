use super::*;
use lemnos_discovery::InventorySnapshot;
use lemnos_driver_sdk::BoundDevice;
use lemnos_mock::{MockGpioLine, MockHardware};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::{thread, time::Duration};

const SLOW_CLOSE_DELAY_MS: u64 = 100;

struct CloseTrackingDriver {
    close_calls: Arc<AtomicUsize>,
}

impl Driver for CloseTrackingDriver {
    fn id(&self) -> &str {
        "test.gpio.close-tracking"
    }

    fn interface(&self) -> InterfaceKind {
        InterfaceKind::Gpio
    }

    fn manifest_ref(&self) -> Cow<'static, DriverManifest> {
        Cow::Owned(
            DriverManifest::new(self.id(), "Close tracking GPIO", vec![InterfaceKind::Gpio])
                .with_priority(DriverPriority::Preferred)
                .with_kind(lemnos_core::DeviceKind::GpioLine),
        )
    }

    fn bind(
        &self,
        device: &DeviceDescriptor,
        _context: &DriverBindContext<'_>,
    ) -> DriverResult<Box<dyn BoundDevice>> {
        Ok(Box::new(CloseTrackingBoundDevice {
            driver_id: self.id().to_string(),
            device: device.clone(),
            close_calls: Arc::clone(&self.close_calls),
        }))
    }
}

struct CloseTrackingBoundDevice {
    driver_id: String,
    device: DeviceDescriptor,
    close_calls: Arc<AtomicUsize>,
}

impl BoundDevice for CloseTrackingBoundDevice {
    fn device(&self) -> &DeviceDescriptor {
        &self.device
    }

    fn driver_id(&self) -> &str {
        self.driver_id.as_str()
    }

    fn close(&mut self) -> DriverResult<()> {
        self.close_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn state(&mut self) -> DriverResult<Option<DeviceStateSnapshot>> {
        Ok(Some(DeviceStateSnapshot::new(self.device.id.clone())))
    }
}

struct SlowCloseTrackingDriver {
    close_started: Arc<AtomicBool>,
}

impl Driver for SlowCloseTrackingDriver {
    fn id(&self) -> &str {
        "test.gpio.slow-close-tracking"
    }

    fn interface(&self) -> InterfaceKind {
        InterfaceKind::Gpio
    }

    fn manifest_ref(&self) -> Cow<'static, DriverManifest> {
        Cow::Owned(
            DriverManifest::new(
                self.id(),
                "Slow close tracking GPIO",
                vec![InterfaceKind::Gpio],
            )
            .with_priority(DriverPriority::Preferred)
            .with_kind(lemnos_core::DeviceKind::GpioLine),
        )
    }

    fn bind(
        &self,
        device: &DeviceDescriptor,
        _context: &DriverBindContext<'_>,
    ) -> DriverResult<Box<dyn BoundDevice>> {
        Ok(Box::new(SlowCloseTrackingBoundDevice {
            driver_id: self.id().to_string(),
            device: device.clone(),
            close_started: Arc::clone(&self.close_started),
        }))
    }
}

struct SlowCloseTrackingBoundDevice {
    driver_id: String,
    device: DeviceDescriptor,
    close_started: Arc<AtomicBool>,
}

impl BoundDevice for SlowCloseTrackingBoundDevice {
    fn device(&self) -> &DeviceDescriptor {
        &self.device
    }

    fn driver_id(&self) -> &str {
        self.driver_id.as_str()
    }

    fn close(&mut self) -> DriverResult<()> {
        self.close_started.store(true, Ordering::SeqCst);
        thread::sleep(Duration::from_millis(SLOW_CLOSE_DELAY_MS));
        Ok(())
    }

    fn state(&mut self) -> DriverResult<Option<DeviceStateSnapshot>> {
        Ok(Some(DeviceStateSnapshot::new(self.device.id.clone())))
    }
}

#[test]
fn runtime_unbind_closes_bound_device_before_drop() {
    let hardware = MockHardware::builder()
        .with_gpio_line(MockGpioLine::new("gpiochip0", 13).with_configuration(output_config()))
        .build();
    let device_id = hardware.descriptors()[0].id.clone();
    let close_calls = Arc::new(AtomicUsize::new(0));

    let mut runtime = Runtime::new();
    runtime
        .register_driver(CloseTrackingDriver {
            close_calls: Arc::clone(&close_calls),
        })
        .expect("register close-tracking driver");
    runtime
        .refresh(&DiscoveryContext::new(), &[&hardware])
        .expect("refresh");
    runtime.bind(&device_id).expect("bind");

    assert!(runtime.unbind(&device_id));
    assert_eq!(close_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn runtime_shutdown_closes_all_bound_devices() {
    let hardware = MockHardware::builder()
        .with_gpio_line(MockGpioLine::new("gpiochip0", 14).with_configuration(output_config()))
        .build();
    let device_id = hardware.descriptors()[0].id.clone();
    let close_calls = Arc::new(AtomicUsize::new(0));

    let mut runtime = Runtime::new();
    runtime
        .register_driver(CloseTrackingDriver {
            close_calls: Arc::clone(&close_calls),
        })
        .expect("register close-tracking driver");
    runtime
        .refresh(&DiscoveryContext::new(), &[&hardware])
        .expect("refresh");
    runtime.bind(&device_id).expect("bind");

    runtime.shutdown();
    assert_eq!(close_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn runtime_unbind_detached_removes_binding_before_slow_close_finishes() {
    let hardware = MockHardware::builder()
        .with_gpio_line(MockGpioLine::new("gpiochip0", 16).with_configuration(output_config()))
        .build();
    let device_id = hardware.descriptors()[0].id.clone();
    let close_started = Arc::new(AtomicBool::new(false));

    let mut runtime = Runtime::new();
    runtime
        .register_driver(SlowCloseTrackingDriver {
            close_started: Arc::clone(&close_started),
        })
        .expect("register slow close driver");
    runtime
        .refresh(&DiscoveryContext::new(), &[&hardware])
        .expect("refresh");
    runtime.bind(&device_id).expect("bind");
    assert!(runtime.has_state(&device_id));

    let (removed_anything, removed_binding, _, removed_state, _, detached) =
        runtime.unbind_detached(&device_id);
    assert!(removed_anything);
    assert!(removed_binding);
    assert!(removed_state);
    assert!(!runtime.is_bound(&device_id));
    assert!(!runtime.has_state(&device_id));

    let detached = detached.expect("detached binding");
    let worker = thread::spawn(move || crate::runtime::close_detached_bindings(vec![detached]));

    while !close_started.load(Ordering::SeqCst) {
        thread::yield_now();
    }

    assert!(!runtime.is_bound(&device_id));
    assert!(!runtime.has_state(&device_id));

    worker.join().expect("close worker");
}

#[test]
fn runtime_shutdown_detached_clears_runtime_before_slow_close_finishes() {
    let hardware = MockHardware::builder()
        .with_gpio_line(MockGpioLine::new("gpiochip0", 17).with_configuration(output_config()))
        .build();
    let device_id = hardware.descriptors()[0].id.clone();
    let close_started = Arc::new(AtomicBool::new(false));

    let mut runtime = Runtime::new();
    runtime
        .register_driver(SlowCloseTrackingDriver {
            close_started: Arc::clone(&close_started),
        })
        .expect("register slow close driver");
    runtime
        .refresh(&DiscoveryContext::new(), &[&hardware])
        .expect("refresh");
    runtime.bind(&device_id).expect("bind");

    let detached = runtime.shutdown_detached();
    assert!(!runtime.is_running());
    assert!(!runtime.is_bound(&device_id));
    assert!(!runtime.has_state(&device_id));
    assert!(runtime.failures().is_empty());

    let worker = thread::spawn(move || crate::runtime::close_detached_bindings(detached));

    while !close_started.load(Ordering::SeqCst) {
        thread::yield_now();
    }

    assert!(!runtime.is_running());
    assert!(!runtime.is_bound(&device_id));

    worker.join().expect("close worker");
}

#[test]
fn refresh_removal_closes_bound_devices_before_eviction() {
    let hardware = MockHardware::builder()
        .with_gpio_line(MockGpioLine::new("gpiochip0", 15).with_configuration(output_config()))
        .build();
    let device_id = hardware.descriptors()[0].id.clone();
    let close_calls = Arc::new(AtomicUsize::new(0));
    let empty_probe = MockHardware::builder().build();

    let mut runtime = Runtime::new();
    runtime
        .register_driver(CloseTrackingDriver {
            close_calls: Arc::clone(&close_calls),
        })
        .expect("register close-tracking driver");
    runtime
        .refresh(&DiscoveryContext::new(), &[&hardware])
        .expect("refresh");
    runtime.bind(&device_id).expect("bind");

    runtime
        .refresh(&DiscoveryContext::new(), &[&empty_probe])
        .expect("refresh after removal");

    assert_eq!(close_calls.load(Ordering::SeqCst), 1);
    assert!(!runtime.is_bound(&device_id));
}

#[test]
fn runtime_detach_removed_bindings_evicts_state_before_slow_close_finishes() {
    let hardware = MockHardware::builder()
        .with_gpio_line(MockGpioLine::new("gpiochip0", 18).with_configuration(output_config()))
        .build();
    let device_id = hardware.descriptors()[0].id.clone();
    let close_started = Arc::new(AtomicBool::new(false));

    let mut runtime = Runtime::new();
    runtime
        .register_driver(SlowCloseTrackingDriver {
            close_started: Arc::clone(&close_started),
        })
        .expect("register slow close driver");
    runtime
        .refresh(&DiscoveryContext::new(), &[&hardware])
        .expect("refresh");
    runtime.bind(&device_id).expect("bind");

    let next_inventory = InventorySnapshot::default();
    let diff = runtime.inventory().diff(&next_inventory);
    let detached = runtime.detach_invalidated_bindings(&diff, &Default::default());
    assert_eq!(detached.len(), 1);
    assert!(!runtime.is_bound(&device_id));
    assert!(!runtime.has_state(&device_id));

    let worker = thread::spawn(move || crate::runtime::close_detached_bindings(detached));

    while !close_started.load(Ordering::SeqCst) {
        thread::yield_now();
    }

    assert!(!runtime.is_bound(&device_id));
    assert!(!runtime.has_state(&device_id));

    worker.join().expect("close worker");
}

#[test]
fn refresh_drops_bindings_for_removed_devices() {
    let hardware = MockHardware::builder()
        .with_gpio_line(MockGpioLine::new("gpiochip0", 1).with_configuration(output_config()))
        .build();
    let device_id = hardware.descriptors()[0].id.clone();

    let mut runtime = Runtime::new();
    runtime.set_gpio_backend(hardware.clone());
    runtime
        .register_driver(lemnos_drivers_gpio::GpioDriver)
        .expect("register driver");
    runtime
        .refresh(&DiscoveryContext::new(), &[&hardware])
        .expect("refresh");
    runtime.bind(&device_id).expect("bind");
    assert!(runtime.is_bound(&device_id));

    let empty = MockHardware::builder().build();
    runtime
        .refresh(&DiscoveryContext::new(), &[&empty])
        .expect("refresh empty inventory");

    assert!(!runtime.is_bound(&device_id));
    assert!(!runtime.inventory().contains(&device_id));
}
