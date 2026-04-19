use super::*;

mod locking;

#[tokio::test(flavor = "multi_thread")]
async fn async_runtime_summary_queries_avoid_snapshot_reads_for_common_checks() {
    let hardware = MockHardware::builder()
        .with_gpio_line(
            MockGpioLine::new("gpiochip0", 24)
                .with_line_name("async-summary-check")
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

    assert_eq!(runtime.inventory_len(), 0);
    assert!(!runtime.contains_device(&device_id));
    assert!(!runtime.has_state(&device_id));
    assert!(!runtime.has_failure(&device_id));

    runtime
        .refresh(
            DiscoveryContext::new(),
            vec![Arc::new(hardware.clone()) as Arc<dyn DiscoveryProbe>],
        )
        .await
        .expect("refresh");
    assert_eq!(runtime.inventory_len(), 1);
    assert!(runtime.contains_device(&device_id));

    runtime.bind(device_id.clone()).await.expect("bind");
    assert!(runtime.has_state(&device_id));
    assert!(!runtime.has_failure(&device_id));
}

#[tokio::test(flavor = "multi_thread")]
async fn async_runtime_inventory_reads_do_not_wait_for_probe_execution() {
    struct SlowProbe {
        started: Arc<AtomicBool>,
    }

    impl DiscoveryProbe for SlowProbe {
        fn name(&self) -> &'static str {
            "slow-probe"
        }

        fn interfaces(&self) -> &'static [InterfaceKind] {
            &[InterfaceKind::Gpio]
        }

        fn discover(&self, _context: &DiscoveryContext) -> Result<ProbeDiscovery, DiscoveryError> {
            self.started.store(true, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(200));
            Ok(ProbeDiscovery::default())
        }
    }

    let runtime = AsyncRuntime::new();
    let started = Arc::new(AtomicBool::new(false));
    let refresh_runtime = runtime.clone();
    let refresh = tokio::spawn({
        let started = Arc::clone(&started);
        async move {
            refresh_runtime
                .refresh(
                    DiscoveryContext::new(),
                    vec![Arc::new(SlowProbe { started }) as Arc<dyn DiscoveryProbe>],
                )
                .await
        }
    });

    while !started.load(Ordering::SeqCst) {
        tokio::task::yield_now().await;
    }

    let read = tokio::time::timeout(
        Duration::from_millis(50),
        tokio::task::spawn_blocking({
            let runtime = runtime.clone();
            move || runtime.inventory_len()
        }),
    )
    .await
    .expect("inventory read should not wait on probe execution")
    .expect("inventory read task");
    assert_eq!(read, 0);

    let refresh = refresh.await.expect("refresh task").expect("refresh");
    assert_eq!(refresh.discovery.snapshot.len(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn async_runtime_inventory_reads_do_not_wait_for_request_execution() {
    struct StaticProbe {
        descriptor: DeviceDescriptor,
    }

    impl DiscoveryProbe for StaticProbe {
        fn name(&self) -> &'static str {
            "static-slow-request-probe"
        }

        fn interfaces(&self) -> &'static [InterfaceKind] {
            &[InterfaceKind::Gpio]
        }

        fn discover(&self, _context: &DiscoveryContext) -> Result<ProbeDiscovery, DiscoveryError> {
            Ok(ProbeDiscovery::new(vec![self.descriptor.clone()]))
        }
    }

    struct SlowRequestDriver {
        started: Arc<AtomicBool>,
    }

    impl Driver for SlowRequestDriver {
        fn id(&self) -> &str {
            "test.gpio.slow-request"
        }

        fn interface(&self) -> InterfaceKind {
            InterfaceKind::Gpio
        }

        fn manifest_ref(&self) -> Cow<'static, DriverManifest> {
            Cow::Owned(
                DriverManifest::new(self.id(), "Slow request GPIO", vec![InterfaceKind::Gpio])
                    .with_priority(DriverPriority::Preferred)
                    .with_kind(DeviceKind::GpioLine),
            )
        }

        fn bind(
            &self,
            device: &DeviceDescriptor,
            _context: &DriverBindContext<'_>,
        ) -> DriverResult<Box<dyn BoundDevice>> {
            Ok(Box::new(SlowRequestBoundDevice {
                driver_id: self.id().to_string(),
                device: device.clone(),
                started: Arc::clone(&self.started),
            }))
        }
    }

    struct SlowRequestBoundDevice {
        driver_id: String,
        device: DeviceDescriptor,
        started: Arc<AtomicBool>,
    }

    impl BoundDevice for SlowRequestBoundDevice {
        fn device(&self) -> &DeviceDescriptor {
            &self.device
        }

        fn driver_id(&self) -> &str {
            self.driver_id.as_str()
        }

        fn state(&mut self) -> DriverResult<Option<DeviceStateSnapshot>> {
            Ok(Some(DeviceStateSnapshot::new(self.device.id.clone())))
        }

        fn execute(&mut self, _request: &InteractionRequest) -> DriverResult<InteractionResponse> {
            self.started.store(true, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(200));
            Ok(InteractionResponse::Standard(StandardResponse::Gpio(
                GpioResponse::Applied,
            )))
        }
    }

    let descriptor =
        DeviceDescriptor::builder_for_kind("async-slow-request-line", DeviceKind::GpioLine)
            .expect("descriptor builder")
            .build()
            .expect("descriptor");
    let device_id = descriptor.id.clone();

    let mut runtime = Runtime::new();
    let started = Arc::new(AtomicBool::new(false));
    runtime
        .register_driver(SlowRequestDriver {
            started: Arc::clone(&started),
        })
        .expect("register driver");
    let runtime = AsyncRuntime::from_runtime(runtime);

    runtime
        .refresh(
            DiscoveryContext::new(),
            vec![Arc::new(StaticProbe { descriptor }) as Arc<dyn DiscoveryProbe>],
        )
        .await
        .expect("refresh");

    let request_runtime = runtime.clone();
    let request = tokio::spawn(async move {
        request_runtime
            .request(DeviceRequest::new(
                device_id.clone(),
                InteractionRequest::Standard(StandardRequest::Gpio(GpioRequest::Read)),
            ))
            .await
    });

    while !started.load(Ordering::SeqCst) {
        tokio::task::yield_now().await;
    }

    let read = tokio::time::timeout(
        Duration::from_millis(50),
        tokio::task::spawn_blocking({
            let runtime = runtime.clone();
            move || runtime.inventory_len()
        }),
    )
    .await
    .expect("inventory read should not wait on request execution")
    .expect("inventory read task");
    assert_eq!(read, 1);

    let response = request.await.expect("request task").expect("request");
    assert!(matches!(
        response.interaction,
        InteractionResponse::Standard(StandardResponse::Gpio(GpioResponse::Applied))
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn async_runtime_inventory_reads_do_not_wait_for_auto_bind_execution() {
    struct StaticProbe {
        descriptor: DeviceDescriptor,
    }

    impl DiscoveryProbe for StaticProbe {
        fn name(&self) -> &'static str {
            "static-slow-bind-probe"
        }

        fn interfaces(&self) -> &'static [InterfaceKind] {
            &[InterfaceKind::Gpio]
        }

        fn discover(&self, _context: &DiscoveryContext) -> Result<ProbeDiscovery, DiscoveryError> {
            Ok(ProbeDiscovery::new(vec![self.descriptor.clone()]))
        }
    }

    struct SlowBindDriver {
        started: Arc<AtomicBool>,
    }

    impl Driver for SlowBindDriver {
        fn id(&self) -> &str {
            "test.gpio.slow-bind"
        }

        fn interface(&self) -> InterfaceKind {
            InterfaceKind::Gpio
        }

        fn manifest_ref(&self) -> Cow<'static, DriverManifest> {
            Cow::Owned(
                DriverManifest::new(self.id(), "Slow bind GPIO", vec![InterfaceKind::Gpio])
                    .with_priority(DriverPriority::Preferred)
                    .with_kind(DeviceKind::GpioLine),
            )
        }

        fn bind(
            &self,
            device: &DeviceDescriptor,
            _context: &DriverBindContext<'_>,
        ) -> DriverResult<Box<dyn BoundDevice>> {
            self.started.store(true, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(200));
            Ok(Box::new(SlowBindBoundDevice {
                driver_id: self.id().to_string(),
                device: device.clone(),
            }))
        }
    }

    struct SlowBindBoundDevice {
        driver_id: String,
        device: DeviceDescriptor,
    }

    impl BoundDevice for SlowBindBoundDevice {
        fn device(&self) -> &DeviceDescriptor {
            &self.device
        }

        fn driver_id(&self) -> &str {
            self.driver_id.as_str()
        }

        fn state(&mut self) -> DriverResult<Option<DeviceStateSnapshot>> {
            Ok(Some(DeviceStateSnapshot::new(self.device.id.clone())))
        }

        fn execute(&mut self, _request: &InteractionRequest) -> DriverResult<InteractionResponse> {
            Ok(InteractionResponse::Standard(StandardResponse::Gpio(
                GpioResponse::Applied,
            )))
        }
    }

    let descriptor =
        DeviceDescriptor::builder_for_kind("async-slow-bind-line", DeviceKind::GpioLine)
            .expect("descriptor builder")
            .build()
            .expect("descriptor");
    let device_id = descriptor.id.clone();

    let mut runtime = Runtime::new();
    let started = Arc::new(AtomicBool::new(false));
    runtime
        .register_driver(SlowBindDriver {
            started: Arc::clone(&started),
        })
        .expect("register driver");
    let runtime = AsyncRuntime::from_runtime(runtime);

    runtime
        .refresh(
            DiscoveryContext::new(),
            vec![Arc::new(StaticProbe { descriptor }) as Arc<dyn DiscoveryProbe>],
        )
        .await
        .expect("refresh");

    let request_runtime = runtime.clone();
    let request = tokio::spawn(async move {
        request_runtime
            .request(DeviceRequest::new(
                device_id.clone(),
                InteractionRequest::Standard(StandardRequest::Gpio(GpioRequest::Read)),
            ))
            .await
    });

    while !started.load(Ordering::SeqCst) {
        tokio::task::yield_now().await;
    }

    let read = tokio::time::timeout(
        Duration::from_millis(50),
        tokio::task::spawn_blocking({
            let runtime = runtime.clone();
            move || runtime.inventory_len()
        }),
    )
    .await
    .expect("inventory read should not wait on auto-bind execution")
    .expect("inventory read task");
    assert_eq!(read, 1);

    let response = request.await.expect("request task").expect("request");
    assert!(matches!(
        response.interaction,
        InteractionResponse::Standard(StandardResponse::Gpio(GpioResponse::Applied))
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn async_runtime_inventory_reads_do_not_wait_for_bind_execution() {
    struct StaticProbe {
        descriptor: DeviceDescriptor,
    }

    impl DiscoveryProbe for StaticProbe {
        fn name(&self) -> &'static str {
            "static-explicit-bind-probe"
        }

        fn interfaces(&self) -> &'static [InterfaceKind] {
            &[InterfaceKind::Gpio]
        }

        fn discover(&self, _context: &DiscoveryContext) -> Result<ProbeDiscovery, DiscoveryError> {
            Ok(ProbeDiscovery::new(vec![self.descriptor.clone()]))
        }
    }

    struct SlowBindDriver {
        started: Arc<AtomicBool>,
    }

    impl Driver for SlowBindDriver {
        fn id(&self) -> &str {
            "test.gpio.explicit-slow-bind"
        }

        fn interface(&self) -> InterfaceKind {
            InterfaceKind::Gpio
        }

        fn manifest_ref(&self) -> Cow<'static, DriverManifest> {
            Cow::Owned(
                DriverManifest::new(
                    self.id(),
                    "Explicit slow bind GPIO",
                    vec![InterfaceKind::Gpio],
                )
                .with_priority(DriverPriority::Preferred)
                .with_kind(DeviceKind::GpioLine),
            )
        }

        fn bind(
            &self,
            device: &DeviceDescriptor,
            _context: &DriverBindContext<'_>,
        ) -> DriverResult<Box<dyn BoundDevice>> {
            self.started.store(true, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(200));
            Ok(Box::new(SlowBindBoundDevice {
                driver_id: self.id().to_string(),
                device: device.clone(),
            }))
        }
    }

    struct SlowBindBoundDevice {
        driver_id: String,
        device: DeviceDescriptor,
    }

    impl BoundDevice for SlowBindBoundDevice {
        fn device(&self) -> &DeviceDescriptor {
            &self.device
        }

        fn driver_id(&self) -> &str {
            self.driver_id.as_str()
        }

        fn state(&mut self) -> DriverResult<Option<DeviceStateSnapshot>> {
            Ok(Some(DeviceStateSnapshot::new(self.device.id.clone())))
        }
    }

    let descriptor =
        DeviceDescriptor::builder_for_kind("async-explicit-slow-bind-line", DeviceKind::GpioLine)
            .expect("descriptor builder")
            .build()
            .expect("descriptor");
    let device_id = descriptor.id.clone();

    let mut runtime = Runtime::new();
    let started = Arc::new(AtomicBool::new(false));
    runtime
        .register_driver(SlowBindDriver {
            started: Arc::clone(&started),
        })
        .expect("register driver");
    let runtime = AsyncRuntime::from_runtime(runtime);

    runtime
        .refresh(
            DiscoveryContext::new(),
            vec![Arc::new(StaticProbe { descriptor }) as Arc<dyn DiscoveryProbe>],
        )
        .await
        .expect("refresh");

    let bind_runtime = runtime.clone();
    let bind_device_id = device_id.clone();
    let bind = tokio::spawn(async move { bind_runtime.bind(bind_device_id).await });

    while !started.load(Ordering::SeqCst) {
        tokio::task::yield_now().await;
    }

    let read = tokio::time::timeout(
        Duration::from_millis(50),
        tokio::task::spawn_blocking({
            let runtime = runtime.clone();
            move || runtime.inventory_len()
        }),
    )
    .await
    .expect("inventory read should not wait on bind execution")
    .expect("inventory read task");
    assert_eq!(read, 1);

    bind.await.expect("bind task").expect("bind");
    assert!(runtime.is_bound(&device_id));
}

#[tokio::test(flavor = "multi_thread")]
async fn async_runtime_inventory_reads_do_not_wait_for_unbind_close() {
    let descriptor =
        DeviceDescriptor::builder_for_kind("async-slow-close-line", DeviceKind::GpioLine)
            .expect("descriptor builder")
            .build()
            .expect("descriptor");
    let device_id = descriptor.id.clone();

    struct StaticProbe {
        descriptor: DeviceDescriptor,
    }

    impl DiscoveryProbe for StaticProbe {
        fn name(&self) -> &'static str {
            "static-slow-close-probe"
        }

        fn interfaces(&self) -> &'static [InterfaceKind] {
            &[InterfaceKind::Gpio]
        }

        fn discover(&self, _context: &DiscoveryContext) -> Result<ProbeDiscovery, DiscoveryError> {
            Ok(ProbeDiscovery::new(vec![self.descriptor.clone()]))
        }
    }

    let mut runtime = Runtime::new();
    let close_started = Arc::new(AtomicBool::new(false));
    runtime
        .register_driver(SlowCloseDriver {
            close_started: Arc::clone(&close_started),
        })
        .expect("register driver");
    let runtime = AsyncRuntime::from_runtime(runtime);

    runtime
        .refresh(
            DiscoveryContext::new(),
            vec![Arc::new(StaticProbe { descriptor }) as Arc<dyn DiscoveryProbe>],
        )
        .await
        .expect("refresh");
    runtime.bind(device_id.clone()).await.expect("bind");

    let unbind_runtime = runtime.clone();
    let unbind_device_id = device_id.clone();
    let unbind = tokio::spawn(async move { unbind_runtime.unbind(unbind_device_id).await });

    while !close_started.load(Ordering::SeqCst) {
        tokio::task::yield_now().await;
    }

    let read = tokio::time::timeout(
        Duration::from_millis(50),
        tokio::task::spawn_blocking({
            let runtime = runtime.clone();
            move || runtime.inventory_len()
        }),
    )
    .await
    .expect("inventory read should not wait on close during unbind")
    .expect("inventory read task");
    assert_eq!(read, 1);

    assert!(unbind.await.expect("unbind task").expect("unbind"));
    assert!(!runtime.is_bound(&device_id));
}

#[tokio::test(flavor = "multi_thread")]
async fn async_runtime_inventory_reads_do_not_wait_for_shutdown_close() {
    let descriptor =
        DeviceDescriptor::builder_for_kind("async-slow-shutdown-line", DeviceKind::GpioLine)
            .expect("descriptor builder")
            .build()
            .expect("descriptor");

    struct StaticProbe {
        descriptor: DeviceDescriptor,
    }

    impl DiscoveryProbe for StaticProbe {
        fn name(&self) -> &'static str {
            "static-slow-shutdown-probe"
        }

        fn interfaces(&self) -> &'static [InterfaceKind] {
            &[InterfaceKind::Gpio]
        }

        fn discover(&self, _context: &DiscoveryContext) -> Result<ProbeDiscovery, DiscoveryError> {
            Ok(ProbeDiscovery::new(vec![self.descriptor.clone()]))
        }
    }

    let device_id = descriptor.id.clone();
    let mut runtime = Runtime::new();
    let close_started = Arc::new(AtomicBool::new(false));
    runtime
        .register_driver(SlowCloseDriver {
            close_started: Arc::clone(&close_started),
        })
        .expect("register driver");
    let runtime = AsyncRuntime::from_runtime(runtime);

    runtime
        .refresh(
            DiscoveryContext::new(),
            vec![Arc::new(StaticProbe { descriptor }) as Arc<dyn DiscoveryProbe>],
        )
        .await
        .expect("refresh");
    runtime.bind(device_id).await.expect("bind");

    let shutdown_runtime = runtime.clone();
    let shutdown = tokio::spawn(async move { shutdown_runtime.shutdown_async().await });

    while !close_started.load(Ordering::SeqCst) {
        tokio::task::yield_now().await;
    }

    let read = tokio::time::timeout(
        Duration::from_millis(50),
        tokio::task::spawn_blocking({
            let runtime = runtime.clone();
            move || (runtime.inventory_len(), runtime.is_running())
        }),
    )
    .await
    .expect("inventory read should not wait on close during shutdown")
    .expect("inventory read task");
    assert_eq!(read.0, 1);
    assert!(!read.1);

    shutdown.await.expect("shutdown task").expect("shutdown");
    assert!(!runtime.is_running());
}
