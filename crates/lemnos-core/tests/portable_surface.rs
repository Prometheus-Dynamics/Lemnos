#![cfg(feature = "serde")]

use lemnos_core::{
    CapabilityAccess, CapabilityDescriptor, ConfiguredGpioSignal, ConfiguredGpioSignalBinding,
    ConfiguredGpioSignalTarget, ConfiguredI2cEndpoint, ConfiguredSpiEndpoint, DeviceDescriptor,
    DeviceHealth, DeviceId, DeviceIssue, DeviceKind, DeviceLink, DeviceRelation, DeviceRequest,
    DeviceStateSnapshot, GpioEdge, I2cRequest, InteractionRequest, InventoryEvent, IssueCategory,
    IssueSeverity, LemnosEvent, OperationRecord, OperationStatus, StandardRequest, StateEvent,
    TimestampMs,
};
use std::sync::Arc;

#[test]
fn serde_round_trip_preserves_portable_descriptor_request_and_events() {
    let owner_id = DeviceId::new("configured.bmi088").expect("owner id");
    let descriptor = DeviceDescriptor::builder_for_kind(owner_id.as_str(), DeviceKind::I2cDevice)
        .expect("builder")
        .display_name("Configured BMI088")
        .summary("Synthetic configured device")
        .health(DeviceHealth::Degraded)
        .driver_hint("example.sensor.bmi088")
        .modalias("i2c:bmi088")
        .compatible("bosch,bmi088")
        .vendor("bosch")
        .model("bmi088")
        .revision("rev-a")
        .serial_number("imu-0")
        .hardware_id("board", "carrier-a")
        .capability(
            CapabilityDescriptor::new("i2c.write_read", CapabilityAccess::READ_WRITE)
                .expect("capability"),
        )
        .link(
            DeviceLink::new(
                DeviceId::new("i2c.4").expect("parent id"),
                DeviceRelation::Parent,
            )
            .with_attribute("name", "imu-bus"),
        )
        .property("requires_interrupt", true)
        .build()
        .expect("descriptor");

    let request = DeviceRequest::new(
        owner_id.clone(),
        InteractionRequest::Standard(StandardRequest::I2c(I2cRequest::WriteRead {
            write: vec![0x00, 0x0f],
            read_length: 2,
        })),
    );

    let issue = DeviceIssue::new(
        "imu.polling",
        IssueCategory::Configuration,
        IssueSeverity::Warning,
        "interrupt line not wired; falling back to polling",
    )
    .expect("issue")
    .with_first_seen_at(TimestampMs::new(1_000))
    .with_last_seen_at(TimestampMs::new(2_000))
    .with_attribute("mode", "polling");

    let state = DeviceStateSnapshot::new(owner_id.clone())
        .with_health(DeviceHealth::Degraded)
        .with_issue(issue)
        .with_observed_at(TimestampMs::new(2_500))
        .with_updated_at(TimestampMs::new(2_750))
        .with_config("accel_odr_hz", 400_u64)
        .with_telemetry("temperature_c", 34.5_f64)
        .with_last_operation(
            OperationRecord::new("sensor.imu.sample", OperationStatus::Succeeded)
                .with_observed_at(TimestampMs::new(2_700))
                .with_output("ok"),
        );

    let event = LemnosEvent::State(Box::new(StateEvent::Snapshot(Arc::new(state))));

    let endpoint = ConfiguredI2cEndpoint::new("accel", 4, 0x18);
    let spi_endpoint = ConfiguredSpiEndpoint::new("flash", 2, 0);
    let signal = ConfiguredGpioSignalBinding::new(
        "accel-int",
        ConfiguredGpioSignal::by_chip_line("gpiochip4", 23)
            .with_global_line(151)
            .with_edge(GpioEdge::Rising)
            .with_active_low(false)
            .with_required(true),
    );

    let encoded_descriptor = serde_json::to_string(&descriptor).expect("serialize descriptor");
    let encoded_request = serde_json::to_string(&request).expect("serialize request");
    let encoded_event = serde_json::to_string(&event).expect("serialize event");
    let encoded_endpoint = serde_json::to_string(&endpoint).expect("serialize endpoint");
    let encoded_spi_endpoint =
        serde_json::to_string(&spi_endpoint).expect("serialize spi endpoint");
    let encoded_signal = serde_json::to_string(&signal).expect("serialize signal");

    let decoded_descriptor: DeviceDescriptor =
        serde_json::from_str(&encoded_descriptor).expect("deserialize descriptor");
    let decoded_request: DeviceRequest =
        serde_json::from_str(&encoded_request).expect("deserialize request");
    let decoded_event: LemnosEvent =
        serde_json::from_str(&encoded_event).expect("deserialize event");
    let decoded_endpoint: ConfiguredI2cEndpoint =
        serde_json::from_str(&encoded_endpoint).expect("deserialize endpoint");
    let decoded_spi_endpoint: ConfiguredSpiEndpoint =
        serde_json::from_str(&encoded_spi_endpoint).expect("deserialize spi endpoint");
    let decoded_signal: ConfiguredGpioSignalBinding =
        serde_json::from_str(&encoded_signal).expect("deserialize signal");

    assert_eq!(decoded_descriptor, descriptor);
    assert_eq!(decoded_request, request);
    assert_eq!(decoded_event, event);
    assert_eq!(decoded_endpoint, endpoint);
    assert_eq!(decoded_spi_endpoint, spi_endpoint);
    assert_eq!(decoded_signal, signal);
}

#[test]
fn serde_rejects_invalid_identifier_values() {
    let invalid_device_id = serde_json::from_str::<DeviceId>("\"invalid device id\"");
    assert!(invalid_device_id.is_err());

    let invalid_target =
        serde_json::from_str::<ConfiguredGpioSignalTarget>(r#"{"device":"not valid either"}"#);
    assert!(invalid_target.is_err());
}

#[test]
fn serde_round_trip_preserves_inventory_events() {
    let descriptor = DeviceDescriptor::builder_for_kind("gpiochip4-line23", DeviceKind::GpioLine)
        .expect("builder")
        .summary("Configured interrupt line")
        .build()
        .expect("descriptor");

    let event = LemnosEvent::Inventory(Box::new(InventoryEvent::Added(Box::new(descriptor))));
    let encoded = serde_json::to_string(&event).expect("serialize inventory event");
    let decoded: LemnosEvent = serde_json::from_str(&encoded).expect("deserialize inventory event");

    assert_eq!(decoded, event);
}
