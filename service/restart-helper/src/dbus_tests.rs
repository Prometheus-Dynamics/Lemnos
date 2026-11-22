//! The client against the manager's own bytes. The replies and the signal in `testdata/` were
//! recorded from systemd 259's manager over its private socket (this machine's user manager):
//! `LoadUnit("default.target")`, `Get(Manager, "SystemState")`, `StartUnit` of a unit that does not
//! exist (the manager's error), and a `PropertiesChanged` signal it sends to every direct
//! connection. A replaying peer answers each call with the recorded reply, its reply serial set to
//! the call's, after a recorded signal. The live calls run against PID 1 in the systemd guest
//! (`testing/qemu/run-systemd-restart.sh`); the user manager here is too busy to answer in bounded
//! time, which is why the replies are recorded rather than asked for in this test.

use super::*;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;

const LOAD_UNIT: &[u8] = include_bytes!("../testdata/load-unit.reply");
const SYSTEM_STATE: &[u8] = include_bytes!("../testdata/get-system-state.reply");
const NO_SUCH_UNIT: &[u8] = include_bytes!("../testdata/start-unit-no-such.reply");
const SIGNAL: &[u8] = include_bytes!("../testdata/signal.msg");

fn uid() -> u32 {
    std::fs::metadata("/proc/self").unwrap().uid()
}

/// `reply` with its reply serial (header field 5) set to `serial`.
fn answering(reply: &[u8], serial: u32) -> Vec<u8> {
    let mut out = reply.to_vec();
    let fields_end = 16 + u32::from_le_bytes(out[12..16].try_into().unwrap()) as usize;
    let mut at = 16;
    while at < fields_end {
        at = at.div_ceil(8) * 8;
        let code = out[at];
        let sig_len = out[at + 1] as usize;
        let sig = out[at + 2..at + 2 + sig_len].to_vec();
        at += 3 + sig_len;
        match sig.as_slice() {
            b"u" => {
                at = at.div_ceil(4) * 4;
                if code == 5 {
                    out[at..at + 4].copy_from_slice(&serial.to_le_bytes());
                    return out;
                }
                at += 4;
            }
            b"s" | b"o" => {
                at = at.div_ceil(4) * 4;
                let n = u32::from_le_bytes(out[at..at + 4].try_into().unwrap()) as usize;
                at += 4 + n + 1;
            }
            b"g" => at += 1 + out[at] as usize + 1,
            other => panic!("recorded field of type {other:?}"),
        }
    }
    panic!("the recorded reply has no reply serial");
}

/// A peer that takes the client's handshake as the manager does and answers each call with the
/// next of `replies` (after one signal), checking each call's member. Returns the calls' members.
fn replay(replies: Vec<(&'static str, &'static [u8])>) -> (Manager, std::thread::JoinHandle<()>) {
    let (client, server) = UnixStream::pair().unwrap();
    let peer = std::thread::spawn(move || {
        let mut r = BufReader::new(server.try_clone().unwrap());
        let mut w = server;
        let mut nul = [0u8; 1];
        r.read_exact(&mut nul).unwrap();
        assert_eq!(nul, [0]);
        let mut line = String::new();
        r.read_line(&mut line).unwrap();
        assert!(line.starts_with("AUTH EXTERNAL "), "{line:?}");
        w.write_all(b"OK 0123456789abcdef0123456789abcdef\r\n")
            .unwrap();
        line.clear();
        r.read_line(&mut line).unwrap();
        assert_eq!(line, "NEGOTIATE_UNIX_FD\r\n");
        w.write_all(b"AGREE_UNIX_FD\r\n").unwrap();
        line.clear();
        r.read_line(&mut line).unwrap();
        assert_eq!(line, "BEGIN\r\n");
        // The client's handshake pings the manager after BEGIN and waits for its answer before any call.
        let ping = read_message(&mut r).unwrap();
        assert_eq!(ping.kind, METHOD_CALL);
        assert_eq!(ping.member.as_deref(), Some("Ping"));
        w.write_all(&ping_answer(ping.serial)).unwrap();
        for (member, reply) in replies {
            let call = read_message(&mut r).unwrap();
            assert_eq!(call.kind, METHOD_CALL);
            let mut fixed = [0u8; 4];
            fixed.copy_from_slice(&call_serial_bytes(&call));
            let serial = u32::from_le_bytes(fixed);
            assert!(call.member.as_deref() == Some(member), "{:?}", call.member);
            assert_eq!(
                call.destination.as_deref(),
                Some("org.freedesktop.systemd1")
            );
            w.write_all(SIGNAL).unwrap();
            w.write_all(&answering(reply, serial)).unwrap();
        }
    });
    let m = Manager::over(client, uid(), Path::new("replay")).unwrap();
    (m, peer)
}

/// The manager's answer to a Ping with reply serial `serial`: an empty METHOD_RETURN.
fn ping_answer(serial: u32) -> Vec<u8> {
    let mut out = vec![b'l', METHOD_RETURN, 0, 1];
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&100u32.to_le_bytes());
    // One REPLY_SERIAL field: struct (code 5, signature "u", pad, value).
    out.extend_from_slice(&8u32.to_le_bytes());
    out.extend_from_slice(&[5, 1, b'u', 0]);
    out.extend_from_slice(&serial.to_le_bytes());
    out
}

fn call_serial_bytes(m: &Message) -> [u8; 4] {
    m.serial.to_le_bytes()
}

#[test]
fn the_managers_recorded_replies_are_read_as_the_answers_to_each_call_past_its_signals() {
    let (mut m, peer) = replay(vec![("LoadUnit", LOAD_UNIT), ("Get", SYSTEM_STATE)]);
    assert_eq!(
        m.load_unit("default.target").unwrap(),
        "/org/freedesktop/systemd1/unit/default_2etarget"
    );
    assert_eq!(m.system_state().unwrap(), "degraded");
    peer.join().unwrap();
}

#[test]
fn the_managers_recorded_error_is_its_refusal_by_name() {
    let (mut m, peer) = replay(vec![("StartUnit", NO_SUCH_UNIT)]);
    let e = m
        .start_unit("lemnos-restart-test-no-such-unit.target", "replace")
        .unwrap_err();
    assert_eq!(
        e,
        CallError::Refused {
            name: "org.freedesktop.systemd1.NoSuchUnit".into(),
            text: "Unit lemnos-restart-test-no-such-unit.target not found.".into()
        }
    );
    peer.join().unwrap();
}

#[test]
fn a_manager_that_only_sends_signals_is_given_up_on_at_the_answer_deadline() {
    let (client, server) = UnixStream::pair().unwrap();
    let peer = std::thread::spawn(move || {
        let mut r = BufReader::new(server.try_clone().unwrap());
        let mut w = server;
        let mut line = Vec::new();
        for answer in [&b"OK 00\r\n"[..], b"AGREE_UNIX_FD\r\n"] {
            line.clear();
            r.read_until(b'\n', &mut line).unwrap();
            w.write_all(answer).unwrap();
        }
        // Signals, never the answer to a Ping, until the client hangs up.
        while w.write_all(SIGNAL).is_ok() {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    });
    let start = std::time::Instant::now();
    let e = Manager::over(client, uid(), Path::new("replay"))
        .err()
        .expect("no ping answer");
    assert_eq!(
        e,
        CallError::Link("the manager did not answer a ping within 5000 ms".into())
    );
    assert!(start.elapsed() < ANSWER_WAIT + std::time::Duration::from_secs(2));
    peer.join().unwrap();
}

/// The handshake's Pings are retried, but the call is sent once: a manager that ignores the first two Pings answers the
/// third, then answers the one LoadUnit. Counts the Pings and the calls it reads.
#[test]
fn a_ping_is_retried_and_the_call_is_sent_exactly_once() {
    let (client, server) = UnixStream::pair().unwrap();
    let peer = std::thread::spawn(move || {
        let mut r = BufReader::new(server.try_clone().unwrap());
        let mut w = server;
        let mut nul = [0u8; 1];
        r.read_exact(&mut nul).unwrap();
        let mut line = String::new();
        r.read_line(&mut line).unwrap();
        w.write_all(b"OK 0123456789abcdef0123456789abcdef\r\n")
            .unwrap();
        line.clear();
        r.read_line(&mut line).unwrap();
        w.write_all(b"AGREE_UNIX_FD\r\n").unwrap();
        line.clear();
        r.read_line(&mut line).unwrap();
        assert_eq!(line, "BEGIN\r\n");
        let (mut pings, mut calls) = (0, 0);
        loop {
            let msg = read_message(&mut r).unwrap();
            if msg.member.as_deref() == Some("Ping") {
                pings += 1;
                if pings >= 3 {
                    w.write_all(&ping_answer(msg.serial)).unwrap();
                }
            } else {
                assert_eq!(msg.member.as_deref(), Some("LoadUnit"));
                calls += 1;
                w.write_all(&answering(LOAD_UNIT, msg.serial)).unwrap();
                break;
            }
        }
        assert_eq!(pings, 3, "pings read before the answered one");
        assert_eq!(calls, 1);
    });
    let mut m = Manager::over(client, uid(), Path::new("replay")).unwrap();
    assert_eq!(
        m.load_unit("default.target").unwrap(),
        "/org/freedesktop/systemd1/unit/default_2etarget"
    );
    peer.join().unwrap();
}

/// The twin of the test above for the call that restarts: the handshake's Pings are retried, and `StartUnit` is sent
/// once. A manager that ignores the first two Pings answers the third, reads the one `StartUnit`, answers it late (with
/// its recorded refusal, so nothing is started), and then reads on until the client hangs up: a second `StartUnit` at any
/// point, before the answer or after it, is counted and fails the test.
#[test]
fn a_ping_is_retried_and_start_unit_is_sent_exactly_once() {
    let (client, server) = UnixStream::pair().unwrap();
    let peer = std::thread::spawn(move || {
        let mut r = BufReader::new(server.try_clone().unwrap());
        let mut w = server;
        let mut nul = [0u8; 1];
        r.read_exact(&mut nul).unwrap();
        let mut line = String::new();
        r.read_line(&mut line).unwrap();
        w.write_all(b"OK 0123456789abcdef0123456789abcdef\r\n")
            .unwrap();
        line.clear();
        r.read_line(&mut line).unwrap();
        w.write_all(b"AGREE_UNIX_FD\r\n").unwrap();
        line.clear();
        r.read_line(&mut line).unwrap();
        assert_eq!(line, "BEGIN\r\n");
        let (mut pings, mut starts, mut answered) = (0, 0, false);
        // Every message until the client hangs up.
        while let Ok(msg) = read_message(&mut r) {
            match msg.member.as_deref() {
                Some("Ping") => {
                    pings += 1;
                    if pings >= 3 {
                        w.write_all(&ping_answer(msg.serial)).unwrap();
                    }
                }
                Some("StartUnit") => {
                    starts += 1;
                    if !answered {
                        // Late, but inside the call's wait.
                        std::thread::sleep(std::time::Duration::from_millis(300));
                        w.write_all(&answering(NO_SUCH_UNIT, msg.serial)).unwrap();
                        answered = true;
                    }
                }
                other => panic!("unexpected call {other:?}"),
            }
        }
        (pings, starts)
    });
    let mut m = Manager::over(client, uid(), Path::new("replay")).unwrap();
    let e = m
        .start_unit("lemnos-restart-test-no-such-unit.target", "replace")
        .unwrap_err();
    assert!(
        matches!(&e, CallError::Refused { name, .. } if name == "org.freedesktop.systemd1.NoSuchUnit"),
        "{e:?}"
    );
    drop(m);
    let (pings, starts) = peer.join().unwrap();
    assert_eq!(pings, 3, "pings read before the answered one");
    assert_eq!(starts, 1, "StartUnit is sent exactly once");
}

/// A socket that is not there is named. (The handshake against a real systemd is proven in the
/// QEMU systemd guest, testing/qemu/run-systemd-restart.sh: no test touches this PC's managers.)
#[test]
fn a_missing_socket_is_named() {
    let gone = PathBuf::from("/run/lemnos-restart-test-no-such-socket");
    let Err(CallError::Link(why)) = Manager::connect(&gone, uid()) else {
        panic!("a missing socket must fail");
    };
    assert!(why.starts_with(gone.to_str().unwrap()), "{why}");
}

#[test]
fn a_call_is_the_bytes_the_protocol_lays_out() {
    let m = encode_call(
        7,
        "/org/freedesktop/systemd1",
        "org.freedesktop.systemd1.Manager",
        "LoadUnit",
        "s",
        &[Arg::Str("a")],
    );
    // Fixed header: little endian, a method call, no flags, version 1, body 6 bytes, serial 7.
    assert_eq!(&m[..12], &[b'l', 1, 0, 1, 6, 0, 0, 0, 7, 0, 0, 0]);
    let fields = u32::from_le_bytes(m[12..16].try_into().unwrap()) as usize;
    let body_at = (16 + fields).div_ceil(8) * 8;
    assert_eq!(&m[body_at..], &[1, 0, 0, 0, b'a', 0]);
    // The path field first: code 1, signature "o", the path's length.
    assert_eq!(&m[16..24], &[1, 1, b'o', 0, 25, 0, 0, 0]);
}
