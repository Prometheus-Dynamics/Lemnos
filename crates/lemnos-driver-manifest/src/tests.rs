use crate::{DriverManifest, DriverPriority, DriverVersion, MatchCondition, MatchRule};
#[cfg(feature = "serde")]
use lemnos_core::Value;
use lemnos_core::{
    CapabilityAccess, CapabilityDescriptor, CapabilityId, DeviceDescriptor, DeviceKind,
    InterfaceKind,
};

fn gpio_line() -> DeviceDescriptor {
    DeviceDescriptor::builder_for_kind("gpio.line0", DeviceKind::GpioLine)
        .unwrap()
        .vendor("acme")
        .model("mk1")
        .compatible("acme,gpio-line")
        .capability(
            CapabilityDescriptor::new("gpio.write", CapabilityAccess::WRITE).expect("capability"),
        )
        .label("line_name", "status")
        .build()
        .expect("descriptor")
}

#[test]
fn validates_manifest_and_interaction_ids() {
    let manifest = DriverManifest::new("lemnos.gpio.generic", "GPIO", vec![InterfaceKind::Gpio])
        .with_version(DriverVersion::new(1, 2, 3))
        .with_standard_interaction("gpio.read", "Read a line")
        .with_custom_interaction("vendor.calibrate", "Calibrate board");

    manifest.validate().expect("manifest should validate");
    assert_eq!(manifest.version, DriverVersion::new(1, 2, 3));
}

#[test]
fn matches_device_on_kind_and_rule() {
    let manifest = DriverManifest::new("lemnos.gpio.generic", "GPIO", vec![InterfaceKind::Gpio])
        .with_priority(DriverPriority::Preferred)
        .with_kind(DeviceKind::GpioLine)
        .with_rule(
            MatchRule::new(40)
                .described("vendor/model matched")
                .require(MatchCondition::Vendor("acme".into()))
                .require(MatchCondition::Model("mk1".into())),
        );

    let report = manifest.match_device(&gpio_line());
    assert!(report.matched);
    assert!(report.score >= 265);
    assert_eq!(report.matched_rule, Some(0));
}

#[test]
fn capability_condition_matches_descriptor_capabilities() {
    let manifest = DriverManifest::new("lemnos.gpio.write", "GPIO", vec![InterfaceKind::Gpio])
        .with_kind(DeviceKind::GpioLine)
        .with_rule(MatchRule::new(10).require(MatchCondition::Capability(
            CapabilityId::new("gpio.write").expect("capability id"),
        )));

    let report = manifest.match_device(&gpio_line());
    assert!(report.matched);
}

#[test]
fn driver_manifest_defaults_to_initial_version() {
    let manifest = DriverManifest::new("lemnos.gpio.default", "GPIO", vec![InterfaceKind::Gpio]);
    assert_eq!(manifest.version, DriverVersion::new(0, 1, 0));
}

#[cfg(feature = "serde")]
#[test]
fn manifest_json_round_trip_preserves_matching_shape() {
    let manifest = DriverManifest::new("lemnos.gpio.json", "GPIO JSON", vec![InterfaceKind::Gpio])
        .with_version(DriverVersion::new(1, 0, 0))
        .with_priority(DriverPriority::Preferred)
        .with_kind(DeviceKind::GpioLine)
        .with_standard_interaction("gpio.read", "Read a line")
        .with_custom_interaction("vendor.calibrate", "Calibrate board")
        .with_rule(
            MatchRule::new(20)
                .described("requires vendor and property")
                .require(MatchCondition::Vendor("acme".into()))
                .require(MatchCondition::PropertyEq {
                    key: "profile".into(),
                    value: Value::from("test"),
                }),
        )
        .with_tag("json");

    let json = manifest.to_json_pretty().expect("serialize manifest");
    let restored = DriverManifest::from_json(&json).expect("deserialize manifest");

    assert_eq!(restored, manifest);
    assert!(json.contains("\"version\""));
    assert!(json.contains("\"interfaces\""));
    assert!(json.contains("\"rules\""));
}

#[test]
fn manifest_rejects_missing_interfaces() {
    let err = DriverManifest::new("lemnos.invalid", "Invalid", Vec::new())
        .validate()
        .expect_err("manifest without interfaces should fail");

    assert!(
        err.to_string()
            .contains("must declare at least one interface")
    );
}

#[test]
fn manifest_match_reports_unsupported_kind() {
    let manifest = DriverManifest::new("lemnos.gpio.line", "GPIO line", vec![InterfaceKind::Gpio])
        .with_kind(DeviceKind::GpioChip);

    let report = manifest.match_device(&gpio_line());
    assert!(!report.matched);
    assert!(
        report
            .reasons
            .first()
            .expect("reason")
            .contains("device kind")
    );
}

#[test]
fn manifest_match_reports_rule_miss() {
    let manifest = DriverManifest::new(
        "lemnos.gpio.vendor",
        "GPIO vendor",
        vec![InterfaceKind::Gpio],
    )
    .with_kind(DeviceKind::GpioLine)
    .with_rule(
        MatchRule::new(10)
            .described("vendor must match")
            .require(MatchCondition::Vendor("other-vendor".into())),
    );

    let report = manifest.match_device(&gpio_line());
    assert!(!report.matched);
    assert!(
        report
            .reasons
            .first()
            .expect("reason")
            .contains("no manifest rules matched")
    );
}

#[test]
fn manifest_preserves_standard_and_custom_interaction_declarations() {
    let manifest = DriverManifest::new(
        "lemnos.gpio.actions",
        "GPIO actions",
        vec![InterfaceKind::Gpio],
    )
    .with_standard_interaction("gpio.read", "Read a GPIO line")
    .with_standard_interaction("gpio.write", "Write a GPIO line")
    .with_custom_interaction("vendor.calibrate", "Calibrate attached board");

    manifest.validate().expect("manifest should validate");

    assert_eq!(manifest.standard_interactions.len(), 2);
    assert_eq!(manifest.custom_interactions.len(), 1);
    assert_eq!(manifest.standard_interactions[0].id.as_str(), "gpio.read");
    assert_eq!(manifest.standard_interactions[1].id.as_str(), "gpio.write");
    assert_eq!(
        manifest.custom_interactions[0].id.as_str(),
        "vendor.calibrate"
    );
}
