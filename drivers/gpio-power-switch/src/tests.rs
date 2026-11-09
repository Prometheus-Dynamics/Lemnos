use super::*;
use lemnos_core::{Driver, Resources};
use lemnos_hal::ErrorKind;
use lemnos_hal::raw::LineConfig;
use lemnos_sim::SimBoard;
use lemnos_sim::mock::MockLine;

/// A board with the output line `line` and, if `with_fault`, the fault input.
fn wired(with_fault: bool) -> (SimBoard, MockLine, Option<MockLine>) {
    let board = SimBoard::new();
    let line = board.add_line("line");
    let fault = with_fault.then(|| board.add_line("fault"));
    (board, line, fault)
}

/// Binds the switch with `entries` as its `config`.
fn bind(
    board: &mut SimBoard,
    entries: &[(&str, ConfigValue<'_>)],
) -> lemnos_core::Result<GpioPowerSwitch> {
    let mut resources = Resources::new(board, None);
    GpioPowerSwitch::bind(&mut resources, &Config::new(entries))
}

#[test]
fn starts_at_its_default_level_with_no_later_write() {
    let (mut board, line, _) = wired(false);
    let mut switch = bind(&mut board, &[]).unwrap();
    let configs = line.configs();
    assert_eq!(configs.len(), 1, "configured once, at the default level");
    assert_eq!(configs[0], LineConfig::output(true));
    assert_eq!(line.level(), Some(true));
    assert!(switch.is_on().unwrap());
}

#[test]
fn default_off_starts_the_line_low() {
    let (mut board, line, _) = wired(false);
    let mut switch = bind(&mut board, &[("default_on", ConfigValue::Bool(false))]).unwrap();
    assert_eq!(line.configs(), [LineConfig::output(false)]);
    assert_eq!(line.level(), Some(false));
    assert!(!switch.is_on().unwrap());
}

#[test]
fn set_drives_the_line_and_reads_back() {
    let (mut board, line, _) = wired(false);
    let mut switch = bind(&mut board, &[]).unwrap();
    switch.set(false).unwrap();
    assert_eq!(line.level(), Some(false));
    assert!(!switch.is_on().unwrap());
    switch.set(true).unwrap();
    assert_eq!(line.level(), Some(true));
    assert!(switch.is_on().unwrap());
    assert_eq!(line.configs().len(), 1, "writes never reconfigure the line");
}

#[test]
fn active_low_inverts_the_physical_level() {
    let (mut board, line, _) = wired(false);
    let mut switch = bind(&mut board, &[("active_low", ConfigValue::Bool(true))]).unwrap();
    assert_eq!(line.level(), Some(false), "logical on is a low pin");
    assert!(switch.is_on().unwrap());
    switch.set(false).unwrap();
    assert_eq!(line.level(), Some(true), "logical off is a high pin");
    assert!(!switch.is_on().unwrap());
}

#[test]
fn a_failed_write_reports_and_the_next_call_tries_again() {
    let (mut board, line, _) = wired(false);
    let mut switch = bind(&mut board, &[]).unwrap();
    line.fail(Some(ErrorKind::Busy));
    assert_eq!(switch.set(false), Err(Status::BUSY));
    assert!(
        switch.is_on().unwrap(),
        "a failed write leaves the state as it was"
    );
    line.fail(None);
    switch.set(false).unwrap();
    assert_eq!(line.level(), Some(false));
}

#[test]
fn a_line_that_refuses_at_bind_fails_the_bind() {
    let (mut board, line, _) = wired(false);
    line.fail(Some(ErrorKind::Busy));
    assert!(bind(&mut board, &[]).is_err());
    line.fail(None);
    assert!(bind(&mut board, &[]).is_ok(), "a later bind tries again");
}

#[test]
fn a_non_boolean_setting_is_refused() {
    let (mut board, line, _) = wired(false);
    assert!(bind(&mut board, &[("default_on", ConfigValue::Int(1))]).is_err());
    assert!(
        line.configs().is_empty(),
        "refused before the line is touched"
    );
}

#[test]
fn fault_input_reads_asserted_when_the_pin_is_low() {
    let (mut board, _line, fault) = wired(true);
    let fault = fault.unwrap();
    let mut switch = bind(&mut board, &[]).unwrap();
    fault.drive(true);
    assert_eq!(
        switch.fault().unwrap(),
        Some(false),
        "active low: high is no fault"
    );
    fault.drive(false);
    assert_eq!(
        switch.fault().unwrap(),
        Some(true),
        "active low: low is a fault"
    );
}

#[test]
fn fault_polarity_follows_its_setting() {
    let (mut board, _line, fault) = wired(true);
    let fault = fault.unwrap();
    let mut switch = bind(
        &mut board,
        &[("fault_active_low", ConfigValue::Bool(false))],
    )
    .unwrap();
    fault.drive(true);
    assert_eq!(switch.fault().unwrap(), Some(true));
}

#[test]
fn fault_input_adds_a_channel() {
    let (mut board, _line, _) = wired(true);
    let switch = bind(&mut board, &[]).unwrap();
    let mut description = Description::default();
    Driver::describe(&switch, &mut description);
    assert!(description.find("power.on").is_some());
    assert!(description.find("power.fault").is_some());
}

#[test]
fn no_fault_input_has_one_channel() {
    let (mut board, _line, _) = wired(false);
    let mut switch = bind(&mut board, &[]).unwrap();
    let mut description = Description::default();
    Driver::describe(&switch, &mut description);
    assert!(description.find("power.on").is_some());
    assert!(description.find("power.fault").is_none());
    assert_eq!(switch.fault().unwrap(), None);
}
