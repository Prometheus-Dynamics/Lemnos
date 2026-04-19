use super::*;

#[tokio::test(flavor = "multi_thread")]
async fn async_runtime_same_device_bind_attempts_serialize_and_bind_once() {
    struct StaticProbe {
        descriptor: DeviceDescriptor,
    }

    impl DiscoveryProbe for StaticProbe {
        fn name(&self) -> &'static str {
            "static-serialized-bind-probe"
        }

        fn interfaces(&self) -> &'static [InterfaceKind] {
            &[InterfaceKind::Gpio]
        }

        fn discover(&self, _context: &DiscoveryContext) -> Result<ProbeDiscovery, DiscoveryError> {
            Ok(ProbeDiscovery::new(vec![self.descriptor.clone()]))
        }
    }

    struct CountingBindDriver {
        bind_calls: Arc<AtomicUsize>,
    }

    impl Driver for CountingBindDriver {
        fn id(&self) -> &str {
            "test.gpio.serialized-bind"
        }

        fn interface(&self) -> InterfaceKind {
            InterfaceKind::Gpio
        }

        fn manifest_ref(&self) -> Cow<'static, DriverManifest> {
            Cow::Owned(
                DriverManifest::new(self.id(), "Serialized bind GPIO", vec![InterfaceKind::Gpio])
                    .with_priority(DriverPriority::Preferred)
                    .with_kind(DeviceKind::GpioLine),
            )
        }

        fn bind(
            &self,
            device: &DeviceDescriptor,
            _context: &DriverBindContext<'_>,
        ) -> DriverResult<Box<dyn BoundDevice>> {
            self.bind_calls.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(100));
            Ok(Box::new(CountingBoundDevice {
                driver_id: self.id().to_string(),
                device: device.clone(),
            }))
        }
    }

    struct CountingBoundDevice {
        driver_id: String,
        device: DeviceDescriptor,
    }

    impl BoundDevice for CountingBoundDevice {
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
        DeviceDescriptor::builder_for_kind("async-serialized-bind-line", DeviceKind::GpioLine)
            .expect("descriptor builder")
            .build()
            .expect("descriptor");
    let device_id = descriptor.id.clone();

    let mut runtime = Runtime::new();
    let bind_calls = Arc::new(AtomicUsize::new(0));
    runtime
        .register_driver(CountingBindDriver {
            bind_calls: Arc::clone(&bind_calls),
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

    let first_runtime = runtime.clone();
    let second_runtime = runtime.clone();
    let first_id = device_id.clone();
    let second_id = device_id.clone();
    let (first, second) = tokio::join!(
        tokio::spawn(async move { first_runtime.bind(first_id).await }),
        tokio::spawn(async move { second_runtime.bind(second_id).await }),
    );

    first.expect("first bind task").expect("first bind");
    second.expect("second bind task").expect("second bind");

    assert_eq!(bind_calls.load(Ordering::SeqCst), 1);
    assert!(runtime.is_bound(&device_id));
}

#[tokio::test(flavor = "multi_thread")]
async fn async_runtime_inventory_reads_do_not_wait_for_watcher_probe_execution() {
    struct SlowProbe {
        started: Arc<AtomicBool>,
    }

    impl DiscoveryProbe for SlowProbe {
        fn name(&self) -> &'static str {
            "slow-watch-probe"
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
    let watcher = AsyncInventoryWatcher::new(StaticWatcher {
        events: vec![InventoryWatchEvent::new(
            "async-static-watcher",
            vec![InterfaceKind::Gpio],
            vec!["/sys/class/gpio/gpio31".into()],
        )],
    });
    let started = Arc::new(AtomicBool::new(false));
    let refresh_runtime = runtime.clone();
    let refresh = tokio::spawn({
        let started = Arc::clone(&started);
        async move {
            refresh_runtime
                .poll_watcher_and_refresh(
                    DiscoveryContext::new(),
                    vec![Arc::new(SlowProbe { started }) as Arc<dyn DiscoveryProbe>],
                    &watcher,
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
    .expect("inventory read should not wait on watcher probe execution")
    .expect("inventory read task");
    assert_eq!(read, 0);

    let report = refresh
        .await
        .expect("watch refresh task")
        .expect("watch refresh");
    assert!(report.is_some());
}

#[test]
fn async_runtime_bind_lock_table_prunes_stale_entries() {
    let runtime = AsyncRuntime::new();

    for index in 0..32 {
        let device_id = DeviceDescriptor::builder_for_kind(
            format!("async-bind-lock-prune-{index}"),
            DeviceKind::GpioLine,
        )
        .expect("descriptor builder")
        .build()
        .expect("descriptor")
        .id;
        drop(runtime.bind_lock(&device_id));
    }

    let sentinel =
        DeviceDescriptor::builder_for_kind("async-bind-lock-prune-sentinel", DeviceKind::GpioLine)
            .expect("descriptor builder")
            .build()
            .expect("descriptor")
            .id;
    let _sentinel_lock = runtime.bind_lock(&sentinel);

    let bind_lock_count = runtime
        .bind_locks
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .len();
    assert_eq!(bind_lock_count, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn async_runtime_subscription_status_checks_do_not_deadlock_waiters() {
    let hardware = MockHardware::builder()
        .with_gpio_line(
            MockGpioLine::new("gpiochip0", 29)
                .with_line_name("async-subscription-lock-order")
                .with_configuration(output_config()),
        )
        .build();

    let mut runtime = Runtime::new();
    runtime.set_gpio_backend(hardware.clone());
    runtime
        .register_driver(GpioDriver)
        .expect("register driver");
    let runtime = Arc::new(AsyncRuntime::from_runtime(runtime));

    let subscription = Arc::new(runtime.subscribe_from_start());
    let stop = Arc::new(AtomicBool::new(false));

    let waiter = {
        let subscription = Arc::clone(&subscription);
        tokio::spawn(async move {
            subscription
                .wait_and_poll_next(Some(Duration::from_secs(1)))
                .await
                .expect("wait_and_poll_next")
                .expect("event batch")
        })
    };

    let status_checker = {
        let runtime = Arc::clone(&runtime);
        let subscription = Arc::clone(&subscription);
        let stop = Arc::clone(&stop);
        tokio::spawn(async move {
            while !stop.load(Ordering::Relaxed) {
                let _ = subscription.has_pending();
                let _ = subscription.pending_count();
                let _ = subscription.is_stale();
                let _ = runtime.event_retention_stats();
                tokio::task::yield_now().await;
            }
        })
    };

    let refresh = tokio::time::timeout(
        Duration::from_millis(250),
        runtime.refresh(
            DiscoveryContext::new(),
            vec![Arc::new(hardware.clone()) as Arc<dyn DiscoveryProbe>],
        ),
    )
    .await
    .expect("refresh should complete while status checks are active")
    .expect("refresh");
    assert_eq!(refresh.diff.added.len(), 1);

    let events = tokio::time::timeout(Duration::from_millis(250), waiter)
        .await
        .expect("waiter should complete")
        .expect("wait task");
    assert_eq!(events.len(), 1);

    stop.store(true, Ordering::Relaxed);
    status_checker.await.expect("status checker task");
}

#[tokio::test(flavor = "multi_thread")]
async fn async_runtime_subscription_async_status_checks_do_not_block_refresh() {
    let hardware = MockHardware::builder()
        .with_gpio_line(
            MockGpioLine::new("gpiochip0", 31)
                .with_line_name("async-subscription-async-status")
                .with_configuration(output_config()),
        )
        .build();

    let mut runtime = Runtime::new();
    runtime.set_gpio_backend(hardware.clone());
    runtime
        .register_driver(GpioDriver)
        .expect("register driver");
    let runtime = Arc::new(AsyncRuntime::from_runtime(runtime));

    let subscription = Arc::new(
        runtime
            .subscribe_from_start_async()
            .await
            .expect("subscribe from start"),
    );
    let stop = Arc::new(AtomicBool::new(false));

    let waiter = {
        let subscription = Arc::clone(&subscription);
        tokio::spawn(async move {
            subscription
                .wait_and_poll_next(Some(Duration::from_secs(1)))
                .await
                .expect("wait_and_poll_next")
                .expect("event batch")
        })
    };

    let status_checker = {
        let runtime = Arc::clone(&runtime);
        let subscription = Arc::clone(&subscription);
        let stop = Arc::clone(&stop);
        tokio::spawn(async move {
            while !stop.load(Ordering::Relaxed) {
                let _ = subscription.cursor_async().await.expect("cursor");
                let _ = subscription.next_index_async().await.expect("next index");
                let _ = subscription.has_pending_async().await.expect("has pending");
                let _ = subscription
                    .pending_count_async()
                    .await
                    .expect("pending count");
                let _ = subscription.is_stale_async().await.expect("is stale");
                let _ = runtime
                    .event_retention_stats_async()
                    .await
                    .expect("event retention stats");
                tokio::task::yield_now().await;
            }
        })
    };

    let refresh = tokio::time::timeout(
        Duration::from_millis(250),
        runtime.refresh(
            DiscoveryContext::new(),
            vec![Arc::new(hardware.clone()) as Arc<dyn DiscoveryProbe>],
        ),
    )
    .await
    .expect("refresh should complete while async status checks are active")
    .expect("refresh");
    assert_eq!(refresh.diff.added.len(), 1);

    let events = tokio::time::timeout(Duration::from_millis(250), waiter)
        .await
        .expect("waiter should complete")
        .expect("wait task");
    assert_eq!(events.len(), 1);

    stop.store(true, Ordering::Relaxed);
    status_checker.await.expect("status checker task");
}

#[tokio::test(flavor = "multi_thread")]
async fn async_runtime_refresh_state_shared_reuses_cached_snapshot_arc() {
    let hardware = MockHardware::builder()
        .with_gpio_line(
            MockGpioLine::new("gpiochip0", 30)
                .with_line_name("async-shared-state")
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
        .expect("refresh");
    runtime.bind(device_id.clone()).await.expect("bind");

    let refreshed = runtime
        .refresh_state_shared(device_id.clone())
        .await
        .expect("refresh shared state")
        .expect("shared state");
    let cached = runtime
        .shared_state(&device_id)
        .expect("cached shared state");

    assert!(Arc::ptr_eq(&refreshed, &cached));
}
