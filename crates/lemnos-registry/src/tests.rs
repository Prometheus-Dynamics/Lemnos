use crate::{DriverId, DriverRegistry, RegistryError};
use lemnos_core::{DeviceDescriptor, DeviceKind, InterfaceKind};
use lemnos_driver_manifest::{DriverManifest, DriverPriority, MatchCondition, MatchRule};
use lemnos_driver_sdk::{Driver, DriverMatchLevel};
use std::borrow::Cow;
use std::time::Instant;

struct GenericGpioDriver;
struct PreferredGpioDriver;
struct AnotherPreferredGpioDriver;
struct BenchmarkGpioDriver {
    id: String,
    rule_score: u32,
}

impl Driver for GenericGpioDriver {
    fn id(&self) -> &str {
        "driver.gpio.generic"
    }

    fn interface(&self) -> InterfaceKind {
        InterfaceKind::Gpio
    }

    fn manifest_ref(&self) -> Cow<'static, DriverManifest> {
        Cow::Owned(
            DriverManifest::new(self.id(), "Generic GPIO", vec![InterfaceKind::Gpio])
                .with_priority(DriverPriority::Generic)
                .with_kind(DeviceKind::GpioLine),
        )
    }
}

impl Driver for PreferredGpioDriver {
    fn id(&self) -> &str {
        "driver.gpio.preferred"
    }

    fn interface(&self) -> InterfaceKind {
        InterfaceKind::Gpio
    }

    fn manifest_ref(&self) -> Cow<'static, DriverManifest> {
        Cow::Owned(
            DriverManifest::new(self.id(), "Preferred GPIO", vec![InterfaceKind::Gpio])
                .with_priority(DriverPriority::Preferred)
                .with_kind(DeviceKind::GpioLine)
                .with_rule(MatchRule::new(50).require(MatchCondition::Vendor("acme".into()))),
        )
    }
}

impl Driver for AnotherPreferredGpioDriver {
    fn id(&self) -> &str {
        "driver.gpio.preferred.2"
    }

    fn interface(&self) -> InterfaceKind {
        InterfaceKind::Gpio
    }

    fn manifest_ref(&self) -> Cow<'static, DriverManifest> {
        Cow::Owned(
            DriverManifest::new(self.id(), "Preferred GPIO 2", vec![InterfaceKind::Gpio])
                .with_priority(DriverPriority::Preferred)
                .with_kind(DeviceKind::GpioLine)
                .with_rule(MatchRule::new(50).require(MatchCondition::Vendor("acme".into()))),
        )
    }
}

impl Driver for BenchmarkGpioDriver {
    fn id(&self) -> &str {
        self.id.as_str()
    }

    fn interface(&self) -> InterfaceKind {
        InterfaceKind::Gpio
    }

    fn manifest_ref(&self) -> Cow<'static, DriverManifest> {
        Cow::Owned(
            DriverManifest::new(self.id(), "Benchmark GPIO", vec![InterfaceKind::Gpio])
                .with_priority(DriverPriority::Generic)
                .with_kind(DeviceKind::GpioLine)
                .with_rule(
                    MatchRule::new(self.rule_score).require(MatchCondition::Vendor("bench".into())),
                ),
        )
    }
}

fn gpio_line(vendor: Option<&str>) -> DeviceDescriptor {
    let builder =
        DeviceDescriptor::builder_for_kind("gpio.line0", DeviceKind::GpioLine).expect("builder");
    let builder = if let Some(vendor) = vendor {
        builder.vendor(vendor)
    } else {
        builder
    };
    builder.build().expect("descriptor")
}

#[test]
fn register_rejects_duplicate_driver_ids() {
    let mut registry = DriverRegistry::default();
    registry.register(GenericGpioDriver).expect("register");
    let err = registry
        .register(GenericGpioDriver)
        .expect_err("duplicate register should fail");

    assert!(matches!(err, RegistryError::DuplicateDriverId { .. }));
}

#[test]
fn resolve_prefers_higher_ranked_driver() {
    let mut registry = DriverRegistry::default();
    registry
        .register(GenericGpioDriver)
        .expect("register generic");
    registry
        .register(PreferredGpioDriver)
        .expect("register preferred");

    let candidate = registry.resolve(&gpio_line(Some("acme"))).expect("resolve");
    assert_eq!(candidate.driver_id, "driver.gpio.preferred");
    assert_eq!(candidate.match_result.level, DriverMatchLevel::Preferred);
}

#[test]
fn resolve_reports_conflicts_for_equal_top_matches() {
    let mut registry = DriverRegistry::default();
    registry
        .register(PreferredGpioDriver)
        .expect("register preferred");
    registry
        .register(AnotherPreferredGpioDriver)
        .expect("register preferred 2");

    let err = match registry.resolve(&gpio_line(Some("acme"))) {
        Ok(candidate) => panic!("expected conflict, resolved to {}", candidate.driver_id),
        Err(err) => err,
    };
    assert!(matches!(err, RegistryError::ConflictingMatches { .. }));
}

#[test]
fn best_match_returns_none_when_unmatched() {
    let mut registry = DriverRegistry::default();
    registry.register(GenericGpioDriver).expect("register");

    let unmatched = DeviceDescriptor::builder_for_kind("uart.port0", DeviceKind::UartPort)
        .expect("builder")
        .build()
        .expect("descriptor");

    assert!(registry.best_match(&unmatched).is_none());
}

#[test]
fn preferred_driver_must_exist_before_it_can_be_pinned() {
    let mut registry = DriverRegistry::default();
    let device = gpio_line(None);

    let err = registry
        .prefer_driver_for_device(device.id.clone(), "driver.gpio.missing")
        .expect_err("preference should require a registered driver");

    assert!(matches!(err, RegistryError::UnknownPreferredDriver { .. }));
}

#[test]
fn resolve_honors_device_level_driver_preferences() {
    let mut registry = DriverRegistry::default();
    let device = gpio_line(Some("acme"));
    registry
        .register(GenericGpioDriver)
        .expect("register generic");
    registry
        .register(PreferredGpioDriver)
        .expect("register preferred");
    registry
        .prefer_driver_for_device(device.id.clone(), DriverId::from("driver.gpio.generic"))
        .expect("pin driver");

    let candidate = registry.resolve(&device).expect("resolve preferred driver");
    assert_eq!(candidate.driver_id, "driver.gpio.generic");
}

#[test]
fn resolve_reports_preferred_driver_mismatches() {
    let mut registry = DriverRegistry::default();
    let device = gpio_line(None);
    registry
        .register(GenericGpioDriver)
        .expect("register generic");
    registry
        .register(PreferredGpioDriver)
        .expect("register preferred");
    registry
        .prefer_driver_for_device(device.id.clone(), DriverId::from("driver.gpio.preferred"))
        .expect("pin driver");

    let err = match registry.resolve(&device) {
        Ok(candidate) => panic!(
            "preferred mismatch should fail, resolved to {}",
            candidate.driver_id
        ),
        Err(err) => err,
    };

    assert!(matches!(
        err,
        RegistryError::PreferredDriverDidNotMatch { .. }
    ));
}

#[test]
fn match_report_lists_supported_and_rejected_candidates() {
    let mut registry = DriverRegistry::default();
    registry
        .register(GenericGpioDriver)
        .expect("register generic");
    registry
        .register(PreferredGpioDriver)
        .expect("register preferred");

    let report = registry.match_report(&gpio_line(None));

    assert_eq!(report.supported.len(), 1);
    assert_eq!(
        report.supported[0].driver_id,
        DriverId::from("driver.gpio.generic")
    );
    assert_eq!(report.rejected.len(), 1);
    assert_eq!(
        report.rejected[0].driver_id,
        DriverId::from("driver.gpio.preferred")
    );
    assert!(report.preferred_driver_id.is_none());
    assert!(
        report.rejected[0]
            .reasons
            .iter()
            .any(|reason| reason.contains("no manifest rules matched"))
    );
}

#[test]
fn match_report_identifies_conflicting_top_matches() {
    let mut registry = DriverRegistry::default();
    registry
        .register(PreferredGpioDriver)
        .expect("register preferred");
    registry
        .register(AnotherPreferredGpioDriver)
        .expect("register preferred 2");

    let report = registry.match_report(&gpio_line(Some("acme")));
    let conflicts = report.conflicting_top_matches();

    assert_eq!(conflicts.len(), 2);
    assert_eq!(
        conflicts[0].driver_id,
        DriverId::from("driver.gpio.preferred")
    );
    assert_eq!(
        conflicts[1].driver_id,
        DriverId::from("driver.gpio.preferred.2")
    );
}

#[test]
fn match_report_exposes_the_pinned_driver() {
    let mut registry = DriverRegistry::default();
    let device = gpio_line(Some("acme"));
    registry
        .register(GenericGpioDriver)
        .expect("register generic");
    registry
        .register(PreferredGpioDriver)
        .expect("register preferred");
    registry
        .prefer_driver_for_device(device.id.clone(), DriverId::from("driver.gpio.generic"))
        .expect("pin driver");

    let report = registry.match_report(&device);

    assert_eq!(
        report.preferred_driver_id.as_ref(),
        Some(&DriverId::from("driver.gpio.generic"))
    );
    assert_eq!(
        report
            .preferred()
            .map(|candidate| candidate.driver_id.as_str()),
        Some("driver.gpio.generic")
    );
}

#[test]
fn resolve_scales_to_larger_driver_sets_without_changing_preference_order() {
    let mut registry = DriverRegistry::default();
    for index in 0..64 {
        registry
            .register(BenchmarkGpioDriver {
                id: format!("driver.gpio.bench.{index:02}"),
                rule_score: index,
            })
            .expect("register benchmark driver");
    }

    let candidate = registry
        .resolve(&gpio_line(Some("bench")))
        .expect("resolve");
    assert_eq!(candidate.driver_id, "driver.gpio.bench.63");
    assert_eq!(candidate.match_result.level, DriverMatchLevel::Generic);
}

#[test]
#[ignore = "benchmark-style diagnostic for release prep; run with --ignored --nocapture"]
fn resolve_benchmark_reports_larger_registry_match_cost() {
    let mut registry = DriverRegistry::default();
    for index in 0..128 {
        registry
            .register(BenchmarkGpioDriver {
                id: format!("driver.gpio.bench.{index:03}"),
                rule_score: index,
            })
            .expect("register benchmark driver");
    }

    let device = gpio_line(Some("bench"));
    let started_at = Instant::now();
    for _ in 0..1_000 {
        let candidate = registry.resolve(&device).expect("resolve");
        assert_eq!(candidate.driver_id, "driver.gpio.bench.127");
    }
    assert!(started_at.elapsed().as_secs() < 5);
}
