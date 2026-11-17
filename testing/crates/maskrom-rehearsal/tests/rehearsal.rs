use lemnos_maskrom_rehearsal::{
    Behaviour, Observed, Refused, Stand, Stub, WDT_471_US, WDT_472_US, run_script,
};

#[test]
fn rehearsal_faithful_script_passes_every_step() {
    let mut stand = Stand::from_running_system(Behaviour::Faithful);
    let report = run_script(&mut stand).expect("the faithful stand-in must pass the script");
    let steps: Vec<&str> = report
        .lines
        .iter()
        .map(|l| l.split(':').next().unwrap())
        .collect();
    assert_eq!(steps, ["E", "1a", "1b", "2", "3", "4", "5", "end"]);
    assert!(stand.booted(), "the rehearsal ends with the board booted");
}

#[test]
fn rehearsal_no_reset_after_spin_fails_at_step_1a() {
    let mut stand = Stand::from_running_system(Behaviour::NoResetAfterSpin);
    let failure =
        run_script(&mut stand).expect_err("a 471 spin with no reset must fail the script");
    assert_eq!(failure.step, "1a");
    assert!(
        failure.reason.contains("no reset within 90 s"),
        "{}",
        failure.reason
    );
}

#[test]
fn rehearsal_refused_472_fails_at_step_2() {
    let mut stand = Stand::from_running_system(Behaviour::Refuses472);
    let failure = run_script(&mut stand).expect_err("a refused 472 must fail the script");
    assert_eq!(failure.step, "2");
    assert!(failure.reason.contains("472 refused"), "{}", failure.reason);
}

#[test]
fn rehearsal_471_is_refused_outside_maskrom() {
    let mut stand = Stand::from_running_system(Behaviour::Faithful);
    assert_eq!(stand.send_471(Stub::ArmAndSpin), Err(Refused::NotInMaskrom));
    assert!(stand.booted(), "a refused 471 leaves the system running");
}

#[test]
fn rehearsal_472_is_refused_without_a_returned_471() {
    let mut stand = Stand::from_running_system(Behaviour::Faithful);
    stand.enter_maskrom().unwrap();
    assert_eq!(stand.send_472(), Err(Refused::NoStubReturned));
}

#[test]
fn rehearsal_watchdog_periods_match_the_plan() {
    // TOP 15 and TOP 12 at 24 MHz: 2^(TOP+16) / 24e6 seconds, in microseconds.
    assert_eq!(WDT_471_US, (1u64 << 31) * 1_000_000 / 24_000_000);
    assert_eq!(WDT_472_US, (1u64 << 28) * 1_000_000 / 24_000_000);
}

#[test]
fn rehearsal_watchdog_survives_a_471_return_and_resets_inside_the_window() {
    let mut stand = Stand::from_running_system(Behaviour::Faithful);
    stand.enter_maskrom().unwrap();
    stand.send_471(Stub::ArmAndReturn).unwrap();
    assert_eq!(stand.observe(WDT_471_US - 1), Observed::NoReset);
    assert_eq!(stand.observe(2), Observed::Reset);
}
