//! The daemon's own error-output lines stay whole when another process writes to the same
//! descriptor. The gadget supervisor is forked from the daemon and shares its descriptor 2; a
//! daemon line written in pieces lets the supervisor's line land in the middle of it, and a reader
//! that waits for a whole line (the QEMU checks wait for `start trace: <stamp> root phase done`)
//! never sees it.
//!
//! The test runs itself twice more: a tracer (start trace on, writing trace and warning lines) and,
//! started by the tracer so it shares the tracer's descriptor 2, a writer that logs as the
//! supervisor does. Both write into one pipe the test reads; every line must be one of theirs, whole.

use std::io::Read;
use std::process::{Command, Stdio};

const ROLE: &str = "LEMNOSD_TEST_STDERR_ROLE";
const TEST: &str = "daemon_lines_stay_whole_beside_a_writer_child";
/// Lines each side writes. Enough that pieces written apart meet the other side's lines.
const LINES: usize = 20_000;

fn again(role: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", TEST, "--nocapture", "--test-threads=1"])
        .env(ROLE, role)
        .stdin(Stdio::null())
        .stdout(Stdio::null());
    command
}

fn tracer() {
    lemnosd::start_trace_on();
    // Inherits descriptor 2, as the forked supervisor does.
    let mut writer = again("writer").spawn().unwrap();
    for i in 0..LINES {
        if i % 4 == 0 {
            lemnosd::warning_line(&format!("warning {i}"));
        } else {
            lemnosd::start_trace_line(&format!("event {i}"));
        }
    }
    assert!(writer.wait().unwrap().success(), "the writer child failed");
}

fn writer() {
    for i in 0..LINES {
        lemnos_linux_sys::stderr::line(&format!("lemnosd-gsuper: running, for daemon pid {i}"));
    }
}

/// Whether `line` is one whole line of the tracer's or the writer's.
fn whole(line: &str) -> bool {
    if let Some(n) = line.strip_prefix("lemnosd-gsuper: running, for daemon pid ") {
        return n.parse::<usize>().is_ok();
    }
    if let Some(n) = line.strip_prefix("<4>lemnosd: warning ") {
        return n.parse::<usize>().is_ok();
    }
    let Some(rest) = line.strip_prefix("lemnosd: start trace: ") else {
        return false;
    };
    let Some((stamp, event)) = rest.split_once(' ') else {
        return false;
    };
    let Some((whole_secs, micros)) = stamp.split_once('.') else {
        return false;
    };
    whole_secs.parse::<u64>().is_ok()
        && micros.len() == 6
        && micros.parse::<u32>().is_ok()
        && event
            .strip_prefix("event ")
            .is_some_and(|n| n.parse::<usize>().is_ok())
}

#[test]
fn daemon_lines_stay_whole_beside_a_writer_child() {
    match std::env::var(ROLE).as_deref() {
        Ok("tracer") => return tracer(),
        Ok("writer") => return writer(),
        _ => {}
    }
    let mut child = again("tracer").stderr(Stdio::piped()).spawn().unwrap();
    let mut out = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut out)
        .unwrap();
    assert!(child.wait().unwrap().success(), "the tracer failed:\n{out}");
    let lines: Vec<&str> = out.lines().collect();
    let torn: Vec<&str> = lines.iter().copied().filter(|line| !whole(line)).collect();
    assert_eq!(
        lines.len(),
        2 * LINES,
        "every line arrives on its own; torn: {:?}",
        &torn[..torn.len().min(6)]
    );
    assert!(
        torn.is_empty(),
        "{} of {} lines torn, the first: {:?}",
        torn.len(),
        lines.len(),
        &torn[..torn.len().min(6)]
    );
}
