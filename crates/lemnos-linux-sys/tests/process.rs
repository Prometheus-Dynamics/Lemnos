//! `process::spawn` in its own test binary: a child holds copies of the
//! parent's descriptors between its fork and its exec, which would race the
//! library's descriptor tests (a pipe's hangup seen late) if they shared one.

use lemnos_linux_sys::process::{Exit, spawn};

#[test]
fn exit_codes_and_signals_come_back() {
    assert_eq!(spawn(&["true"]).unwrap().wait().unwrap(), Exit::Code(0));
    assert_eq!(
        spawn(&["sh", "-c", "exit 7"]).unwrap().wait().unwrap(),
        Exit::Code(7)
    );
    // 15: SIGTERM.
    assert_eq!(
        spawn(&["sh", "-c", "kill -TERM $$"])
            .unwrap()
            .wait()
            .unwrap(),
        Exit::Signal(15)
    );
}

#[test]
fn try_wait_reports_a_running_child_then_its_end() {
    let mut c = spawn(&["sleep", "0.2"]).unwrap();
    assert_eq!(c.try_wait().unwrap(), None);
    assert_eq!(c.wait().unwrap(), Exit::Code(0));
    assert_eq!(c.try_wait().unwrap(), Some(Exit::Code(0)));
}

#[test]
fn a_missing_program_or_none_is_an_error() {
    assert!(spawn(&["lemnos-no-such-program"]).is_err());
    assert!(spawn::<&str>(&[]).is_err());
}
