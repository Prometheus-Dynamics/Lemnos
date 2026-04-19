use super::super::support::{TestRoot, create_linux_watch_roots};
use crate::{Runtime, RuntimeConfig, RuntimeWatchRefreshMode};
use lemnos_discovery::{
    DiscoveryContext, DiscoveryError, DiscoveryProbe, InventoryWatchEvent, InventoryWatcher,
    ProbeDiscovery,
};
use lemnos_drivers_gpio::GpioDriver;
use lemnos_linux::LinuxBackend;
use std::fs;
use std::sync::{Arc, Mutex};

#[test]
fn runtime_poll_watcher_and_refresh_consumes_linux_hotplug_signals() {
    let root = TestRoot::new();
    create_linux_watch_roots(&root);
    root.create_dir("sys/class/gpio/gpiochip0");
    root.write("sys/class/gpio/gpiochip0/label", "watch-runtime\n");
    root.write("sys/class/gpio/gpiochip0/base", "0\n");
    root.write("sys/class/gpio/gpiochip0/ngpio", "1\n");
    root.touch("dev/gpiochip0");

    let backend = LinuxBackend::with_paths(root.paths());
    let mut watcher = backend.hotplug_watcher().expect("create hotplug watcher");
    let gpio_probe = backend.gpio_probe();
    let context = DiscoveryContext::new();

    let mut runtime = Runtime::new();
    runtime.set_gpio_backend(backend.clone());
    runtime
        .register_driver(GpioDriver)
        .expect("register gpio driver");
    runtime
        .refresh(&context, &[&gpio_probe])
        .expect("initial refresh");
    assert_eq!(runtime.inventory().len(), 2);
    assert!(watcher.poll().expect("initial watcher poll").is_empty());

    root.create_dir("sys/class/gpio/gpiochip1");
    root.write("sys/class/gpio/gpiochip1/label", "watch-runtime-2\n");
    root.write("sys/class/gpio/gpiochip1/base", "8\n");
    root.write("sys/class/gpio/gpiochip1/ngpio", "2\n");
    root.touch("dev/gpiochip1");

    let report = runtime
        .poll_watcher_and_refresh(&context, &[&gpio_probe], &mut watcher)
        .expect("watch and refresh")
        .expect("change should trigger refresh");

    assert!(
        report
            .watch_events
            .iter()
            .any(|event| event.touches(lemnos_core::InterfaceKind::Gpio))
    );
    assert_eq!(report.refresh.diff.added.len(), 3);
    assert_eq!(runtime.inventory().len(), 5);
    assert!(
        runtime
            .poll_watcher_and_refresh(&context, &[&gpio_probe], &mut watcher)
            .expect("second watch poll")
            .is_none()
    );
}

#[test]
fn runtime_poll_watcher_and_refresh_reports_gpio_changes() {
    let root = TestRoot::new();
    create_linux_watch_roots(&root);
    root.create_dir("sys/class/gpio/gpiochip0");
    root.write("sys/class/gpio/gpiochip0/label", "watch-alias\n");
    root.write("sys/class/gpio/gpiochip0/base", "0\n");
    root.write("sys/class/gpio/gpiochip0/ngpio", "1\n");
    root.touch("dev/gpiochip0");

    let backend = LinuxBackend::with_paths(root.paths());
    let mut watcher = backend.hotplug_watcher().expect("create hotplug watcher");
    let gpio_probe = backend.gpio_probe();
    let context = DiscoveryContext::new();

    let mut runtime = Runtime::new();
    runtime.set_gpio_backend(backend.clone());
    runtime
        .register_driver(GpioDriver)
        .expect("register gpio driver");
    runtime
        .refresh(&context, &[&gpio_probe])
        .expect("initial refresh");
    assert!(watcher.poll().expect("initial watcher poll").is_empty());

    root.create_dir("sys/class/gpio/gpiochip1");
    root.write("sys/class/gpio/gpiochip1/label", "watch-alias-2\n");
    root.write("sys/class/gpio/gpiochip1/base", "8\n");
    root.write("sys/class/gpio/gpiochip1/ngpio", "1\n");
    root.touch("dev/gpiochip1");

    let report = runtime
        .poll_watcher_and_refresh(&context, &[&gpio_probe], &mut watcher)
        .expect("watch and refresh")
        .expect("change should trigger refresh");
    assert_eq!(report.refresh.diff.added.len(), 2);
}

#[test]
fn runtime_poll_watcher_and_refresh_rebinds_linux_devices_after_hotplug_cycle() {
    let root = TestRoot::new();
    create_linux_watch_roots(&root);
    root.create_dir("sys/class/gpio/gpiochip0");
    root.write("sys/class/gpio/gpiochip0/label", "watch-rebind\n");
    root.write("sys/class/gpio/gpiochip0/base", "32\n");
    root.write("sys/class/gpio/gpiochip0/ngpio", "1\n");
    root.touch("dev/gpiochip0");
    root.create_dir("sys/class/gpio/gpio32");
    root.write("sys/class/gpio/gpio32/direction", "out\n");
    root.write("sys/class/gpio/gpio32/value", "0\n");
    root.write("sys/class/gpio/gpio32/active_low", "0\n");
    root.write("sys/class/gpio/gpio32/edge", "none\n");

    let backend = LinuxBackend::with_paths(root.paths());
    let mut watcher = backend.hotplug_watcher().expect("create hotplug watcher");
    let gpio_probe = backend.gpio_probe();
    let context = DiscoveryContext::new();
    let device_id = gpio_probe
        .discover(&context)
        .expect("discover gpio devices")
        .devices
        .into_iter()
        .find(|device| device.kind == lemnos_core::DeviceKind::GpioLine)
        .expect("gpio line")
        .id;

    let mut runtime = Runtime::new();
    runtime.set_gpio_backend(backend.clone());
    runtime
        .register_driver(GpioDriver)
        .expect("register gpio driver");
    runtime
        .refresh(&context, &[&gpio_probe])
        .expect("initial refresh");
    runtime.bind(&device_id).expect("bind");
    assert!(
        watcher
            .poll()
            .expect("clear initial watch noise")
            .is_empty()
    );

    fs::remove_dir_all(root.root.join("sys/class/gpio/gpio32")).expect("remove line root");
    fs::remove_dir_all(root.root.join("sys/class/gpio/gpiochip0")).expect("remove chip root");
    let removal = runtime
        .poll_watcher_and_refresh(&context, &[&gpio_probe], &mut watcher)
        .expect("watch removal")
        .expect("removal should trigger refresh");
    assert!(
        removal
            .refresh
            .diff
            .removed
            .iter()
            .any(|id| id == &device_id)
    );
    assert!(!runtime.is_bound(&device_id));
    assert!(runtime.wants_binding(&device_id));

    root.create_dir("sys/class/gpio/gpiochip0");
    root.write("sys/class/gpio/gpiochip0/label", "watch-rebind\n");
    root.write("sys/class/gpio/gpiochip0/base", "32\n");
    root.write("sys/class/gpio/gpiochip0/ngpio", "1\n");
    root.touch("dev/gpiochip0");
    root.create_dir("sys/class/gpio/gpio32");
    root.write("sys/class/gpio/gpio32/direction", "out\n");
    root.write("sys/class/gpio/gpio32/value", "0\n");
    root.write("sys/class/gpio/gpio32/active_low", "0\n");
    root.write("sys/class/gpio/gpio32/edge", "none\n");

    let reattach = runtime
        .poll_watcher_and_refresh(&context, &[&gpio_probe], &mut watcher)
        .expect("watch reattach")
        .expect("reattach should trigger refresh");
    assert_eq!(reattach.refresh.rebinds.attempted, vec![device_id.clone()]);
    assert_eq!(reattach.refresh.rebinds.rebound, vec![device_id.clone()]);
    assert!(runtime.is_bound(&device_id));
}

#[test]
fn runtime_poll_watcher_and_refresh_scopes_probe_execution_to_touched_interfaces() {
    const GPIO_ONLY: [lemnos_core::InterfaceKind; 1] = [lemnos_core::InterfaceKind::Gpio];
    const I2C_ONLY: [lemnos_core::InterfaceKind; 1] = [lemnos_core::InterfaceKind::I2c];

    struct CountingProbe {
        name: &'static str,
        interfaces: &'static [lemnos_core::InterfaceKind],
        calls: Arc<Mutex<Vec<&'static str>>>,
    }

    impl DiscoveryProbe for CountingProbe {
        fn name(&self) -> &'static str {
            self.name
        }

        fn interfaces(&self) -> &'static [lemnos_core::InterfaceKind] {
            self.interfaces
        }

        fn discover(&self, _context: &DiscoveryContext) -> Result<ProbeDiscovery, DiscoveryError> {
            self.calls
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(self.name);
            Ok(ProbeDiscovery::default())
        }
    }

    struct StaticWatcher {
        events: Vec<InventoryWatchEvent>,
    }

    impl InventoryWatcher for StaticWatcher {
        fn name(&self) -> &'static str {
            "static-watcher"
        }

        fn poll(&mut self) -> Result<Vec<InventoryWatchEvent>, DiscoveryError> {
            Ok(std::mem::take(&mut self.events))
        }
    }

    let calls = Arc::new(Mutex::new(Vec::new()));
    let gpio_probe = CountingProbe {
        name: "gpio-probe",
        interfaces: &GPIO_ONLY,
        calls: Arc::clone(&calls),
    };
    let i2c_probe = CountingProbe {
        name: "i2c-probe",
        interfaces: &I2C_ONLY,
        calls: Arc::clone(&calls),
    };

    let mut watcher = StaticWatcher {
        events: vec![InventoryWatchEvent::new(
            "static-watcher",
            vec![lemnos_core::InterfaceKind::Gpio],
            vec!["/sys/class/gpio/gpio42".into()],
        )],
    };

    let mut runtime = Runtime::new();
    let report = runtime
        .poll_watcher_and_refresh(
            &DiscoveryContext::new(),
            &[&gpio_probe, &i2c_probe],
            &mut watcher,
        )
        .expect("poll_watcher_and_refresh")
        .expect("gpio watch should trigger a scoped refresh");

    assert_eq!(report.watch_events.len(), 1);
    assert_eq!(
        calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_slice(),
        &["gpio-probe"]
    );

    let mut watcher = StaticWatcher {
        events: vec![InventoryWatchEvent::new(
            "static-watcher",
            vec![lemnos_core::InterfaceKind::I2c],
            vec!["/sys/bus/i2c/devices/1-0040".into()],
        )],
    };

    let result = runtime
        .poll_watcher_and_refresh(
            &DiscoveryContext::new().with_requested_interface(lemnos_core::InterfaceKind::Gpio),
            &[&gpio_probe, &i2c_probe],
            &mut watcher,
        )
        .expect("poll_watcher_and_refresh should ignore unrelated watch events");

    assert!(result.is_none());
    assert_eq!(
        calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_slice(),
        &["gpio-probe"]
    );
}

#[test]
fn runtime_watch_refresh_mode_strict_scoped_skips_when_watcher_has_no_interface_hints() {
    const GPIO_ONLY: [lemnos_core::InterfaceKind; 1] = [lemnos_core::InterfaceKind::Gpio];

    struct CountingProbe {
        name: &'static str,
        calls: Arc<Mutex<Vec<&'static str>>>,
    }

    impl DiscoveryProbe for CountingProbe {
        fn name(&self) -> &'static str {
            self.name
        }

        fn interfaces(&self) -> &'static [lemnos_core::InterfaceKind] {
            &GPIO_ONLY
        }

        fn discover(&self, _context: &DiscoveryContext) -> Result<ProbeDiscovery, DiscoveryError> {
            self.calls
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(self.name);
            Ok(ProbeDiscovery::default())
        }
    }

    struct StaticWatcher {
        events: Vec<InventoryWatchEvent>,
    }

    impl InventoryWatcher for StaticWatcher {
        fn name(&self) -> &'static str {
            "static-watcher"
        }

        fn poll(&mut self) -> Result<Vec<InventoryWatchEvent>, DiscoveryError> {
            Ok(std::mem::take(&mut self.events))
        }
    }

    let calls = Arc::new(Mutex::new(Vec::new()));
    let gpio_probe = CountingProbe {
        name: "gpio-probe",
        calls: Arc::clone(&calls),
    };
    let mut watcher = StaticWatcher {
        events: vec![InventoryWatchEvent::new(
            "static-watcher",
            Vec::new(),
            vec!["/sys/class/gpio/gpio42".into()],
        )],
    };

    let mut runtime = Runtime::with_config(
        RuntimeConfig::new().with_watch_refresh_mode(RuntimeWatchRefreshMode::StrictScoped),
    );
    let report = runtime
        .poll_watcher_and_refresh(&DiscoveryContext::new(), &[&gpio_probe], &mut watcher)
        .expect("poll_watcher_and_refresh should succeed");

    assert!(report.is_none());
    assert!(
        calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_empty()
    );
}

#[test]
fn runtime_watch_refresh_mode_can_fallback_to_full_refresh() {
    const GPIO_ONLY: [lemnos_core::InterfaceKind; 1] = [lemnos_core::InterfaceKind::Gpio];
    const I2C_ONLY: [lemnos_core::InterfaceKind; 1] = [lemnos_core::InterfaceKind::I2c];

    struct CountingProbe {
        name: &'static str,
        interfaces: &'static [lemnos_core::InterfaceKind],
        calls: Arc<Mutex<Vec<&'static str>>>,
    }

    impl DiscoveryProbe for CountingProbe {
        fn name(&self) -> &'static str {
            self.name
        }

        fn interfaces(&self) -> &'static [lemnos_core::InterfaceKind] {
            self.interfaces
        }

        fn discover(&self, _context: &DiscoveryContext) -> Result<ProbeDiscovery, DiscoveryError> {
            self.calls
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(self.name);
            Ok(ProbeDiscovery::default())
        }
    }

    struct StaticWatcher {
        events: Vec<InventoryWatchEvent>,
    }

    impl InventoryWatcher for StaticWatcher {
        fn name(&self) -> &'static str {
            "static-watcher"
        }

        fn poll(&mut self) -> Result<Vec<InventoryWatchEvent>, DiscoveryError> {
            Ok(std::mem::take(&mut self.events))
        }
    }

    let calls = Arc::new(Mutex::new(Vec::new()));
    let gpio_probe = CountingProbe {
        name: "gpio-probe",
        interfaces: &GPIO_ONLY,
        calls: Arc::clone(&calls),
    };
    let i2c_probe = CountingProbe {
        name: "i2c-probe",
        interfaces: &I2C_ONLY,
        calls: Arc::clone(&calls),
    };

    let mut watcher = StaticWatcher {
        events: vec![InventoryWatchEvent::new(
            "static-watcher",
            vec![lemnos_core::InterfaceKind::I2c],
            vec!["/sys/bus/i2c/devices/1-0040".into()],
        )],
    };

    let mut runtime = Runtime::with_config(
        RuntimeConfig::new().with_watch_refresh_mode(RuntimeWatchRefreshMode::FallbackToFull),
    );
    let report = runtime
        .poll_watcher_and_refresh(
            &DiscoveryContext::new().with_requested_interface(lemnos_core::InterfaceKind::Gpio),
            &[&gpio_probe, &i2c_probe],
            &mut watcher,
        )
        .expect("poll_watcher_and_refresh")
        .expect("fallback mode should run the original refresh");

    assert_eq!(report.watch_events.len(), 1);
    assert_eq!(
        calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_slice(),
        &["gpio-probe"]
    );
}

#[test]
fn runtime_watch_refresh_mode_can_force_full_refresh() {
    const GPIO_ONLY: [lemnos_core::InterfaceKind; 1] = [lemnos_core::InterfaceKind::Gpio];
    const I2C_ONLY: [lemnos_core::InterfaceKind; 1] = [lemnos_core::InterfaceKind::I2c];

    struct CountingProbe {
        name: &'static str,
        interfaces: &'static [lemnos_core::InterfaceKind],
        calls: Arc<Mutex<Vec<&'static str>>>,
    }

    impl DiscoveryProbe for CountingProbe {
        fn name(&self) -> &'static str {
            self.name
        }

        fn interfaces(&self) -> &'static [lemnos_core::InterfaceKind] {
            self.interfaces
        }

        fn discover(&self, _context: &DiscoveryContext) -> Result<ProbeDiscovery, DiscoveryError> {
            self.calls
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(self.name);
            Ok(ProbeDiscovery::default())
        }
    }

    struct StaticWatcher {
        events: Vec<InventoryWatchEvent>,
    }

    impl InventoryWatcher for StaticWatcher {
        fn name(&self) -> &'static str {
            "static-watcher"
        }

        fn poll(&mut self) -> Result<Vec<InventoryWatchEvent>, DiscoveryError> {
            Ok(std::mem::take(&mut self.events))
        }
    }

    let calls = Arc::new(Mutex::new(Vec::new()));
    let gpio_probe = CountingProbe {
        name: "gpio-probe",
        interfaces: &GPIO_ONLY,
        calls: Arc::clone(&calls),
    };
    let i2c_probe = CountingProbe {
        name: "i2c-probe",
        interfaces: &I2C_ONLY,
        calls: Arc::clone(&calls),
    };

    let mut watcher = StaticWatcher {
        events: vec![InventoryWatchEvent::new(
            "static-watcher",
            vec![lemnos_core::InterfaceKind::Gpio],
            vec!["/sys/class/gpio/gpio42".into()],
        )],
    };

    let mut runtime = Runtime::with_config(
        RuntimeConfig::new().with_watch_refresh_mode(RuntimeWatchRefreshMode::Full),
    );
    let report = runtime
        .poll_watcher_and_refresh(
            &DiscoveryContext::new(),
            &[&gpio_probe, &i2c_probe],
            &mut watcher,
        )
        .expect("poll_watcher_and_refresh")
        .expect("full mode should rerun all configured probes");

    assert_eq!(report.watch_events.len(), 1);
    let mut calls = calls
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    calls.sort();
    assert_eq!(calls, vec!["gpio-probe", "i2c-probe"]);
}
