#![cfg(all(feature = "tokio", feature = "mock", feature = "builtin-drivers"))]

#[path = "support/mock_gpio.rs"]
mod mock_gpio_support;

use lemnos::mock::{MockGpioLine, MockHardware};
use lemnos::prelude::*;
use std::sync::Arc;

#[tokio::test(flavor = "multi_thread")]
async fn async_facade_builds_refreshes_and_dispatches_gpio_requests() {
    let hardware = MockHardware::builder()
        .with_gpio_line(
            MockGpioLine::new("gpiochip0", 17)
                .with_line_name("status")
                .with_configuration(mock_gpio_support::output_config()),
        )
        .build();
    let device_id = hardware.descriptors()[0].id.clone();

    let lemnos = Lemnos::builder()
        .with_mock_hardware_ref(&hardware)
        .with_builtin_drivers()
        .expect("register built-in drivers")
        .build_async();

    let subscription = lemnos.subscribe_from_start();
    lemnos
        .refresh_with_mock_default(&hardware)
        .await
        .expect("refresh with mock");

    let event_batch = subscription.poll().await.expect("poll subscription");
    assert_eq!(event_batch.len(), 1);
    assert!(lemnos.inventory().contains(&device_id));

    lemnos
        .write_gpio(device_id.clone(), GpioLevel::High)
        .await
        .expect("write gpio");
    assert_eq!(hardware.gpio_level(&device_id), Some(GpioLevel::High));
    assert_eq!(
        lemnos
            .state(&device_id)
            .expect("cached gpio state")
            .telemetry
            .get("level"),
        Some(&"high".into())
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn async_facade_allows_runtime_owned_backend_configuration_helpers() {
    let hardware = MockHardware::builder()
        .with_gpio_line(
            MockGpioLine::new("gpiochip0", 23)
                .with_line_name("status")
                .with_configuration(mock_gpio_support::output_config()),
        )
        .build();
    let device_id = hardware.descriptors()[0].id.clone();

    let lemnos = AsyncLemnos::new();
    lemnos.set_mock_hardware_ref(&hardware);
    lemnos
        .register_builtin_drivers()
        .await
        .expect("register built-in drivers");

    lemnos
        .refresh_with_mock_default(&hardware)
        .await
        .expect("refresh with mock");
    lemnos
        .write_gpio(device_id.clone(), GpioLevel::High)
        .await
        .expect("write gpio");

    assert_eq!(hardware.gpio_level(&device_id), Some(GpioLevel::High));
}

#[tokio::test(flavor = "multi_thread")]
async fn async_facade_exposes_typed_driver_id_preferences() {
    let hardware = MockHardware::builder()
        .with_gpio_line(
            MockGpioLine::new("gpiochip0", 24)
                .with_line_name("status")
                .with_configuration(mock_gpio_support::output_config()),
        )
        .build();
    let device_id = hardware.descriptors()[0].id.clone();

    let lemnos = Lemnos::builder()
        .with_mock_hardware_ref(&hardware)
        .with_builtin_drivers()
        .expect("register built-in drivers")
        .build_async();

    lemnos
        .refresh_with_mock_default(&hardware)
        .await
        .expect("refresh with mock");

    let driver_id = DriverId::from("lemnos.gpio.generic");
    lemnos
        .prefer_driver_id_for_device(device_id.clone(), driver_id.clone())
        .await
        .expect("set preferred driver");

    assert_eq!(
        lemnos
            .preferred_driver_id_for_device(device_id.clone())
            .await
            .expect("read preferred driver"),
        Some(driver_id.clone())
    );
    assert_eq!(
        lemnos
            .clear_preferred_driver_id_for_device(device_id.clone())
            .await
            .expect("clear preferred driver"),
        Some(driver_id)
    );
    assert_eq!(
        lemnos
            .preferred_driver_id_for_device(device_id)
            .await
            .expect("read cleared preferred driver"),
        None
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn async_facade_refresh_state_shared_reuses_cached_snapshot_arc() {
    let hardware = MockHardware::builder()
        .with_gpio_line(
            MockGpioLine::new("gpiochip0", 25)
                .with_line_name("status")
                .with_configuration(mock_gpio_support::output_config()),
        )
        .build();
    let device_id = hardware.descriptors()[0].id.clone();

    let lemnos = Lemnos::builder()
        .with_mock_hardware_ref(&hardware)
        .with_builtin_drivers()
        .expect("register built-in drivers")
        .build_async();

    lemnos
        .refresh_with_mock_default(&hardware)
        .await
        .expect("refresh with mock");
    lemnos.bind(device_id.clone()).await.expect("bind gpio");

    let refreshed = lemnos
        .refresh_state_shared(device_id.clone())
        .await
        .expect("refresh shared state")
        .expect("shared state");
    let cached = lemnos
        .shared_state(&device_id)
        .expect("cached shared state");

    assert!(Arc::ptr_eq(&refreshed, &cached));
}
