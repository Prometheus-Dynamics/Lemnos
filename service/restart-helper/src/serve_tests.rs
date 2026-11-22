use super::*;
use std::io::{BufRead, Cursor, Read};

/// One connection: the caller's bytes in, the helper's answers out.
struct Conn {
    input: Cursor<Vec<u8>>,
    output: Vec<u8>,
}

impl Conn {
    fn new(request: &[u8]) -> Self {
        Self {
            input: Cursor::new(request.to_vec()),
            output: Vec::new(),
        }
    }
    fn answers(&self) -> String {
        String::from_utf8(self.output.clone()).unwrap()
    }
}

impl Read for Conn {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.input.read(buf)
    }
}
impl BufRead for Conn {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        self.input.fill_buf()
    }
    fn consume(&mut self, n: usize) {
        self.input.consume(n)
    }
}
impl Write for Conn {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.output.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A stand-in manager: each step's answer, and how many times each was asked.
#[derive(Default)]
struct Stand {
    check: Option<String>,
    start: Option<String>,
    states: Vec<&'static str>,
    calls: Vec<&'static str>,
}

impl Pid1 for Stand {
    fn check(&mut self) -> Result<(), String> {
        self.calls.push("check");
        self.check.clone().map_or(Ok(()), Err)
    }
    fn start_reboot(&mut self) -> Result<String, String> {
        self.calls.push("start");
        self.start
            .clone()
            .map_or(Ok("/org/freedesktop/systemd1/job/1".into()), Err)
    }
    fn system_state(&mut self) -> Result<String, String> {
        self.calls.push("state");
        Ok(if self.states.is_empty() {
            "running"
        } else {
            self.states.remove(0)
        }
        .to_string())
    }
}

const POLICY: Policy = Policy { allowed_uid: 900 };
const QUICK: Watch = Watch {
    bound: Duration::from_millis(50),
    every: Duration::from_millis(5),
};

fn run(request: &[u8], uid: u32, pid1: &mut dyn Pid1) -> String {
    let mut conn = Conn::new(request);
    serve(&mut conn, uid, POLICY, pid1, QUICK, &mut || false).unwrap();
    conn.answers()
}

#[test]
fn only_root_and_the_daemons_user_are_answered_and_a_group_member_is_refused_by_its_uid() {
    for uid in [0, 900] {
        let mut stand = Stand::default();
        assert_eq!(run(b"check 1a\n", uid, &mut stand), "ok 1a\n");
    }
    // A process in the socket's group that is neither: it got as far as the connect, no further.
    let mut stand = Stand::default();
    assert_eq!(
        run(b"reboot 1a\n", 998, &mut stand),
        "refused 1a uid 998 may not restart this board: only uid 900 (the daemon's user) and root may\n"
    );
    assert!(stand.calls.is_empty(), "{:?}", stand.calls);
}

#[test]
fn anything_but_a_verb_and_a_nonce_is_refused_before_the_manager_is_asked() {
    let cases: [(&[u8], &str); 7] = [
        (b"reboot\n", "a request is exactly"),
        (b"reboot 1a now\n", "a request is exactly"),
        (b"halt 1a\n", "the verb is check or reboot"),
        (b"reboot 1A\n", "lower-case hex"),
        (b"reboot 1a", "one line of at most 48 bytes"),
        (
            b"reboot 0123456789abcdef0123456789abcdef0123456789abcdef\n",
            "one line of at most 48 bytes",
        ),
        (b"reboot \xff\n", "not text"),
    ];
    for (request, why) in cases {
        let mut stand = Stand::default();
        let answer = run(request, 900, &mut stand);
        assert!(answer.starts_with("refused - "), "{answer}");
        assert!(answer.contains(why), "{request:?}: {answer}");
        assert!(stand.calls.is_empty());
    }
}

#[test]
fn a_check_answers_ok_or_the_managers_reason_and_starts_nothing() {
    let mut stand = Stand::default();
    assert_eq!(run(b"check 7\n", 900, &mut stand), "ok 7\n");
    assert_eq!(stand.calls, ["check"]);
    let mut stand = Stand {
        check: Some("the service manager has reboot.target not-found".into()),
        ..Stand::default()
    };
    assert_eq!(
        run(b"check 7\n", 900, &mut stand),
        "refused 7 the service manager has reboot.target not-found\n"
    );
    assert_eq!(stand.calls, ["check"]);
}

#[test]
fn a_reboot_is_accepted_then_says_stopping_once_the_manager_is_stopping() {
    let mut stand = Stand {
        states: vec!["running", "stopping"],
        ..Stand::default()
    };
    assert_eq!(
        run(b"reboot b2\n", 900, &mut stand),
        "accepted b2\nstopping b2\n"
    );
    assert_eq!(stand.calls, ["check", "start", "state", "state"]);
}

#[test]
fn a_reboot_the_manager_refuses_to_start_is_failed_on_the_same_connection() {
    let mut stand = Stand {
        start: Some(
            "the service manager: org.freedesktop.systemd1.TransactionIsDestructive: no".into(),
        ),
        ..Stand::default()
    };
    assert_eq!(
        run(b"reboot b2\n", 900, &mut stand),
        "accepted b2\nfailed b2 the service manager: org.freedesktop.systemd1.TransactionIsDestructive: no\n"
    );
    // A reboot whose check fails is refused before it is accepted, and nothing is started.
    let mut stand = Stand {
        check: Some("the service manager: gone".into()),
        ..Stand::default()
    };
    assert_eq!(
        run(b"reboot b2\n", 900, &mut stand),
        "refused b2 the service manager: gone\n"
    );
    assert_eq!(stand.calls, ["check"]);
}

#[test]
fn a_started_reboot_whose_system_never_stops_is_failed_at_the_bound_with_the_last_state() {
    let mut stand = Stand::default();
    let answer = run(b"reboot c3\n", 900, &mut stand);
    assert!(answer.starts_with("accepted c3\nfailed c3 reboot.target was started and the system did not start stopping within 50 ms: the system is running\n"), "{answer}");
}

#[test]
fn the_helpers_own_stop_after_a_started_reboot_is_the_system_stopping() {
    let mut stand = Stand::default();
    let mut conn = Conn::new(b"reboot d4\n");
    serve(&mut conn, 0, POLICY, &mut stand, QUICK, &mut || true).unwrap();
    assert_eq!(conn.answers(), "accepted d4\nstopping d4\n");
}

/// A missing manager socket fails the way the helper would see it, named. (A real systemd is proven
/// in the QEMU systemd guest: no test touches this PC's managers.)
#[test]
fn a_missing_manager_link_is_named() {
    let mut gone = Systemd {
        socket: "/run/lemnos-restart-test-no-such-socket".into(),
    };
    let why = gone.check().unwrap_err();
    assert!(
        why.starts_with("the service manager: /run/lemnos-restart-test-no-such-socket: "),
        "{why}"
    );
}

#[test]
fn every_answer_reads_back_as_itself_and_a_line_for_another_request_is_named() {
    use crate::proto::{answer_line, parse_answer};
    for a in [
        Answer::Ok,
        Answer::Accepted,
        Answer::Stopping,
        Answer::Refused("no\nmore".into()),
        Answer::Failed("x".into()),
    ] {
        let line = answer_line("ab", &a);
        let back = parse_answer(line.trim_end_matches('\n'), "ab").unwrap();
        match a {
            Answer::Refused(_) => assert_eq!(back, Answer::Refused("no more".into())),
            other => assert_eq!(back, other),
        }
    }
    assert_eq!(
        parse_answer("refused - why", "ab").unwrap(),
        Answer::Refused("why".into())
    );
    assert!(
        parse_answer("ok cd", "ab")
            .unwrap_err()
            .contains("not to this request")
    );
    assert!(parse_answer("ok ab extra", "ab").is_err());
    assert!(parse_answer("maybe ab", "ab").is_err());
}
