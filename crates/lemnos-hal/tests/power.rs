use lemnos_hal::mock::{MockClock, MockPin, MockRegulator};
use lemnos_hal::{ClockOutput, ErrorKind, FixedClock, GpioRegulator, Regulator};

#[test]
fn gpio_regulator_drives_its_enable_pin() {
    let mut reg = GpioRegulator::with_polarity(MockPin::new(), true)
        .unwrap()
        .with_voltage_uv(1_800_000);
    reg.enable().unwrap();
    assert!(reg.is_enabled().unwrap());
    reg.set_enabled(false).unwrap();
    assert_eq!(reg.set_voltage_uv(1_700_000, 1_900_000), Ok(1_800_000));
    assert_eq!(
        reg.set_voltage_uv(2_500_000, 2_800_000),
        Err(ErrorKind::InvalidInput)
    );
    // Active low: off (high) at construction, on (low), off (high).
    assert_eq!(reg.release().log, [true, false, true]);
}

#[test]
fn clocks_accept_their_rate_or_change_it() {
    let mut fixed = FixedClock::new(24_000_000);
    fixed.set(Some(24_000_000)).unwrap();
    assert!(fixed.is_enabled());
    assert_eq!(fixed.set(Some(19_200_000)), Err(ErrorKind::InvalidInput));
    fixed.set(None).unwrap();
    assert!(!fixed.is_enabled());

    let mut clock = MockClock::default();
    clock.set(Some(24_000_000)).unwrap();
    clock.set(None).unwrap();
    assert_eq!(clock.log, [Some(24_000_000), None]);

    let mut supply = MockRegulator::default();
    supply.enable().unwrap();
    assert_eq!(supply.set_voltage_uv(1, 2), Err(ErrorKind::Unsupported));
    assert_eq!(supply.log, [true]);
}
