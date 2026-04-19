use super::support::output_config;
use crate::{AsyncInventoryWatcher, AsyncRuntime, Runtime};
use lemnos_core::{
    DeviceAddress, DeviceDescriptor, DeviceKind, DeviceRequest, DeviceStateSnapshot, GpioLevel,
    GpioRequest, GpioResponse, InteractionRequest, InteractionResponse, InterfaceKind,
    StandardRequest, StandardResponse,
};
use lemnos_discovery::{
    DiscoveryContext, DiscoveryError, DiscoveryProbe, InventoryWatchEvent, InventoryWatcher,
    ProbeDiscovery,
};
use lemnos_driver_manifest::{DriverManifest, DriverPriority};
use lemnos_driver_sdk::{BoundDevice, Driver, DriverBindContext, DriverResult};
use lemnos_drivers_gpio::GpioDriver;
use lemnos_mock::{MockGpioLine, MockHardware};
use std::borrow::Cow;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

mod inventory_reads;

struct StaticWatcher {
    events: Vec<InventoryWatchEvent>,
}

impl InventoryWatcher for StaticWatcher {
    fn name(&self) -> &'static str {
        "async-static-watcher"
    }

    fn poll(&mut self) -> Result<Vec<InventoryWatchEvent>, DiscoveryError> {
        Ok(std::mem::take(&mut self.events))
    }
}

struct SlowCloseDriver {
    close_started: Arc<AtomicBool>,
}

impl Driver for SlowCloseDriver {
    fn id(&self) -> &str {
        "test.gpio.slow-close"
    }

    fn interface(&self) -> InterfaceKind {
        InterfaceKind::Gpio
    }

    fn manifest_ref(&self) -> Cow<'static, DriverManifest> {
        Cow::Owned(
            DriverManifest::new(self.id(), "Slow close GPIO", vec![InterfaceKind::Gpio])
                .with_priority(DriverPriority::Preferred)
                .with_kind(DeviceKind::GpioLine),
        )
    }

    fn bind(
        &self,
        device: &DeviceDescriptor,
        _context: &DriverBindContext<'_>,
    ) -> DriverResult<Box<dyn BoundDevice>> {
        Ok(Box::new(SlowCloseBoundDevice {
            driver_id: self.id().to_string(),
            device: device.clone(),
            close_started: Arc::clone(&self.close_started),
        }))
    }
}

struct SlowCloseBoundDevice {
    driver_id: String,
    device: DeviceDescriptor,
    close_started: Arc<AtomicBool>,
}

impl BoundDevice for SlowCloseBoundDevice {
    fn device(&self) -> &DeviceDescriptor {
        &self.device
    }

    fn driver_id(&self) -> &str {
        self.driver_id.as_str()
    }

    fn close(&mut self) -> DriverResult<()> {
        self.close_started.store(true, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(200));
        Ok(())
    }

    fn state(&mut self) -> DriverResult<Option<DeviceStateSnapshot>> {
        Ok(Some(DeviceStateSnapshot::new(self.device.id.clone())))
    }
}

struct MetadataChangeProbe {
    generation: Arc<AtomicUsize>,
}

impl DiscoveryProbe for MetadataChangeProbe {
    fn name(&self) -> &'static str {
        "async-metadata-change"
    }

    fn interfaces(&self) -> &'static [InterfaceKind] {
        &[InterfaceKind::Gpio]
    }

    fn discover(&self, _context: &DiscoveryContext) -> Result<ProbeDiscovery, DiscoveryError> {
        let generation = self.generation.load(Ordering::SeqCst);
        let descriptor =
            DeviceDescriptor::builder_for_kind("gpiochip0-line-29", DeviceKind::GpioLine)
                .expect("descriptor builder")
                .address(DeviceAddress::GpioLine {
                    chip_name: "gpiochip0".to_string(),
                    offset: 29,
                })
                .display_name(format!("async metadata generation {generation}"))
                .label("generation", generation.to_string())
                .build()
                .expect("descriptor");
        Ok(ProbeDiscovery::new(vec![descriptor]))
    }
}

struct CountingDriver {
    bind_count: Arc<AtomicUsize>,
    close_count: Arc<AtomicUsize>,
}

impl Driver for CountingDriver {
    fn id(&self) -> &str {
        "test.gpio.async-counting"
    }

    fn interface(&self) -> InterfaceKind {
        InterfaceKind::Gpio
    }

    fn manifest_ref(&self) -> Cow<'static, DriverManifest> {
        Cow::Owned(
            DriverManifest::new(self.id(), "Async counting GPIO", vec![InterfaceKind::Gpio])
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

#[tokio::test(flavor = "multi_thread")]
async fn async_runtime_waits_for_refresh_events_and_dispatches_requests() {
    let hardware = MockHardware::builder()
        .with_gpio_line(
            MockGpioLine::new("gpiochip0", 12)
                .with_line_name("async-line")
                .with_configuration(output_config()),
        )
        .build();
    let device_id = hardware.descriptors()[0].id.clone();

    let mut runtime = Runtime::new();
    runtime.set_gpio_backend(hardware.clone());
    runtime
        .register_driver(GpioDriver)
        .expect("register driver");
    let runtime = AsyncRuntime::from_runtime(runtime);

    let subscription = runtime.subscribe_from_start();
    let waiter = {
        let subscription = subscription.clone();
        tokio::spawn(async move {
            subscription
                .wait_and_poll_next(Some(Duration::from_secs(1)))
                .await
                .expect("wait_and_poll_next")
                .expect("event batch")
        })
    };

    runtime
        .refresh(
            DiscoveryContext::new(),
            vec![Arc::new(hardware.clone()) as Arc<dyn DiscoveryProbe>],
        )
        .await
        .expect("refresh");
    let events = waiter.await.expect("wait task");
    assert_eq!(events.len(), 1);

    let response = runtime
        .request(DeviceRequest::new(
            device_id.clone(),
            InteractionRequest::Standard(StandardRequest::Gpio(GpioRequest::Write {
                level: GpioLevel::High,
            })),
        ))
        .await
        .expect("write request");
    assert!(matches!(
        response.interaction,
        lemnos_core::InteractionResponse::Standard(lemnos_core::StandardResponse::Gpio(
            lemnos_core::GpioResponse::Applied
        ))
    ));
    assert_eq!(hardware.gpio_level(&device_id), Some(GpioLevel::High));
    assert_eq!(
        runtime
            .state(&device_id)
            .expect("cached state")
            .telemetry
            .get("level"),
        Some(&"high".into())
    );
    let inventory = runtime.shared_inventory();
    assert_eq!(inventory.len(), 1);
    let shared_state = runtime.shared_state(&device_id).expect("shared state");
    assert_eq!(shared_state.telemetry.get("level"), Some(&"high".into()));
}

#[tokio::test(flavor = "multi_thread")]
async fn async_runtime_subscription_clones_share_one_cursor() {
    let hardware = MockHardware::builder()
        .with_gpio_line(
            MockGpioLine::new("gpiochip0", 17)
                .with_line_name("shared-cursor")
                .with_configuration(output_config()),
        )
        .build();

    let mut runtime = Runtime::new();
    runtime.set_gpio_backend(hardware.clone());
    runtime
        .register_driver(GpioDriver)
        .expect("register driver");
    let runtime = AsyncRuntime::from_runtime(runtime);

    let first = runtime.subscribe_from_start();
    let second = first.clone();

    runtime
        .refresh(
            DiscoveryContext::new(),
            vec![Arc::new(hardware.clone()) as Arc<dyn DiscoveryProbe>],
        )
        .await
        .expect("refresh");

    let first_events = first.poll().await.expect("first poll");
    assert_eq!(first_events.len(), 1);

    let second_events = second.poll().await.expect("second poll");
    assert!(second_events.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn async_runtime_waiter_does_not_block_refresh_progress() {
    let hardware = MockHardware::builder()
        .with_gpio_line(
            MockGpioLine::new("gpiochip0", 18)
                .with_line_name("async-deadlock-check")
                .with_configuration(output_config()),
        )
        .build();

    let mut runtime = Runtime::new();
    runtime.set_gpio_backend(hardware.clone());
    runtime
        .register_driver(GpioDriver)
        .expect("register driver");
    let runtime = AsyncRuntime::from_runtime(runtime);

    let subscription = runtime.subscribe_from_start();
    let waiter = {
        let subscription = subscription.clone();
        tokio::spawn(async move {
            subscription
                .wait_and_poll_next(Some(Duration::from_secs(1)))
                .await
                .expect("wait_and_poll_next")
        })
    };

    tokio::time::sleep(Duration::from_millis(50)).await;

    let refresh = tokio::time::timeout(
        Duration::from_millis(250),
        runtime.refresh(
            DiscoveryContext::new(),
            vec![Arc::new(hardware.clone()) as Arc<dyn DiscoveryProbe>],
        ),
    )
    .await
    .expect("refresh should not be blocked by waiter")
    .expect("refresh");
    assert_eq!(refresh.diff.added.len(), 1);

    let waited = tokio::time::timeout(Duration::from_millis(250), waiter)
        .await
        .expect("waiter should complete")
        .expect("wait task");
    assert_eq!(waited.expect("event batch").len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn async_runtime_refresh_keeps_bound_device_alive_across_metadata_only_changes() {
    let generation = Arc::new(AtomicUsize::new(0));
    let bind_count = Arc::new(AtomicUsize::new(0));
    let close_count = Arc::new(AtomicUsize::new(0));
    let probe = Arc::new(MetadataChangeProbe {
        generation: Arc::clone(&generation),
    }) as Arc<dyn DiscoveryProbe>;

    let mut runtime = Runtime::new();
    runtime
        .register_driver(CountingDriver {
            bind_count: Arc::clone(&bind_count),
            close_count: Arc::clone(&close_count),
        })
        .expect("register driver");
    let runtime = AsyncRuntime::from_runtime(runtime);

    let initial = runtime
        .refresh(DiscoveryContext::new(), vec![Arc::clone(&probe)])
        .await
        .expect("initial refresh");
    let device_id = initial.discovery.snapshot.devices[0].id.clone();
    runtime.bind(device_id.clone()).await.expect("bind");

    generation.store(1, Ordering::SeqCst);
    let refresh = runtime
        .refresh(DiscoveryContext::new(), vec![probe])
        .await
        .expect("metadata refresh");

    assert_eq!(refresh.diff.changed.len(), 1);
    assert!(refresh.rebinds.attempted.is_empty());
    assert!(runtime.is_bound(&device_id));
    assert_eq!(bind_count.load(Ordering::SeqCst), 1);
    assert_eq!(close_count.load(Ordering::SeqCst), 0);
    assert_eq!(
        runtime
            .inventory()
            .get(&device_id)
            .expect("updated descriptor")
            .labels
            .get("generation")
            .map(String::as_str),
        Some("1")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn async_runtime_waiter_does_not_block_request_progress() {
    let hardware = MockHardware::builder()
        .with_gpio_line(
            MockGpioLine::new("gpiochip0", 19)
                .with_line_name("async-request-check")
                .with_configuration(output_config()),
        )
        .build();
    let device_id = hardware.descriptors()[0].id.clone();

    let mut runtime = Runtime::new();
    runtime.set_gpio_backend(hardware.clone());
    runtime
        .register_driver(GpioDriver)
        .expect("register driver");
    let runtime = AsyncRuntime::from_runtime(runtime);

    runtime
        .refresh(
            DiscoveryContext::new(),
            vec![Arc::new(hardware.clone()) as Arc<dyn DiscoveryProbe>],
        )
        .await
        .expect("initial refresh");

    let subscription = runtime.subscribe();
    let waiter = {
        let subscription = subscription.clone();
        tokio::spawn(async move {
            subscription
                .wait_and_poll_next(Some(Duration::from_secs(1)))
                .await
                .expect("wait_and_poll_next")
        })
    };

    tokio::time::sleep(Duration::from_millis(50)).await;

    let response = tokio::time::timeout(
        Duration::from_millis(250),
        runtime.request(DeviceRequest::new(
            device_id.clone(),
            InteractionRequest::Standard(StandardRequest::Gpio(GpioRequest::Write {
                level: GpioLevel::High,
            })),
        )),
    )
    .await
    .expect("request should not be blocked by waiter")
    .expect("write request");
    assert!(matches!(
        response.interaction,
        lemnos_core::InteractionResponse::Standard(lemnos_core::StandardResponse::Gpio(
            lemnos_core::GpioResponse::Applied
        ))
    ));

    let waited = tokio::time::timeout(Duration::from_millis(250), waiter)
        .await
        .expect("waiter should complete")
        .expect("wait task");
    assert!(!waited.expect("event batch").is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn async_runtime_waiter_does_not_block_watcher_refresh_progress() {
    let hardware = MockHardware::builder().build();
    let line = MockGpioLine::new("gpiochip0", 23)
        .with_line_name("async-watch-check")
        .with_configuration(output_config());
    let device_id = line.descriptor().id.clone();

    let mut runtime = Runtime::new();
    runtime.set_gpio_backend(hardware.clone());
    runtime
        .register_driver(GpioDriver)
        .expect("register driver");
    let runtime = AsyncRuntime::from_runtime(runtime);

    runtime
        .refresh(
            DiscoveryContext::new(),
            vec![Arc::new(hardware.clone()) as Arc<dyn DiscoveryProbe>],
        )
        .await
        .expect("initial refresh");

    hardware.attach_gpio_line(line);
    let watcher = AsyncInventoryWatcher::new(StaticWatcher {
        events: vec![InventoryWatchEvent::new(
            "async-static-watcher",
            vec![InterfaceKind::Gpio],
            vec!["/sys/class/gpio/gpio23".into()],
        )],
    });

    let subscription = runtime.subscribe();
    let waiter = {
        let subscription = subscription.clone();
        tokio::spawn(async move {
            subscription
                .wait_and_poll_next(Some(Duration::from_secs(1)))
                .await
                .expect("wait_and_poll_next")
        })
    };

    tokio::time::sleep(Duration::from_millis(50)).await;

    let report = tokio::time::timeout(
        Duration::from_millis(250),
        runtime.poll_watcher_and_refresh(
            DiscoveryContext::new(),
            vec![Arc::new(hardware.clone()) as Arc<dyn DiscoveryProbe>],
            &watcher,
        ),
    )
    .await
    .expect("watch refresh should not be blocked by waiter")
    .expect("watch refresh")
    .expect("watch event report");
    assert_eq!(report.watch_events.len(), 1);
    assert!(runtime.inventory().contains(&device_id));

    let waited = tokio::time::timeout(Duration::from_millis(250), waiter)
        .await
        .expect("waiter should complete")
        .expect("wait task");
    assert!(!waited.expect("event batch").is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn async_runtime_polls_watcher_and_refreshes_inventory() {
    let hardware = MockHardware::builder().build();
    let line = MockGpioLine::new("gpiochip0", 22)
        .with_line_name("watch-line")
        .with_configuration(output_config());
    let device_id = line.descriptor().id.clone();

    let mut runtime = Runtime::new();
    runtime.set_gpio_backend(hardware.clone());
    runtime
        .register_driver(GpioDriver)
        .expect("register driver");
    let runtime = AsyncRuntime::from_runtime(runtime);

    runtime
        .refresh(
            DiscoveryContext::new(),
            vec![Arc::new(hardware.clone()) as Arc<dyn DiscoveryProbe>],
        )
        .await
        .expect("initial refresh");
    assert!(!runtime.contains_device(&device_id));

    hardware.attach_gpio_line(line);
    let watcher = AsyncInventoryWatcher::new(StaticWatcher {
        events: vec![InventoryWatchEvent::new(
            "async-static-watcher",
            vec![InterfaceKind::Gpio],
            vec!["/sys/class/gpio/gpio22".into()],
        )],
    });

    let report = runtime
        .poll_watcher_and_refresh(
            DiscoveryContext::new(),
            vec![Arc::new(hardware.clone()) as Arc<dyn DiscoveryProbe>],
            &watcher,
        )
        .await
        .expect("watch refresh")
        .expect("watch event report");
    assert_eq!(report.watch_events.len(), 1);
    assert!(runtime.contains_device(&device_id));
}
