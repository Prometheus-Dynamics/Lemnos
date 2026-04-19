use crate::{BusSession, GpioSession, PwmSession, SessionAccess, SessionState, StreamSession};
use lemnos_core::{DeviceDescriptor, GpioDirection, GpioLevel, InterfaceKind, PwmConfiguration};

/// Assert the common metadata and ownership contract shared by all sessions.
pub fn assert_session_contract(
    session: &dyn BusSession,
    expected_interface: InterfaceKind,
    expected_device: &DeviceDescriptor,
    expected_backend_name: &str,
    expected_access: SessionAccess,
) {
    assert_eq!(session.interface(), expected_interface);
    assert_eq!(session.device(), expected_device);
    assert_eq!(session.metadata().backend_name, expected_backend_name);
    assert_eq!(session.metadata().access, expected_access);
    assert_ne!(session.metadata().state, SessionState::Closed);
    assert!(session.metadata().opened_at.is_some());
    assert!(session.metadata().last_active_at.is_some());
}

/// Assert that a session can be cleanly closed and reports the closed state.
pub fn assert_close_contract(session: &mut dyn BusSession) {
    session.close().expect("close session");
    assert_eq!(session.metadata().state, SessionState::Closed);
    assert!(session.metadata().last_active_at.is_some());
}

/// Assert a writable GPIO session can read, write, and expose its configuration.
pub fn assert_gpio_round_trip_contract(
    session: &mut dyn GpioSession,
    initial_level: GpioLevel,
    written_level: GpioLevel,
    expected_direction: GpioDirection,
) {
    assert_eq!(
        session.read_level().expect("read initial level"),
        initial_level
    );
    session
        .write_level(written_level)
        .expect("write gpio level");
    assert_eq!(
        session.read_level().expect("read written level"),
        written_level
    );
    assert_eq!(
        session
            .configuration()
            .expect("gpio configuration")
            .direction,
        expected_direction
    );
}

/// Assert a PWM session reports an initial configuration and persists updates.
pub fn assert_pwm_configuration_contract(
    session: &mut dyn PwmSession,
    expected_initial: &PwmConfiguration,
    updated: &PwmConfiguration,
) {
    assert_eq!(
        session.configuration().expect("initial pwm configuration"),
        *expected_initial
    );
    session.configure(updated).expect("configure pwm");
    assert_eq!(
        session.configuration().expect("updated pwm configuration"),
        *updated
    );
}

/// Assert a polling stream session yields the expected typed event batch.
pub fn assert_stream_poll_contract<S>(
    session: &mut S,
    max_events: u32,
    timeout_ms: Option<u32>,
    expected: &[S::Event],
) where
    S: StreamSession + ?Sized,
    S::Event: std::fmt::Debug,
{
    let events = session
        .poll_events(max_events, timeout_ms)
        .expect("poll stream events");
    assert_eq!(events, expected);
}
