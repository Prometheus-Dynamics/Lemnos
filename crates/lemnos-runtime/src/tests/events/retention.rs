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
fn runtime_truncates_retained_events_when_configured_limit_is_reached() {
    let hardware = MockHardware::builder()
        .with_gpio_line(
            MockGpioLine::new("gpiochip0", 23)
                .with_line_name("bounded-history")
                .with_configuration(output_config()),
        )
        .build();
    let device_id = hardware.descriptors()[0].id.clone();

    let mut runtime = Runtime::with_config(RuntimeConfig::new().with_max_retained_events(2));
    runtime.set_gpio_backend(hardware.clone());
    runtime
        .register_driver(GpioDriver)
        .expect("register driver");

    let mut from_start = runtime.subscribe_from_start();

    runtime
        .refresh(&DiscoveryContext::new(), &[&hardware])
        .expect("refresh");
    runtime
        .request(DeviceRequest::new(
            device_id.clone(),
            InteractionRequest::Standard(StandardRequest::Gpio(GpioRequest::Write {
                level: GpioLevel::High,
            })),
        ))
        .expect("first write");
    runtime
        .request(DeviceRequest::new(
            device_id,
            InteractionRequest::Standard(StandardRequest::Gpio(GpioRequest::Write {
                level: GpioLevel::Low,
            })),
        ))
        .expect("second write");

    assert_eq!(runtime.events().len(), 2);
    assert!(runtime.is_cursor_stale(&from_start));

    let retained = runtime.poll_events_with_status(&mut from_start);
    assert!(retained.was_truncated());
    assert_eq!(retained.events().len(), 2);
    assert!(
        retained
            .events()
            .iter()
            .all(|event| matches!(event, LemnosEvent::State(_)))
    );
    assert!(!runtime.is_cursor_stale(&from_start));
}

#[test]
fn runtime_enforces_retained_event_byte_budget() {
    let hardware = MockHardware::builder()
        .with_gpio_line(
            MockGpioLine::new("gpiochip0", 24)
                .with_line_name("byte-budget")
                .with_configuration(output_config()),
        )
        .build();
    let device_id = hardware.descriptors()[0].id.clone();

    let mut runtime = Runtime::with_config(
        RuntimeConfig::new()
            .with_max_retained_events(32)
            .with_max_retained_event_bytes(Some(1)),
    );
    runtime.set_gpio_backend(hardware.clone());
    runtime
        .register_driver(GpioDriver)
        .expect("register driver");

    let mut from_start = runtime.subscribe_from_start();

    runtime
        .refresh(&DiscoveryContext::new(), &[&hardware])
        .expect("refresh");
    runtime
        .request(DeviceRequest::new(
            device_id,
            InteractionRequest::Standard(StandardRequest::Gpio(GpioRequest::Write {
                level: GpioLevel::High,
            })),
        ))
        .expect("write request");

    let retention = runtime.event_retention_stats();
    assert_eq!(retention.max_retained_event_bytes, Some(1));
    assert_eq!(retention.retained_events, 0);
    assert_eq!(retention.retained_event_bytes, 0);
    assert!(runtime.is_cursor_stale(&from_start));

    let retained = runtime.poll_events_with_status(&mut from_start);
    assert!(retained.was_truncated());
    assert!(retained.events().is_empty());
}

#[test]
fn runtime_event_retention_stats_report_current_limits_and_usage() {
    let hardware = MockHardware::builder()
        .with_gpio_line(
            MockGpioLine::new("gpiochip0", 25)
                .with_line_name("retention-stats")
                .with_configuration(output_config()),
        )
        .build();

    let mut runtime = Runtime::with_config(
        RuntimeConfig::new()
            .with_max_retained_events(4)
            .with_max_retained_event_bytes(Some(4_096)),
    );
    runtime.set_gpio_backend(hardware.clone());
    runtime
        .register_driver(GpioDriver)
        .expect("register driver");
    runtime
        .refresh(&DiscoveryContext::new(), &[&hardware])
        .expect("refresh");

    let retention = runtime.event_retention_stats();
    assert_eq!(retention.event_base_index, 0);
    assert_eq!(retention.event_tail_index, 1);
    assert_eq!(retention.retained_events, 1);
    assert!(retention.retained_event_bytes > 0);
    assert_eq!(retention.max_retained_events, 4);
    assert_eq!(retention.max_retained_event_bytes, Some(4_096));
}

#[test]
fn runtime_lagging_live_subscription_recovers_after_repeated_retention_compaction() {
    let hardware = MockHardware::builder()
        .with_gpio_line(
            MockGpioLine::new("gpiochip0", 26)
                .with_line_name("lagging-live")
                .with_configuration(output_config()),
        )
        .build();
    let device_id = hardware.descriptors()[0].id.clone();

    let mut runtime = Runtime::with_config(RuntimeConfig::new().with_max_retained_events(3));
    runtime.set_gpio_backend(hardware.clone());
    runtime
        .register_driver(GpioDriver)
        .expect("register driver");
    runtime
        .refresh(&DiscoveryContext::new(), &[&hardware])
        .expect("refresh");
    runtime.bind(&device_id).expect("bind");

    let mut subscription = runtime.subscribe_from_start_blocking();

    for step in 0..8 {
        let level = if step % 2 == 0 {
            GpioLevel::High
        } else {
            GpioLevel::Low
        };
        runtime
            .request(DeviceRequest::new(
                device_id.clone(),
                InteractionRequest::Standard(StandardRequest::Gpio(GpioRequest::Write { level })),
            ))
            .expect("write during retention churn");
    }

    assert_eq!(runtime.events().len(), 3);
    assert!(subscription.is_stale(&runtime));
    assert_eq!(subscription.pending_count(&runtime), 3);

    let retained = subscription.poll_with_status(&runtime);
    assert!(retained.was_truncated());
    assert_eq!(retained.events().len(), 3);
    assert!(
        retained
            .events()
            .iter()
            .all(|event| matches!(event, LemnosEvent::State(_)))
    );
    assert!(!subscription.is_stale(&runtime));
    assert!(!subscription.has_pending(&runtime));

    runtime
        .request(DeviceRequest::new(
            device_id.clone(),
            InteractionRequest::Standard(StandardRequest::Gpio(GpioRequest::Write {
                level: GpioLevel::High,
            })),
        ))
        .expect("write after recovery");

    assert!(subscription.wait_for_update(Some(Duration::from_secs(1))));
    let events = subscription.poll_with_status(&runtime);
    assert!(!events.was_truncated());
    assert_eq!(events.events().len(), 1);
    assert!(
        events
            .events()
            .iter()
            .all(|event| matches!(event, LemnosEvent::State(_)))
    );
    assert!(!subscription.is_stale(&runtime));
    assert!(!subscription.has_pending(&runtime));
}

#[test]
fn runtime_live_subscription_tracks_sustained_state_churn_without_duplicates() {
    let hardware = MockHardware::builder()
        .with_gpio_line(
            MockGpioLine::new("gpiochip0", 27)
                .with_line_name("live-churn")
                .with_configuration(output_config()),
        )
        .build();
    let device_id = hardware.descriptors()[0].id.clone();

    let runtime = Arc::new(Mutex::new(Runtime::with_config(
        RuntimeConfig::new().with_max_retained_events(8),
    )));
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
        runtime.bind(&device_id).expect("bind");
    }

    let mut subscription = {
        let runtime = runtime
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        runtime.subscribe_blocking()
    };

    const WRITES: usize = 12;
    let runtime_for_worker = Arc::clone(&runtime);
    let device_id_for_worker = device_id.clone();
    let worker = thread::spawn(move || {
        for step in 0..WRITES {
            let level = if step % 2 == 0 {
                GpioLevel::High
            } else {
                GpioLevel::Low
            };
            let mut runtime = runtime_for_worker
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            runtime
                .request(DeviceRequest::new(
                    device_id_for_worker.clone(),
                    InteractionRequest::Standard(StandardRequest::Gpio(GpioRequest::Write {
                        level,
                    })),
                ))
                .expect("worker write should publish state event");
            drop(runtime);
            thread::sleep(Duration::from_millis(2));
        }
    });

    let mut total_events = 0;
    while total_events < WRITES {
        assert!(subscription.wait_for_update(Some(Duration::from_secs(1))));
        let batch_len = {
            let runtime = runtime
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            assert!(!subscription.is_stale(&runtime));
            let events = subscription.poll_with_status(&runtime);
            assert!(!events.was_truncated());
            assert!(
                events
                    .events()
                    .iter()
                    .all(|event| matches!(event, LemnosEvent::State(_)))
            );
            events.events().len()
        };
        assert!(batch_len > 0);
        total_events += batch_len;
    }

    worker.join().expect("worker thread should finish");

    {
        let runtime = runtime
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        total_events += subscription.poll_with_status(&runtime).events().len();
        assert!(!subscription.is_stale(&runtime));
        assert!(!subscription.has_pending(&runtime));
        assert_eq!(total_events, WRITES);
        assert_eq!(
            runtime
                .state(&device_id)
                .expect("cached state")
                .telemetry
                .get("level"),
            Some(&"low".into())
        );
    }
}
