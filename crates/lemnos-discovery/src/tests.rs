use crate::run::parallel_worker_count;
use crate::*;
use lemnos_core::{
    ConfiguredDeviceModel, CoreResult, DeviceDescriptor, DeviceHealth, DeviceId, DeviceKind,
    InterfaceKind, TimestampMs, Value,
};
use std::hint::black_box;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::thread;
use std::time::{Duration, Instant};

struct StaticProbe {
    name: &'static str,
    interfaces: &'static [InterfaceKind],
    devices: Vec<DeviceDescriptor>,
}

impl DiscoveryProbe for StaticProbe {
    fn name(&self) -> &'static str {
        self.name
    }

    fn interfaces(&self) -> &'static [InterfaceKind] {
        self.interfaces
    }

    fn discover(&self, context: &DiscoveryContext) -> DiscoveryResult<ProbeDiscovery> {
        Ok(ProbeDiscovery::new(
            self.devices
                .iter()
                .filter(|device| context.wants(device.interface))
                .cloned()
                .collect(),
        ))
    }
}

struct FailingProbe;

impl DiscoveryProbe for FailingProbe {
    fn name(&self) -> &'static str {
        "failing"
    }

    fn interfaces(&self) -> &'static [InterfaceKind] {
        &[InterfaceKind::Usb]
    }

    fn discover(&self, _context: &DiscoveryContext) -> DiscoveryResult<ProbeDiscovery> {
        Err(DiscoveryError::ProbeFailed {
            probe: self.name().into(),
            message: "hardware unavailable".into(),
        })
    }
}

struct DelayedProbe {
    name: &'static str,
    interfaces: &'static [InterfaceKind],
    delay: Duration,
    devices: Vec<DeviceDescriptor>,
}

struct EmptyProbe {
    name: &'static str,
    interfaces: &'static [InterfaceKind],
}

impl DiscoveryProbe for EmptyProbe {
    fn name(&self) -> &'static str {
        self.name
    }

    fn interfaces(&self) -> &'static [InterfaceKind] {
        self.interfaces
    }

    fn discover(&self, _context: &DiscoveryContext) -> DiscoveryResult<ProbeDiscovery> {
        black_box(());
        Ok(ProbeDiscovery::new(Vec::new()))
    }
}

impl DiscoveryProbe for DelayedProbe {
    fn name(&self) -> &'static str {
        self.name
    }

    fn interfaces(&self) -> &'static [InterfaceKind] {
        self.interfaces
    }

    fn discover(&self, _context: &DiscoveryContext) -> DiscoveryResult<ProbeDiscovery> {
        thread::sleep(self.delay);
        Ok(ProbeDiscovery::new(self.devices.clone()))
    }
}

struct ConcurrentProbe {
    name: &'static str,
    interfaces: &'static [InterfaceKind],
    delay: Duration,
    inflight: Arc<AtomicUsize>,
    max_inflight: Arc<AtomicUsize>,
}

impl DiscoveryProbe for ConcurrentProbe {
    fn name(&self) -> &'static str {
        self.name
    }

    fn interfaces(&self) -> &'static [InterfaceKind] {
        self.interfaces
    }

    fn discover(&self, _context: &DiscoveryContext) -> DiscoveryResult<ProbeDiscovery> {
        let current = self.inflight.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_inflight.fetch_max(current, Ordering::SeqCst);
        thread::sleep(self.delay);
        self.inflight.fetch_sub(1, Ordering::SeqCst);
        Ok(ProbeDiscovery::new(Vec::new()))
    }
}

struct StaticConfiguredDevice(&'static str);

impl ConfiguredDeviceModel for StaticConfiguredDevice {
    fn configured_interfaces() -> &'static [InterfaceKind]
    where
        Self: Sized,
    {
        &[InterfaceKind::I2c]
    }

    fn configured_descriptors(&self) -> CoreResult<Vec<DeviceDescriptor>> {
        Ok(vec![
            DeviceDescriptor::builder_for_kind(self.0, DeviceKind::I2cDevice)?.build()?,
        ])
    }
}

struct HealthEnricher;

impl DiscoveryEnricher for HealthEnricher {
    fn name(&self) -> &'static str {
        "health-enricher"
    }

    fn interfaces(&self) -> &'static [InterfaceKind] {
        &[InterfaceKind::I2c]
    }

    fn enrich(
        &self,
        _context: &DiscoveryContext,
        snapshot: &InventorySnapshot,
    ) -> DiscoveryResult<EnrichmentOutput> {
        let devices = snapshot
            .devices
            .iter()
            .cloned()
            .map(|mut device| {
                if device.interface == InterfaceKind::I2c {
                    device.health = DeviceHealth::Degraded;
                    device.set_property("enriched", true);
                }
                device
            })
            .collect();
        Ok(EnrichmentOutput::new(
            InventorySnapshot::with_observed_at(devices, snapshot.observed_at)
                .expect("valid enriched snapshot"),
        )
        .with_note("mark i2c devices degraded"))
    }
}

#[test]
fn snapshot_rejects_duplicate_ids() {
    let device = DeviceFixtureBuilder::gpio_line("gpio.line0", "gpiochip0", 0)
        .expect("fixture")
        .build()
        .expect("descriptor");
    let err = InventorySnapshot::new(vec![device.clone(), device])
        .expect_err("duplicate ids should fail");

    assert!(matches!(err, DiscoveryError::DuplicateDeviceId { .. }));
}

#[test]
fn snapshot_diff_reports_added_changed_and_removed_devices() {
    let fixture = InventoryDiffFixtureBuilder::new()
        .current(
            InventoryFixtureBuilder::new()
                .with_gpio_line("gpio.line0", "gpiochip0", 0)
                .expect("gpio fixture")
                .with_i2c_device("i2c.dev0", 1, 0x40)
                .expect("i2c fixture"),
        )
        .next(
            InventoryFixtureBuilder::new()
                .with_fixture(
                    DeviceFixtureBuilder::gpio_line("gpio.line0", "gpiochip0", 0)
                        .expect("gpio fixture")
                        .health(DeviceHealth::Degraded),
                )
                .expect("changed gpio fixture")
                .with_uart_port("uart.port0", "/dev/ttyS0")
                .expect("uart fixture"),
        )
        .build()
        .expect("diff fixture");

    assert_eq!(fixture.current.len(), 2);
    assert_eq!(fixture.next.len(), 2);
    assert_eq!(fixture.diff.added.len(), 1);
    assert_eq!(fixture.diff.changed.len(), 1);
    assert_eq!(fixture.diff.removed.len(), 1);
    assert!(!fixture.diff.is_empty());
}

#[test]
fn snapshot_can_return_first_device_id_for_kind() {
    let snapshot = InventoryFixtureBuilder::new()
        .with_gpio_line("gpio.line0", "gpiochip0", 0)
        .expect("fixture")
        .build()
        .expect("snapshot");

    assert_eq!(
        snapshot
            .first_id_by_kind(DeviceKind::GpioLine)
            .expect("gpio line")
            .as_str(),
        "gpio.line0"
    );
}

#[test]
fn run_probes_merges_successes_and_reports_failures() {
    let gpio_probe = StaticProbe {
        name: "gpio",
        interfaces: &[InterfaceKind::Gpio],
        devices: vec![
            DeviceFixtureBuilder::gpio_line("gpio.line0", "gpiochip0", 0)
                .expect("fixture")
                .build()
                .expect("descriptor"),
        ],
    };
    let failing = FailingProbe;
    let context = DiscoveryContext::new().with_requested_interface(InterfaceKind::Gpio);

    let report = run_probes(&context, &[&gpio_probe, &failing]).expect("run probes");

    assert_eq!(report.snapshot.len(), 1);
    assert_eq!(report.probe_reports.len(), 1);
    assert_eq!(report.probe_reports[0].probe, "gpio");
}

#[test]
fn run_probes_keeps_report_order_stable_when_parallelized() {
    let slow_probe = DelayedProbe {
        name: "slow-gpio",
        interfaces: &[InterfaceKind::Gpio],
        delay: Duration::from_millis(20),
        devices: vec![
            DeviceFixtureBuilder::gpio_line("gpio.line0", "gpiochip0", 0)
                .expect("fixture")
                .build()
                .expect("descriptor"),
        ],
    };
    let fast_probe = DelayedProbe {
        name: "fast-i2c",
        interfaces: &[InterfaceKind::I2c],
        delay: Duration::from_millis(1),
        devices: vec![
            DeviceFixtureBuilder::i2c_device("i2c.dev0", 1, 0x40)
                .expect("fixture")
                .build()
                .expect("descriptor"),
        ],
    };

    let report =
        run_probes(&DiscoveryContext::new(), &[&slow_probe, &fast_probe]).expect("run probes");

    assert_eq!(report.snapshot.len(), 2);
    assert_eq!(report.probe_reports.len(), 2);
    assert_eq!(report.probe_reports[0].probe, "slow-gpio");
    assert_eq!(report.probe_reports[1].probe, "fast-i2c");
}

#[test]
fn run_probes_can_force_sequential_execution_via_context() {
    let inflight = Arc::new(AtomicUsize::new(0));
    let max_inflight = Arc::new(AtomicUsize::new(0));
    let gpio_probe = ConcurrentProbe {
        name: "gpio",
        interfaces: &[InterfaceKind::Gpio],
        delay: Duration::from_millis(20),
        inflight: Arc::clone(&inflight),
        max_inflight: Arc::clone(&max_inflight),
    };
    let i2c_probe = ConcurrentProbe {
        name: "i2c",
        interfaces: &[InterfaceKind::I2c],
        delay: Duration::from_millis(20),
        inflight,
        max_inflight: Arc::clone(&max_inflight),
    };

    run_probes(
        &DiscoveryContext::new()
            .with_inline_probe_threshold(0)
            .with_max_parallel_probe_workers(1),
        &[&gpio_probe, &i2c_probe],
    )
    .expect("run probes");

    assert_eq!(max_inflight.load(Ordering::SeqCst), 1);
}

#[test]
fn run_probes_can_force_parallel_execution_via_context() {
    let inflight = Arc::new(AtomicUsize::new(0));
    let max_inflight = Arc::new(AtomicUsize::new(0));
    let gpio_probe = ConcurrentProbe {
        name: "gpio",
        interfaces: &[InterfaceKind::Gpio],
        delay: Duration::from_millis(20),
        inflight: Arc::clone(&inflight),
        max_inflight: Arc::clone(&max_inflight),
    };
    let i2c_probe = ConcurrentProbe {
        name: "i2c",
        interfaces: &[InterfaceKind::I2c],
        delay: Duration::from_millis(20),
        inflight,
        max_inflight: Arc::clone(&max_inflight),
    };

    run_probes(
        &DiscoveryContext::new()
            .with_inline_probe_threshold(0)
            .with_max_parallel_probe_workers(2),
        &[&gpio_probe, &i2c_probe],
    )
    .expect("run probes");

    assert_eq!(max_inflight.load(Ordering::SeqCst), 2);
}

#[test]
fn parallel_worker_count_uses_inline_threshold_before_fanning_out() {
    let context = DiscoveryContext::new();
    assert_eq!(
        parallel_worker_count(&context, DEFAULT_INLINE_PROBE_THRESHOLD),
        1
    );
    assert!(parallel_worker_count(&context, DEFAULT_INLINE_PROBE_THRESHOLD + 1) >= 1);
}

#[test]
fn parallel_worker_count_honors_max_parallel_override() {
    let context = DiscoveryContext::new()
        .with_inline_probe_threshold(0)
        .with_max_parallel_probe_workers(3);
    let available_parallelism = std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1);
    assert_eq!(parallel_worker_count(&context, 8), 3.min(available_parallelism));
}

#[test]
#[ignore = "benchmark-style diagnostic for release prep; run with --ignored --nocapture"]
fn discovery_refresh_benchmark_reports_thread_spawn_overhead() {
    let context = DiscoveryContext::new();
    let single_probe = EmptyProbe {
        name: "single",
        interfaces: &[InterfaceKind::Gpio],
    };
    let parallel_probes = [
        EmptyProbe {
            name: "gpio",
            interfaces: &[InterfaceKind::Gpio],
        },
        EmptyProbe {
            name: "pwm",
            interfaces: &[InterfaceKind::Pwm],
        },
        EmptyProbe {
            name: "i2c",
            interfaces: &[InterfaceKind::I2c],
        },
        EmptyProbe {
            name: "spi",
            interfaces: &[InterfaceKind::Spi],
        },
        EmptyProbe {
            name: "uart",
            interfaces: &[InterfaceKind::Uart],
        },
        EmptyProbe {
            name: "usb",
            interfaces: &[InterfaceKind::Usb],
        },
        EmptyProbe {
            name: "gpio-led",
            interfaces: &[InterfaceKind::Gpio],
        },
        EmptyProbe {
            name: "pwm-hwmon",
            interfaces: &[InterfaceKind::Pwm],
        },
    ];
    let single_refs: [&dyn DiscoveryProbe; 1] = [&single_probe];
    let parallel_refs: [&dyn DiscoveryProbe; 8] = [
        &parallel_probes[0],
        &parallel_probes[1],
        &parallel_probes[2],
        &parallel_probes[3],
        &parallel_probes[4],
        &parallel_probes[5],
        &parallel_probes[6],
        &parallel_probes[7],
    ];

    let iterations = 500_u32;

    let single_started = Instant::now();
    for _ in 0..iterations {
        let report = run_probes(&context, &single_refs).expect("single-probe refresh");
        black_box(report);
    }
    let single_elapsed = single_started.elapsed();

    let parallel_started = Instant::now();
    for _ in 0..iterations {
        let report = run_probes(&context, &parallel_refs).expect("parallel refresh");
        black_box(report);
    }
    let parallel_elapsed = parallel_started.elapsed();

    let single_average_us = single_elapsed.as_secs_f64() * 1_000_000.0 / f64::from(iterations);
    let parallel_average_us = parallel_elapsed.as_secs_f64() * 1_000_000.0 / f64::from(iterations);
    let per_additional_probe_us = (parallel_average_us - single_average_us) / 7.0;
    assert!(single_average_us >= 0.0);
    assert!(parallel_average_us >= 0.0);
    black_box((
        single_average_us,
        parallel_average_us,
        per_additional_probe_us,
    ));
}

#[test]
fn configured_device_probe_emits_descriptors_for_requested_interface() {
    let probe = ConfiguredDeviceProbe::i2c(
        "configured-i2c",
        vec![StaticConfiguredDevice("configured.sensor0")],
    );

    let report = probe
        .discover(&DiscoveryContext::new().with_requested_interface(InterfaceKind::I2c))
        .expect("configured probe");

    assert_eq!(report.devices.len(), 1);
    assert_eq!(report.devices[0].id.as_str(), "configured.sensor0");
}

#[test]
fn fixture_builders_support_common_snapshot_shapes() {
    let snapshot = InventoryFixtureBuilder::new()
        .with_observed_at(TimestampMs::new(1234))
        .with_gpio_line("gpio.line0", "gpiochip0", 0)
        .expect("gpio fixture")
        .with_spi_device("spi.dev0", 1, 0)
        .expect("spi fixture")
        .with_usb_interface("usb.iface0", 1, vec![2, 3], 1, Some(0x1234), Some(0x5678))
        .expect("usb fixture")
        .build()
        .expect("snapshot");

    assert_eq!(snapshot.observed_at, Some(TimestampMs::new(1234)));
    assert_eq!(snapshot.count_for(InterfaceKind::Gpio), 1);
    assert_eq!(snapshot.count_for(InterfaceKind::Spi), 1);
    assert_eq!(snapshot.count_for(InterfaceKind::Usb), 1);
}

#[test]
fn probe_inventory_index_merges_incremental_refresh_by_interface() {
    let probe = StaticProbe {
        name: "mock",
        interfaces: &[InterfaceKind::Gpio, InterfaceKind::I2c],
        devices: vec![
            DeviceFixtureBuilder::gpio_line("gpio.line0", "gpiochip0", 0)
                .expect("gpio fixture")
                .build()
                .expect("gpio descriptor"),
            DeviceFixtureBuilder::i2c_device("i2c.dev0", 1, 0x40)
                .expect("i2c fixture")
                .build()
                .expect("i2c descriptor"),
        ],
    };
    let full = run_probes(&DiscoveryContext::new(), &[&probe]).expect("full run");
    let index = ProbeInventoryIndex::from_run(&full);

    let changed_i2c_probe = StaticProbe {
        name: "mock",
        interfaces: &[InterfaceKind::Gpio, InterfaceKind::I2c],
        devices: vec![
            DeviceFixtureBuilder::gpio_line("gpio.line0", "gpiochip0", 0)
                .expect("gpio fixture")
                .build()
                .expect("gpio descriptor"),
            DeviceFixtureBuilder::i2c_device("i2c.dev0", 1, 0x40)
                .expect("i2c fixture")
                .health(DeviceHealth::Degraded)
                .build()
                .expect("i2c descriptor"),
        ],
    };
    let incremental = run_probes(
        &DiscoveryContext::new().with_requested_interface(InterfaceKind::I2c),
        &[&changed_i2c_probe],
    )
    .expect("incremental run");
    let merged = index
        .merge_snapshot(&full.snapshot, &incremental)
        .expect("merged snapshot");

    assert_eq!(merged.len(), 2);
    assert_eq!(merged.count_for(InterfaceKind::Gpio), 1);
    assert_eq!(merged.count_for(InterfaceKind::I2c), 1);
    assert_eq!(
        merged
            .get(&DeviceId::new("gpio.line0").expect("device id"))
            .expect("gpio line")
            .health,
        DeviceHealth::Healthy
    );
    assert_eq!(
        merged
            .get(&DeviceId::new("i2c.dev0").expect("device id"))
            .expect("i2c device")
            .health,
        DeviceHealth::Degraded
    );
}

#[test]
fn probe_inventory_index_preserves_previous_devices_on_failed_incremental_refresh() {
    let gpio_probe = StaticProbe {
        name: "gpio",
        interfaces: &[InterfaceKind::Gpio],
        devices: vec![
            DeviceFixtureBuilder::gpio_line("gpio.line0", "gpiochip0", 0)
                .expect("gpio fixture")
                .build()
                .expect("gpio descriptor"),
        ],
    };
    let current = run_probes(&DiscoveryContext::new(), &[&gpio_probe]).expect("full run");
    let index = ProbeInventoryIndex::from_run(&current);

    let failed = run_probes(
        &DiscoveryContext::new().with_requested_interface(InterfaceKind::Usb),
        &[&FailingProbe],
    )
    .expect("failed run still yields report");
    let merged = index
        .merge_snapshot(&current.snapshot, &failed)
        .expect("merged snapshot");

    assert_eq!(merged, *current.snapshot);
}

#[test]
fn run_probes_with_enrichers_keeps_raw_probe_output_separate() {
    let probe = StaticProbe {
        name: "i2c",
        interfaces: &[InterfaceKind::I2c],
        devices: vec![
            DeviceFixtureBuilder::i2c_device("i2c.dev0", 1, 0x40)
                .expect("i2c fixture")
                .build()
                .expect("i2c descriptor"),
        ],
    };

    let raw = run_probes(&DiscoveryContext::new(), &[&probe]).expect("raw run");
    assert_eq!(raw.enrichment_reports.len(), 0);
    assert_eq!(raw.snapshot.devices[0].health, DeviceHealth::Healthy);
    assert!(!raw.snapshot.devices[0].properties.contains_key("enriched"));

    let enriched =
        run_probes_with_enrichers(&DiscoveryContext::new(), &[&probe], &[&HealthEnricher])
            .expect("enriched run");

    assert_eq!(enriched.probe_reports.len(), 1);
    assert_eq!(enriched.enrichment_reports.len(), 1);
    assert!(
        enriched.enrichment_reports[0].succeeded(),
        "enricher should report success"
    );
    assert_eq!(enriched.snapshot.devices[0].health, DeviceHealth::Degraded);
    assert_eq!(
        enriched.snapshot.devices[0].properties.get("enriched"),
        Some(&Value::from(true))
    );
}
