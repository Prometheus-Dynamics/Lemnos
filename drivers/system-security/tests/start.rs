//! A board file cannot point the security switch elsewhere: the view and its render directory are
//! the board's own, so a board file that names them does not start the driver.

use lemnos_core::{Config, ConfigValue, Lifecycle};
use lemnos_driver_system_security::{SystemSecurity, SystemSecurityNeeds};

fn start(entries: &[(&str, ConfigValue<'_>)]) -> bool {
    let cfg = Config::new(entries).on_board("test");
    let needs = SystemSecurityNeeds {
        slots: None,
        flash: None,
    };
    <SystemSecurity as Lifecycle>::start(needs, &cfg).is_ok()
}

#[test]
fn the_record_alone_starts_the_driver() {
    let record = ConfigValue::Str("/tmp/lemnos-security-start-test.rec");
    assert!(start(&[("record", record)]));
}

#[test]
fn a_board_file_naming_the_render_directory_is_refused() {
    let record = ConfigValue::Str("/tmp/lemnos-security-start-test.rec");
    let run = ConfigValue::Str("/tmp");
    assert!(!start(&[("record", record), ("run", run)]));
}

#[test]
fn a_board_file_naming_the_view_is_refused() {
    let record = ConfigValue::Str("/tmp/lemnos-security-start-test.rec");
    let view = ConfigValue::Str("/tmp/security");
    assert!(!start(&[("record", record), ("security_view", view)]));
}

#[test]
fn a_board_file_without_a_record_is_refused() {
    assert!(!start(&[]));
}
