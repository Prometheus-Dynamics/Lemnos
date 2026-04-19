use super::super::support::output_config;
use crate::{Runtime, RuntimeConfig};
use lemnos_core::{
    DeviceRequest, GpioLevel, GpioRequest, InteractionRequest, LemnosEvent, StandardRequest,
};
use lemnos_discovery::DiscoveryContext;
use lemnos_drivers_gpio::GpioDriver;
use lemnos_mock::{MockGpioLine, MockHardware};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

#[test]
fn runtime_event_cursors_track_hotplug_churn_without_duplication() {
    let hardware = MockHardware::builder().build();
    let line = MockGpioLine::new("gpiochip0", 15)
        .with_line_name("cursor")
        .with_configuration(output_config());
    let device_id = line.descriptor().id.clone();

    let mut runtime = Runtime::new();
    runtime.set_gpio_backend(hardware.clone());
    runtime
        .register_driver(GpioDriver)
        .expect("register driver");

    let mut from_start = runtime.subscribe_from_start();
    hardware.attach_gpio_line(line.clone());
    runtime
        .refresh(&DiscoveryContext::new(), &[&hardware])
        .expect("refresh after first attach");

    let initial_batch = runtime.poll_events(&mut from_start);
    assert_eq!(initial_batch.len(), 1);
    assert!(matches!(initial_batch[0], LemnosEvent::Inventory(_)));
    assert!(runtime.poll_events(&mut from_start).is_empty());

    let mut midstream = runtime.subscribe();

    runtime
        .request(DeviceRequest::new(
            device_id.clone(),
            InteractionRequest::Standard(StandardRequest::Gpio(GpioRequest::Write {
                level: GpioLevel::High,
            })),
        ))
        .expect("write before removal");
    assert!(hardware.remove_device(&device_id));
    runtime
        .refresh(&DiscoveryContext::new(), &[&hardware])
        .expect("refresh after removal");
    hardware.attach_gpio_line(line);
    runtime
        .refresh(&DiscoveryContext::new(), &[&hardware])
        .expect("refresh after reattach");
    runtime
        .request(DeviceRequest::new(
            device_id.clone(),
            InteractionRequest::Standard(StandardRequest::Gpio(GpioRequest::Read)),
        ))
        .expect("read after reattach");

    let start_batch = runtime.poll_events(&mut from_start);
    assert_eq!(start_batch.len(), 6);
    assert_eq!(
        start_batch
            .iter()
            .filter(|event| matches!(event, LemnosEvent::Inventory(_)))
            .count(),
        2
    );
    assert_eq!(
        start_batch
            .iter()
            .filter(|event| matches!(event, LemnosEvent::State(_)))
            .count(),
        4
    );
    assert!(runtime.poll_events(&mut from_start).is_empty());

    let midstream_batch = runtime.poll_events(&mut midstream);
    assert_eq!(midstream_batch.len(), 6);
    assert_eq!(
        midstream_batch
            .iter()
            .filter(|event| matches!(event, LemnosEvent::Inventory(_)))
            .count(),
        2
    );
    assert_eq!(
        midstream_batch
            .iter()
            .filter(|event| matches!(event, LemnosEvent::State(_)))
            .count(),
        4
    );
    assert!(runtime.poll_events(&mut midstream).is_empty());

    let mut late = runtime.subscribe();
    assert!(runtime.poll_events(&mut late).is_empty());

    runtime
        .request(DeviceRequest::new(
            device_id,
            InteractionRequest::Standard(StandardRequest::Gpio(GpioRequest::Write {
                level: GpioLevel::Low,
            })),
        ))
        .expect("final write");

    let late_batch = runtime.poll_events(&mut late);
    assert_eq!(late_batch.len(), 1);
    assert!(
        late_batch
            .iter()
            .all(|event| matches!(event, LemnosEvent::State(_)))
    );
    assert!(runtime.poll_events(&mut late).is_empty());
}

#[test]
fn runtime_blocking_subscription_starts_from_the_retained_base() {
    let hardware = MockHardware::builder()
        .with_gpio_line(
            MockGpioLine::new("gpiochip0", 16)
                .with_line_name("blocking-alias")
                .with_configuration(output_config()),
        )
        .build();

    let mut runtime = Runtime::new();
    runtime.set_gpio_backend(hardware.clone());
    runtime
        .register_driver(GpioDriver)
        .expect("register driver");
    runtime
        .refresh(&DiscoveryContext::new(), &[&hardware])
        .expect("refresh");

    let mut subscription = runtime.subscribe_from_start_blocking();
    assert_eq!(subscription.cursor().next_index(), 0);
    assert!(subscription.wait_for_update(Some(Duration::from_millis(0))));

    let events = subscription
        .wait_and_poll_next_with_status(&runtime, Some(Duration::from_millis(0)))
        .expect("pending event batch");
    assert_eq!(events.events().len(), 1);
    assert!(!events.was_truncated());
}

#[test]
fn runtime_event_cursor_polls_new_events_without_duplication() {
    let hardware = MockHardware::builder()
        .with_gpio_line(
            MockGpioLine::new("gpiochip0", 3)
                .with_line_name("heartbeat")
                .with_configuration(output_config()),
        )
        .build();
    let device_id = hardware.descriptors()[0].id.clone();

    let mut runtime = Runtime::new();
    runtime.set_gpio_backend(hardware.clone());
    runtime
        .register_driver(GpioDriver)
        .expect("register driver");

    let mut cursor = runtime.subscribe_from_start();
    runtime
        .refresh(&DiscoveryContext::new(), &[&hardware])
        .expect("refresh");
    let first_batch = runtime.poll_events(&mut cursor);
    assert_eq!(first_batch.len(), 1);
    assert!(matches!(first_batch[0], LemnosEvent::Inventory(_)));
    assert!(runtime.poll_events(&mut cursor).is_empty());

    runtime
        .request(DeviceRequest::new(
            device_id,
            InteractionRequest::Standard(StandardRequest::Gpio(GpioRequest::Write {
                level: GpioLevel::High,
            })),
        ))
        .expect("write request");
    let second_batch = runtime.poll_events(&mut cursor);
    assert_eq!(second_batch.len(), 2);
    assert!(
        second_batch
            .iter()
            .all(|event| matches!(event, LemnosEvent::State(_)))
    );
    assert!(runtime.poll_events(&mut cursor).is_empty());
}

#[test]
fn runtime_blocking_subscription_waits_for_new_events() {
    let hardware = MockHardware::builder()
        .with_gpio_line(
            MockGpioLine::new("gpiochip0", 21)
                .with_line_name("live-subscription")
                .with_configuration(output_config()),
        )
        .build();
    let device_id = hardware.descriptors()[0].id.clone();

    let runtime = Arc::new(Mutex::new(Runtime::new()));
    {
        let mut runtime = runtime
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        runtime.set_gpio_backend(hardware.clone());
        runtime
            .register_driver(GpioDriver)
            .expect("register driver");
        runtime
            .refresh(&DiscoveryContext::new(), &[&hardware])
            .expect("refresh");
    }

    let mut subscription = {
        let runtime = runtime
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        runtime.subscribe_blocking()
    };

    let runtime_for_worker = Arc::clone(&runtime);
    let device_id_for_worker = device_id.clone();
    let worker = thread::spawn(move || {
        thread::sleep(Duration::from_millis(20));
        let mut runtime = runtime_for_worker
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        runtime
            .request(DeviceRequest::new(
                device_id_for_worker,
                InteractionRequest::Standard(StandardRequest::Gpio(GpioRequest::Write {
                    level: GpioLevel::High,
                })),
            ))
            .expect("write should publish state events");
    });

    assert!(subscription.wait_for_update(Some(Duration::from_secs(1))));

    {
        let runtime = runtime
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert!(subscription.has_pending(&runtime));
        let poll = subscription.poll_with_status(&runtime);
        let events = poll.events();
        assert!(!poll.was_truncated());
        assert_eq!(events.len(), 2);
        assert!(
            events
                .iter()
                .all(|event| matches!(event, LemnosEvent::State(_)))
        );
    }

    worker.join().expect("worker thread should finish");

    let runtime = runtime
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    assert!(!subscription.has_pending(&runtime));
    assert_eq!(
        runtime
            .state(&device_id)
            .expect("cached state")
            .telemetry
            .get("level"),
        Some(&"high".into())
    );
}

#[test]
fn runtime_blocking_subscription_wakes_when_shutdown_begins() {
    let runtime = Arc::new(Mutex::new(Runtime::new()));
    let subscription = {
        let runtime = runtime
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        runtime.subscribe_blocking()
    };

    let worker = thread::spawn(move || subscription.wait_for_update(Some(Duration::from_secs(1))));

    thread::sleep(Duration::from_millis(20));
    runtime
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .shutdown();

    assert!(worker.join().expect("waiter should wake on shutdown"));
}

#[test]
fn runtime_blocking_subscription_wakes_when_retention_compaction_makes_cursor_stale() {
    let hardware = MockHardware::builder()
        .with_gpio_line(
            MockGpioLine::new("gpiochip0", 28)
                .with_line_name("stale-wakeup")
                .with_configuration(output_config()),
        )
        .build();

    let runtime = Arc::new(Mutex::new(Runtime::new()));
    let subscription = {
        let mut runtime = runtime
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        runtime.set_gpio_backend(hardware.clone());
        runtime
            .register_driver(GpioDriver)
            .expect("register driver");
        runtime
            .refresh(&DiscoveryContext::new(), &[&hardware])
            .expect("refresh");
        runtime.subscribe_from_start_blocking()
    };

    let initial_runtime = runtime
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    assert!(subscription.has_pending(&initial_runtime));
    drop(initial_runtime);

    let worker = thread::spawn(move || subscription.wait_for_update(Some(Duration::from_secs(1))));

    thread::sleep(Duration::from_millis(20));
    runtime
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .set_config(RuntimeConfig::new().with_max_retained_event_bytes(Some(1)));

    assert!(
        worker
            .join()
            .expect("waiter should wake when compaction changes subscriber state")
    );
}

#[test]
fn runtime_live_subscription_handles_truncated_event_history() {
    let hardware = MockHardware::builder()
        .with_gpio_line(
            MockGpioLine::new("gpiochip0", 22)
                .with_line_name("truncated-history")
                .with_configuration(output_config()),
        )
        .build();
    let device_id = hardware.descriptors()[0].id.clone();

    let mut runtime = Runtime::new();
    runtime.set_gpio_backend(hardware.clone());
    runtime
        .register_driver(GpioDriver)
        .expect("register driver");
    runtime
        .refresh(&DiscoveryContext::new(), &[&hardware])
        .expect("refresh");

    let mut subscription = runtime.subscribe_from_start_blocking();
    assert!(subscription.has_pending(&runtime));
    assert_eq!(subscription.pending_count(&runtime), 1);

    let taken = runtime.take_events();
    assert_eq!(taken.len(), 1);
    assert!(subscription.is_stale(&runtime));
    assert!(!subscription.has_pending(&runtime));

    runtime
        .request(DeviceRequest::new(
            device_id,
            InteractionRequest::Standard(StandardRequest::Gpio(GpioRequest::Write {
                level: GpioLevel::High,
            })),
        ))
        .expect("write request");

    assert!(subscription.is_stale(&runtime));
    assert_eq!(subscription.pending_count(&runtime), 2);

    let events = subscription.poll_with_status(&runtime);
    assert!(events.was_truncated());
    assert_eq!(events.events().len(), 2);
    assert!(
        events
            .events()
            .iter()
            .all(|event| matches!(event, LemnosEvent::State(_)))
    );
    assert!(!subscription.is_stale(&runtime));
    assert!(!subscription.has_pending(&runtime));
}
