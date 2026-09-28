use super::support::{TestRoot, create_watch_roots};
use crate::LinuxBackend;
use lemnos_core::InterfaceKind;
use lemnos_discovery::InventoryWatcher;

#[test]
fn linux_hotplug_watcher_reports_gpio_root_changes() {
    let root = TestRoot::new();
    create_watch_roots(&root);

    let backend = LinuxBackend::with_paths(root.paths());
    let mut watcher = backend.hotplug_watcher().expect("create hotplug watcher");
    assert!(watcher.poll().expect("initial poll").is_empty());

    root.create_dir("sys/class/gpio/gpiochip7");
    root.write("sys/class/gpio/gpiochip7/label", "watch-test\n");
    root.write("sys/class/gpio/gpiochip7/base", "224\n");
    root.write("sys/class/gpio/gpiochip7/ngpio", "4\n");

    let events = watcher.poll().expect("poll after gpio change");
    assert_eq!(events.len(), 1);
    assert!(events[0].touches(InterfaceKind::Gpio));
    assert!(
        events[0]
            .paths
            .iter()
            .any(|path| path.to_string_lossy().contains("gpiochip7"))
    );
}

#[test]
fn linux_hotplug_watcher_reports_pwm_chip_child_changes() {
    let root = TestRoot::new();
    create_watch_roots(&root);
    root.create_dir("sys/class/pwm/pwmchip2");
    root.write("sys/class/pwm/pwmchip2/npwm", "2\n");

    let backend = LinuxBackend::with_paths(root.paths());
    let mut watcher = backend.hotplug_watcher().expect("create hotplug watcher");
    assert!(watcher.poll().expect("initial poll").is_empty());

    root.create_dir("sys/class/pwm/pwmchip2/pwm1");

    let events = watcher.poll().expect("poll after pwm export");
    assert_eq!(events.len(), 1);
    assert!(events[0].touches(InterfaceKind::Pwm));
    assert!(
        events[0]
            .paths
            .iter()
            .any(|path| path.to_string_lossy().contains("pwm1"))
    );
}

#[cfg(feature = "tokio")]
#[tokio::test(flavor = "current_thread")]
async fn async_linux_hotplug_watcher_wakes_on_gpio_root_changes() {
    use std::time::Duration;

    let root = TestRoot::new();
    create_watch_roots(&root);

    let backend = LinuxBackend::with_paths(root.paths());
    let mut watcher = backend
        .async_hotplug_watcher()
        .expect("create async hotplug watcher");

    let idle = tokio::time::timeout(Duration::from_millis(50), watcher.next_events()).await;
    assert!(idle.is_err(), "watcher must park while nothing changes");

    root.create_dir("sys/class/gpio/gpiochip9");

    let events = tokio::time::timeout(Duration::from_secs(5), watcher.next_events())
        .await
        .expect("watcher wakes on change")
        .expect("read watch events");
    assert_eq!(events.len(), 1);
    assert!(events[0].touches(InterfaceKind::Gpio));
    assert!(
        events[0]
            .paths
            .iter()
            .any(|path| path.to_string_lossy().contains("gpiochip9"))
    );

    let drained = tokio::time::timeout(Duration::from_millis(50), watcher.next_events()).await;
    assert!(drained.is_err(), "watcher must park again once drained");
}
