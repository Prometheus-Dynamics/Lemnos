use super::*;
use ed25519_dalek::{Signer, SigningKey};
use lemnos_slot_store::signing::{Domain, record_bytes, security_switch_message, signed_message};
use std::sync::atomic::{AtomicU32, Ordering};

/// The test key: seed `0x07` repeated, the same key the signing vectors use.
fn test_key() -> SigningKey {
    SigningKey::from_bytes(&[7u8; 32])
}

fn public_hex() -> String {
    test_key()
        .verifying_key()
        .to_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The 108-byte record for `message`, signed by `key`.
fn record_over(key: &SigningKey, message: &[u8]) -> Vec<u8> {
    let key_id = signing::key_id(key.verifying_key().as_bytes());
    let signature = key.sign(message).to_bytes();
    record_bytes(&key_id, &signature)
}

/// The authorization that turns the switch off at `generation` on `serial`.
fn off_auth(key: &SigningKey, serial: &str, generation: u64) -> Vec<u8> {
    record_over(
        key,
        &security_switch_message(serial.as_bytes(), false, generation),
    )
}

/// A key file holding the test key, owned by `owner`.
fn keys_file(dir: &Dir) -> PathBuf {
    let path = dir.0.join("keys");
    fs::write(&path, format!("{} board owner\n", public_hex())).unwrap();
    path
}

const SERIAL: &str = "0123456789abcdef";

struct Dir(PathBuf);

impl Dir {
    fn new(label: &str) -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "lemnos-security-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn ok(from: &'static str) -> Answer {
    Answer {
        from,
        lines: Vec::new(),
    }
}

fn missing(from: &'static str, text: &str) -> Answer {
    Answer {
        from,
        lines: vec![format!("missing: {text}")],
    }
}

fn busy(from: &'static str, text: &str) -> Answer {
    Answer {
        from,
        lines: vec![format!("busy: {text}")],
    }
}

#[test]
fn off_is_always_allowed_unless_work_is_running() {
    assert!(
        judge(
            false,
            false,
            &[ok("slots"), missing("flash", "no trust root")]
        )
        .is_ok()
    );
    assert!(
        judge(false, true, &[]).is_ok(),
        "off has no Orion precondition"
    );
    let (status, text) =
        judge(false, false, &[busy("slots", "an install is running")]).unwrap_err();
    assert_eq!(status, Status::BUSY);
    assert!(
        text.contains("slots: busy: an install is running"),
        "{text}"
    );
}

#[test]
fn on_names_each_missing_fact_and_busy_refuses_either_way() {
    let (status, text) = judge(
        true,
        false,
        &[
            missing(
                "slots",
                "default slot 'a' is not verified: install a bundle a listed key signed",
            ),
            ok("flash"),
        ],
    )
    .unwrap_err();
    assert_eq!(status, Status::NOT_PERMITTED);
    assert_eq!(
        text,
        "slots: missing: default slot 'a' is not verified: install a bundle a listed key signed"
    );
    let (status, _) = judge(true, false, &[busy("flash", "a write is running")]).unwrap_err();
    assert_eq!(status, Status::BUSY);
    let (status, _) = judge(true, false, &[busy("slots", "x"), missing("flash", "y")]).unwrap_err();
    assert_eq!(status, Status::BUSY, "busy is judged before missing");
}

#[test]
fn on_with_orion_on_the_board_is_refused_by_name_until_orion_can_be_asked() {
    let (status, text) = judge(true, true, &[ok("slots")]).unwrap_err();
    assert_eq!(status, Status::NOT_PERMITTED);
    assert!(
        text.contains("Orion cannot be asked at this revision"),
        "{text}"
    );
    assert!(
        judge(true, false, &[ok("slots")]).is_ok(),
        "no Orion, no Orion answer"
    );
}

#[test]
fn on_with_everything_in_place_is_allowed() {
    assert!(judge(true, false, &[ok("slots"), ok("flash")]).is_ok());
}

#[test]
fn a_change_writes_the_record_then_renders_the_view_at_once() {
    let dir = Dir::new("set");
    let mut flow = Flow::open(dir.0.join("security.rec"), &dir.0, false);
    assert_eq!(
        flow.switch(),
        Switch {
            on: false,
            generation: 0
        }
    );
    flow.set_plain(true, &[ok("slots")]).unwrap();
    assert_eq!(
        flow.switch(),
        Switch {
            on: true,
            generation: 1
        }
    );
    assert_eq!(
        fs::read_to_string(dir.0.join("security")).unwrap(),
        "on\ngeneration 1\n"
    );
    assert!(!flow.view_pending());
    assert!(
        flow.state_lines(Board::default())
            .contains(&"switch on".to_string())
    );
    flow.set_plain(false, &[]).unwrap();
    assert_eq!(
        fs::read_to_string(dir.0.join("security")).unwrap(),
        "off\ngeneration 2\n",
        "applied with no restart: the same flow changes the view again"
    );
}

#[test]
fn a_refused_change_writes_nothing_and_says_why() {
    let dir = Dir::new("refused");
    let record = dir.0.join("security.rec");
    let mut flow = Flow::open(&record, &dir.0, false);
    flow.set_plain(true, &[ok("slots")]).unwrap();
    let before = fs::read(&record).unwrap();
    let refusal = flow
        .set_plain(false, &[busy("slots", "a trial is armed")])
        .unwrap_err();
    assert_eq!(refusal.0, Status::BUSY);
    assert_eq!(fs::read(&record).unwrap(), before, "nothing written");
    assert_eq!(
        flow.switch(),
        Switch {
            on: true,
            generation: 1
        }
    );
    assert!(
        flow.state_lines(Board::default())
            .iter()
            .any(|line| line.contains("trial is armed"))
    );
}

#[test]
fn a_record_that_cannot_be_written_refuses_and_leaves_the_view_alone() {
    let dir = Dir::new("io");
    let record = dir.0.join("security.rec");
    fs::create_dir(&record).unwrap(); // a directory where the record goes: the write fails
    // Unreadable reads as on (corrupt); a write that cannot land is refused and changes nothing.
    let mut flow = Flow::open(&record, &dir.0, false);
    assert_eq!(
        flow.switch(),
        Switch {
            on: true,
            generation: 0
        }
    );
    let refusal = flow.set_plain(false, &[ok("slots")]).unwrap_err();
    assert_eq!(refusal.0, Status::IO);
    assert_eq!(
        flow.switch(),
        Switch {
            on: true,
            generation: 0
        }
    );
}

#[test]
fn a_failed_render_leaves_the_view_pending_and_the_retry_brings_it_current() {
    let dir = Dir::new("pending");
    let render = dir.0.join("run");
    let mut flow = Flow::open(dir.0.join("security.rec"), &render, false);
    assert!(flow.view_pending(), "no render directory yet");
    flow.set_plain(true, &[ok("slots")]).unwrap();
    assert!(
        flow.view_pending(),
        "the record is written; the view is not"
    );
    assert!(
        flow.state_lines(Board::default())
            .contains(&"view pending".to_string())
    );
    fs::create_dir(&render).unwrap();
    flow.retry_render();
    assert!(!flow.view_pending());
    assert_eq!(
        fs::read_to_string(render.join("security")).unwrap(),
        "on\ngeneration 1\n"
    );
}

#[test]
fn open_reads_absent_as_off_and_corrupt_as_on_reported() {
    let dir = Dir::new("open");
    let record = dir.0.join("security.rec");
    let flow = Flow::open(&record, &dir.0, false);
    assert_eq!(
        flow.switch(),
        Switch {
            on: false,
            generation: 0
        }
    );
    assert_eq!(
        fs::read_to_string(dir.0.join("security")).unwrap(),
        "off\ngeneration 0\n"
    );

    fs::write(&record, b"garbage that is not a record").unwrap();
    let flow = Flow::open(&record, &dir.0, false);
    assert!(flow.switch().on, "corrupt reads on");
    assert!(
        flow.state_lines(Board::default())
            .iter()
            .any(|line| line.starts_with("record corrupt"))
    );
}

#[test]
fn a_record_written_before_restart_is_the_truth_at_the_next_open() {
    let dir = Dir::new("restart");
    let record = dir.0.join("security.rec");
    {
        let mut flow = Flow::open(&record, &dir.0, false);
        flow.set_plain(true, &[ok("slots")]).unwrap();
    }
    fs::remove_file(dir.0.join("security")).unwrap(); // the view was lost with the tmpfs
    let flow = Flow::open(&record, &dir.0, false);
    assert_eq!(
        flow.switch(),
        Switch {
            on: true,
            generation: 1
        }
    );
    assert_eq!(
        fs::read_to_string(dir.0.join("security")).unwrap(),
        "on\ngeneration 1\n"
    );
}

#[test]
fn the_trust_root_is_a_key_line_with_an_owner() {
    let dir = Dir::new("keys");
    let path = dir.0.join("keys");
    assert!(!trust_root(None));
    assert!(!trust_root(Some(&path)), "no file");
    fs::write(&path, format!("# only a comment\n{}\n", public_hex())).unwrap();
    assert!(
        !trust_root(Some(&path)),
        "a key without an owner is no trust root"
    );
    fs::write(&path, format!("{} the board owner\n", public_hex())).unwrap();
    assert!(trust_root(Some(&path)));
    fs::write(&path, format!("{} the board owner\n", "zz".repeat(32))).unwrap();
    assert!(!trust_root(Some(&path)), "not hex is no key");
}

#[test]
fn state_says_what_the_readers_read_when_the_view_is_malformed() {
    let dir = Dir::new("view-corrupt");
    let record = dir.0.join("record");
    // A valid record that says off, and a view someone tore by hand.
    let flow = Flow::open(&record, &dir.0, false);
    assert!(
        flow.state_lines(Board::default())
            .iter()
            .any(|l| l == "view off generation 0: readers read off")
    );
    fs::write(
        dir.0.join(security::FILE_NAME),
        b"on\ngeneration 0\ngarbage\n",
    )
    .unwrap();
    let lines = flow.state_lines(Board::default());
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("view corrupt: readers read on")),
        "{lines:?}"
    );
    // The record still says off: the state names both, so nobody reads "off" as the whole truth.
    assert!(lines.iter().any(|l| l == "switch off"), "{lines:?}");
    // Absent view: the readers read off, and the state says so (without a reopen, which would render it).
    fs::remove_file(dir.0.join(security::FILE_NAME)).unwrap();
    assert!(
        flow.state_lines(Board::default())
            .iter()
            .any(|l| l == "view absent: readers read off"),
        "{:?}",
        flow.state_lines(Board::default())
    );
}

impl Flow {
    /// A change with no boot side: the record and the view alone.
    fn set_plain(&mut self, on: bool, answers: &[Answer]) -> Result<(), Refusal> {
        self.set(on, answers, Board::default(), &[], &mut |_| Ok(()))
    }
}

/// The boot side's mark as slot-init reads it: a file that exists when the switch is on.
struct Mark(PathBuf);

impl Mark {
    fn new(dir: &Path) -> Self {
        Self(dir.join("boot-mark"))
    }
    fn on(&self) -> bool {
        self.0.exists()
    }
    /// The boot side's callback: write the mark for on, or clear it for off (durably, as the slots do).
    fn apply(&self) -> impl FnMut(bool) -> Result<(), String> + '_ {
        move |on| {
            if on {
                fs::write(&self.0, b"yes\n").map_err(|e| e.to_string())
            } else {
                match fs::remove_file(&self.0) {
                    Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
                    _ => Ok(()),
                }
            }
        }
    }
}

/// The record's value as the switch reads it; `None` when there is no record.
fn record_on(record: &Path) -> bool {
    PlainFile::new(record)
        .read()
        .ok()
        .flatten()
        .is_some_and(|switch| switch.on)
}

/// The invariant at every crash point: the boot side's mark is never less strict than the record.
fn assert_never_weaker(mark: &Mark, record: &Path, at: &str) {
    assert!(
        !record_on(record) || mark.on(),
        "{at}: the record says on and the boot mark is off"
    );
}

/// After a crash, the start-up repair makes the mark and the view match the record.
fn restart_repairs(dir: &Dir, record: &Path, mark: &Mark, at: &str) {
    let mut flow = Flow::open(record, &dir.0, false);
    flow.repair(&mut mark.apply()).unwrap();
    assert_eq!(mark.on(), flow.switch().on, "{at}: the repaired mark");
    let view = security::read_at(&dir.0.join(security::FILE_NAME));
    assert_eq!(
        view,
        security::View::Set(flow.switch()),
        "{at}: the repaired view"
    );
}

#[test]
fn a_crash_at_each_step_of_a_change_to_on_never_leaves_the_mark_weaker_than_the_record() {
    for fault in [Step::Boot, Step::Record, Step::View] {
        let at = format!("crash before {fault:?}");
        let dir = Dir::new("crash-on");
        let record = dir.0.join("record");
        let mark = Mark::new(&dir.0);
        let mut flow = Flow::open(&record, &dir.0, false);
        flow.set_plain(false, &[]).unwrap(); // a record that says off
        flow.fault = Some(fault);
        assert!(
            flow.set(
                true,
                &[ok("slots")],
                Board::default(),
                &[],
                &mut mark.apply()
            )
            .is_err(),
            "{at}"
        );
        assert_never_weaker(&mark, &record, &at);
        restart_repairs(&dir, &record, &mark, &at);
    }
}

#[test]
fn a_crash_at_each_step_of_a_change_to_off_never_leaves_the_mark_weaker_than_the_record() {
    for fault in [Step::Record, Step::View, Step::Boot] {
        let at = format!("crash before {fault:?}");
        let dir = Dir::new("crash-off");
        let record = dir.0.join("record");
        let mark = Mark::new(&dir.0);
        let mut flow = Flow::open(&record, &dir.0, false);
        flow.set(
            true,
            &[ok("slots")],
            Board::default(),
            &[],
            &mut mark.apply(),
        )
        .unwrap(); // on, marked
        assert!(mark.on() && record_on(&record), "{at}: the start state");
        flow.fault = Some(fault);
        assert!(
            flow.set(false, &[], Board::default(), &[], &mut mark.apply())
                .is_err(),
            "{at}"
        );
        assert_never_weaker(&mark, &record, &at);
        restart_repairs(&dir, &record, &mark, &at);
    }
}

#[test]
fn a_change_with_every_step_done_leaves_the_mark_the_record_and_the_view_agreeing() {
    let dir = Dir::new("whole");
    let record = dir.0.join("record");
    let mark = Mark::new(&dir.0);
    let mut flow = Flow::open(&record, &dir.0, false);
    flow.set(
        true,
        &[ok("slots")],
        Board::default(),
        &[],
        &mut mark.apply(),
    )
    .unwrap();
    assert!(mark.on() && record_on(&record));
    assert_eq!(
        security::read_at(&dir.0.join(security::FILE_NAME)),
        security::View::Set(flow.switch())
    );
    flow.set(false, &[], Board::default(), &[], &mut mark.apply())
        .unwrap();
    assert!(!mark.on() && !record_on(&record));
    assert_eq!(
        security::read_at(&dir.0.join(security::FILE_NAME)),
        security::View::Set(flow.switch())
    );
}

#[test]
fn a_mark_that_cannot_be_written_stops_a_change_to_on_before_the_record() {
    let dir = Dir::new("mark-fails-on");
    let record = dir.0.join("record");
    let mut flow = Flow::open(&record, &dir.0, false);
    let refusal = flow
        .set(true, &[ok("slots")], Board::default(), &[], &mut |_| {
            Err("disk gone".to_string())
        })
        .unwrap_err();
    assert!(refusal.1.contains("boot mark: disk gone"), "{}", refusal.1);
    assert!(!record_on(&record), "the record is not written");
    assert!(!flow.switch().on);
}

#[test]
fn a_mark_that_cannot_be_cleared_is_reported_and_stays_stricter_than_the_record() {
    let dir = Dir::new("mark-fails-off");
    let record = dir.0.join("record");
    let mark = Mark::new(&dir.0);
    let mut flow = Flow::open(&record, &dir.0, false);
    flow.set(
        true,
        &[ok("slots")],
        Board::default(),
        &[],
        &mut mark.apply(),
    )
    .unwrap();
    let refusal = flow
        .set(false, &[], Board::default(), &[], &mut |_| {
            Err("read-only".to_string())
        })
        .unwrap_err();
    assert!(refusal.1.contains("stays strict"), "{}", refusal.1);
    // The record is off, the mark is still on: stricter, never weaker.
    assert!(!record_on(&record));
    assert!(mark.on());
}

/// Turns the switch on (no trust root needed for on), so the record is at generation `gen`, on.
fn on_at(dir: &Dir) -> PathBuf {
    let record = dir.0.join("security.rec");
    let mut flow = Flow::open(&record, &dir.0, false);
    flow.set_plain(true, &[ok("slots")]).unwrap();
    record
}

fn off_with(
    flow: &mut Flow,
    keys: &Path,
    authorization: &[u8],
) -> std::result::Result<(), Refusal> {
    flow.set(
        false,
        &[],
        Board {
            keys: Some(keys),
            serial: Some(SERIAL),
        },
        authorization,
        &mut |_| Ok(()),
    )
}

#[test]
fn off_with_no_trust_root_needs_no_authorization_and_ignores_one() {
    let dir = Dir::new("off-free");
    let record = on_at(&dir);
    let mut flow = Flow::open(&record, &dir.0, false);
    flow.set(
        false,
        &[],
        Board {
            keys: None,
            serial: Some(SERIAL),
        },
        &[],
        &mut |_| Ok(()),
    )
    .unwrap();
    assert!(!flow.switch().on);
    let mut again = Flow::open(dir.0.join("other.rec"), &dir.0, false);
    again.set_plain(true, &[ok("slots")]).unwrap();
    again
        .set(
            false,
            &[],
            Board {
                keys: None,
                serial: Some(SERIAL),
            },
            &[1, 2, 3],
            &mut |_| Ok(()),
        )
        .unwrap();
    assert!(
        again
            .state_lines(Board::default())
            .iter()
            .any(|l| l.contains("authorization ignored: this board holds no trust root")),
        "{:?}",
        again.state_lines(Board::default())
    );
}

#[test]
fn off_with_a_trust_root_and_no_authorization_is_refused_by_name() {
    let dir = Dir::new("off-none");
    let keys = keys_file(&dir);
    let mut flow = Flow::open(on_at(&dir), &dir.0, false);
    let (status, text) = off_with(&mut flow, &keys, &[]).unwrap_err();
    assert_eq!(status, Status::NOT_PERMITTED);
    assert!(text.contains("no authorization given"), "{text}");
    assert!(flow.switch().on, "nothing changed");
}

#[test]
fn a_valid_authorization_turns_it_off_and_moves_the_generation_once() {
    let dir = Dir::new("off-ok");
    let keys = keys_file(&dir);
    let mut flow = Flow::open(on_at(&dir), &dir.0, false);
    let auth = off_auth(&test_key(), SERIAL, 1);
    off_with(&mut flow, &keys, &auth).unwrap();
    assert_eq!(
        flow.switch(),
        Switch {
            on: false,
            generation: 2
        }
    );
    assert_eq!(
        fs::read_to_string(dir.0.join("security")).unwrap(),
        "off\ngeneration 2\n"
    );
}

#[test]
fn a_malformed_authorization_is_refused_by_name() {
    let dir = Dir::new("off-malformed");
    let keys = keys_file(&dir);
    let mut flow = Flow::open(on_at(&dir), &dir.0, false);
    let mut short = off_auth(&test_key(), SERIAL, 1);
    short.truncate(107);
    for bad in [short, b"not a record".to_vec()] {
        let (status, text) = off_with(&mut flow, &keys, &bad).unwrap_err();
        assert_eq!(status, Status::INVALID_ARGUMENT);
        assert!(text.starts_with("malformed authorization"), "{text}");
    }
    assert!(flow.switch().on);
}

#[test]
fn a_key_the_board_does_not_hold_is_named() {
    let dir = Dir::new("off-stranger");
    let keys = keys_file(&dir);
    let mut flow = Flow::open(on_at(&dir), &dir.0, false);
    let stranger = SigningKey::from_bytes(&[9u8; 32]);
    let auth = off_auth(&stranger, SERIAL, 1);
    let (status, text) = off_with(&mut flow, &keys, &auth).unwrap_err();
    assert_eq!(status, Status::NOT_PERMITTED);
    assert!(
        text.contains("signed by a key this board does not hold"),
        "{text}"
    );
}

#[test]
fn a_bad_signature_by_a_held_key_does_not_verify() {
    let dir = Dir::new("off-bad");
    let keys = keys_file(&dir);
    let mut flow = Flow::open(on_at(&dir), &dir.0, false);
    let mut auth = off_auth(&test_key(), SERIAL, 1);
    auth[60] ^= 0x01;
    let (_, text) = off_with(&mut flow, &keys, &auth).unwrap_err();
    assert!(text.contains("does not verify"), "{text}");
}

#[test]
fn an_authorization_made_for_another_board_does_not_verify_here() {
    let dir = Dir::new("off-board");
    let keys = keys_file(&dir);
    let mut flow = Flow::open(on_at(&dir), &dir.0, false);
    let auth = off_auth(&test_key(), "fedcba9876543210", 1);
    let (_, text) = off_with(&mut flow, &keys, &auth).unwrap_err();
    assert!(
        text.contains("made for another board, or it is corrupted"),
        "{text}"
    );
    assert!(flow.switch().on);
}

#[test]
fn an_authorization_made_for_on_is_named_as_another_state() {
    let dir = Dir::new("off-state");
    let keys = keys_file(&dir);
    let mut flow = Flow::open(on_at(&dir), &dir.0, false);
    let auth = record_over(
        &test_key(),
        &security_switch_message(SERIAL.as_bytes(), true, 1),
    );
    let (_, text) = off_with(&mut flow, &keys, &auth).unwrap_err();
    assert_eq!(
        text,
        "made for another state: it turns the switch on, not off"
    );
}

#[test]
fn an_authorization_is_spent_by_the_change_it_allowed() {
    let dir = Dir::new("off-replay");
    let keys = keys_file(&dir);
    let key = test_key();
    let record = on_at(&dir);
    let mut flow = Flow::open(&record, &dir.0, false);
    let first = off_auth(&key, SERIAL, 1);
    off_with(&mut flow, &keys, &first).unwrap(); // off at generation 2
    flow.set_plain(true, &[ok("slots")]).unwrap(); // on at generation 3
    let (_, text) = off_with(&mut flow, &keys, &first).unwrap_err();
    assert!(
        text.contains("made for generation 1, the switch is at 3"),
        "{text}"
    );
    assert!(flow.switch().on, "the replay changed nothing");
}

#[test]
fn an_authorization_for_a_future_generation_is_not_named_as_past() {
    let dir = Dir::new("off-future");
    let keys = keys_file(&dir);
    let mut flow = Flow::open(on_at(&dir), &dir.0, false);
    let auth = off_auth(&test_key(), SERIAL, 5);
    let (_, text) = off_with(&mut flow, &keys, &auth).unwrap_err();
    assert!(
        text.contains("made for another board, or it is corrupted"),
        "{text}"
    );
}

#[test]
fn a_slot_bundle_signature_over_the_same_bytes_is_not_an_authorization() {
    let dir = Dir::new("off-domain");
    let keys = keys_file(&dir);
    let key = test_key();
    // The body as `docs/slots.md` lays it out (not taken from the builder): version, serial, target
    // (off = 0), generation 1.
    let mut body = Vec::new();
    body.extend_from_slice(&1u32.to_le_bytes());
    body.extend_from_slice(&(SERIAL.len() as u32).to_le_bytes());
    body.extend_from_slice(SERIAL.as_bytes());
    body.push(0);
    body.extend_from_slice(&1u64.to_le_bytes());
    let whole = security_switch_message(SERIAL.as_bytes(), false, 1);
    assert_eq!(
        whole,
        signed_message(Domain::SecuritySwitch, &body),
        "the layout"
    );
    // A bundle-domain signature over the same body the switch signs.
    let bundle = record_over(&key, &signed_message(Domain::SlotBundle, &body));
    // A signature over the body with no domain at all (what a bare, pre-domain signer signs).
    let bare = record_over(&key, &body);
    for auth in [bundle, bare] {
        let mut flow = Flow::open(on_at(&dir), &dir.0, false);
        let (status, _) = off_with(&mut flow, &keys, &auth).unwrap_err();
        assert_eq!(status, Status::NOT_PERMITTED);
        assert!(flow.switch().on);
    }
}

#[test]
fn state_names_the_serial_the_trust_root_and_the_keys_held() {
    let dir = Dir::new("state");
    let keys = keys_file(&dir);
    let flow = Flow::open(on_at(&dir), &dir.0, false);
    let lines = flow.state_lines(Board {
        keys: Some(&keys),
        serial: Some(SERIAL),
    });
    assert!(lines.contains(&format!("serial {SERIAL}")), "{lines:?}");
    assert!(
        lines.contains(&"trust root present".to_string()),
        "{lines:?}"
    );
    let id: String = signing::key_id(test_key().verifying_key().as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert!(
        lines.contains(&format!("key {id} board owner")),
        "{lines:?}"
    );
    assert!(lines.contains(&"generation 1".to_string()), "{lines:?}");
    let none = flow.state_lines(Board::default());
    assert!(none.contains(&"serial absent".to_string()));
    assert!(none.contains(&"trust root absent".to_string()));
}

#[test]
fn request_off_names_the_message_an_authorization_signs() {
    let dir = Dir::new("request");
    let keys = keys_file(&dir);
    let record = on_at(&dir);
    let flow = Flow::open(&record, &dir.0, false);
    let lines = flow.request_off_lines(Board {
        keys: Some(&keys),
        serial: Some(SERIAL),
    });
    let message = security_switch_message(SERIAL.as_bytes(), false, 1);
    let hex: String = message.iter().map(|b| format!("{b:02x}")).collect();
    assert!(lines.contains(&format!("message {hex}")), "{lines:?}");
    assert!(lines.contains(&"generation 1".to_string()), "{lines:?}");
    // What the request says to sign is what the check verifies, at the generation it names.
    let auth = record_over(&test_key(), &message);
    let mut on_flow = Flow::open(&record, &dir.0, false);
    off_with(&mut on_flow, &keys, &auth).unwrap();
}

#[test]
fn request_off_with_no_trust_root_says_off_needs_none() {
    let dir = Dir::new("request-none");
    let flow = Flow::open(on_at(&dir), &dir.0, false);
    assert_eq!(
        flow.request_off_lines(Board {
            keys: None,
            serial: Some(SERIAL)
        }),
        vec!["no trust root: off needs no authorization".to_string()]
    );
}

#[test]
fn with_no_serial_a_trust_root_cannot_be_named_so_off_is_refused() {
    let dir = Dir::new("no-serial");
    let keys = keys_file(&dir);
    let mut flow = Flow::open(on_at(&dir), &dir.0, false);
    let auth = off_auth(&test_key(), SERIAL, 1);
    let (_, text) = flow
        .set(
            false,
            &[],
            Board {
                keys: Some(&keys),
                serial: None,
            },
            &auth,
            &mut |_| Ok(()),
        )
        .unwrap_err();
    assert!(text.contains("no serial"), "{text}");
    assert!(flow.switch().on);
}

#[test]
fn an_authorization_given_with_on_is_ignored_and_says_so() {
    let dir = Dir::new("on-ignored");
    let mut flow = Flow::open(dir.0.join("security.rec"), &dir.0, false);
    flow.set(
        true,
        &[ok("slots")],
        Board::default(),
        &[9, 9, 9],
        &mut |_| Ok(()),
    )
    .unwrap();
    assert!(flow.switch().on);
    assert!(
        flow.state_lines(Board::default())
            .iter()
            .any(|l| l.contains("authorization ignored: turning the switch on needs none")),
    );
}

/// A power cut at every byte of the OFF's record write. What a reader finds after the cut is either
/// the old record (on, generation 1), and then the same authorization still turns it off to
/// generation 2, or the new one (off, generation 2), and then the same authorization is spent. Never
/// anything else, and never a different end state.
#[test]
fn a_cut_at_every_byte_of_the_off_leaves_the_same_end_state_or_a_spent_authorization() {
    let key = test_key();
    let keys_dir = Dir::new("cut-keys");
    let keys = keys_file(&keys_dir);
    let auth = off_auth(&key, SERIAL, 1);
    for cut in 0..=512 {
        let dir = Dir::new("cut");
        let record = on_at(&dir);
        let plain = PlainFile::new(&record);
        let _ = plain.write_cut(false, cut); // the power cut mid-record
        let mut flow = Flow::open(&record, &dir.0, false);
        let after = flow.switch();
        if after.on {
            assert_eq!(
                after,
                Switch {
                    on: true,
                    generation: 1
                },
                "cut {cut}"
            );
            off_with(&mut flow, &keys, &auth).unwrap_or_else(|e| panic!("cut {cut}: {e:?}"));
            assert_eq!(
                flow.switch(),
                Switch {
                    on: false,
                    generation: 2
                },
                "cut {cut}: the retry ends at off, generation 2"
            );
        } else {
            assert_eq!(
                after,
                Switch {
                    on: false,
                    generation: 2
                },
                "cut {cut}"
            );
            let (_, text) = off_with(&mut flow, &keys, &auth).unwrap_err();
            assert_eq!(
                text,
                "made for generation 1, the switch is at 2: the switch changed after it was signed",
                "cut {cut}"
            );
        }
    }
}

/// The record is written and the view is not (the render failed or the power was cut before it):
/// the switch is off at generation 2, the view is pending, and the old authorization is spent. The
/// view is brought current by the retry, and the switch is still the one the record says.
#[test]
fn a_cut_after_the_record_and_before_the_view_leaves_the_view_pending_and_the_authorization_spent()
{
    let dir = Dir::new("crash-render");
    let keys = keys_file(&dir);
    let render = dir.0.join("run");
    let record = dir.0.join("security.rec");
    let mut first = Flow::open(&record, &dir.0, false);
    first.set_plain(true, &[ok("slots")]).unwrap();
    drop(first);
    let mut flow = Flow::open(&record, &render, false); // no render directory: the view is pending
    assert!(flow.view_pending());
    let auth = off_auth(&test_key(), SERIAL, 1);
    off_with(&mut flow, &keys, &auth).unwrap();
    assert!(
        flow.view_pending(),
        "the record is off; the view is not written"
    );
    assert!(
        flow.state_lines(Board::default())
            .contains(&"view pending".to_string())
    );
    // A restart after the crash: the record is the truth; the old authorization is refused.
    let mut restarted = Flow::open(&record, &render, false);
    assert_eq!(
        restarted.switch(),
        Switch {
            on: false,
            generation: 2
        }
    );
    let (_, text) = off_with(&mut restarted, &keys, &auth).unwrap_err();
    assert!(text.contains("the switch is at 2"), "{text}");
    fs::create_dir(&render).unwrap();
    restarted.retry_render();
    assert_eq!(
        fs::read_to_string(render.join("security")).unwrap(),
        "off\ngeneration 2\n"
    );
}

/// The vector in `docs/slots.md`, "Turning it off": the test key (seed `0x07` repeated), the serial
/// `0123456789abcdef`, the switch at generation 1. An independent Ed25519 implementation agrees.
#[test]
fn the_documented_vector_is_the_message_and_the_record() {
    let message = security_switch_message(SERIAL.as_bytes(), false, 1);
    let hex: String = message.iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(
        hex,
        "6c656d6e6f732d73656375726974792d73776974636800010000001000000030313233343536373839616263646566000100000000000000"
    );
    let record = record_over(&test_key(), &message);
    let hex: String = record.iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(
        hex,
        "4c4d4e535349473101000000fe812c12f3ab4ce6ac5db69ac352f906cb1b11ef43fb33e252ef7ff552263889f42567f5963b8e7333394a8c248e380617a7d964d5e5509b8f99dbe56c9e32376275c457f831a4163d5e959a584694e06fab342ebf036002b97d4190888aad00"
    );
    let held = held_keys(Some(&keys_file(&Dir::new("vector"))));
    assert_eq!(held.len(), 1);
    authorize_off(&record, &held, Some(SERIAL), 1).unwrap();
}

/// An authorized off with a power cut before each step. Whatever the cut, the board ends on or
/// consistently off (the mark is never less strict than the record), the view is never off while the
/// record says on, and the authorization follows the record: still good while the record reads on,
/// spent once it reads off. A restart's repair brings the mark and the view to the record.
#[test]
fn an_authorized_off_cut_before_each_step_ends_consistently_and_the_authorization_follows_the_record()
 {
    let key = test_key();
    for fault in [Step::Record, Step::View, Step::Boot] {
        let at = format!("crash before {fault:?}");
        let dir = Dir::new("auth-crash");
        let keys = keys_file(&dir);
        let record = dir.0.join("record");
        let mark = Mark::new(&dir.0);
        let mut flow = Flow::open(&record, &dir.0, false);
        flow.set(
            true,
            &[ok("slots")],
            Board::default(),
            &[],
            &mut mark.apply(),
        )
        .unwrap(); // on at generation 1, marked
        let auth = off_auth(&key, SERIAL, 1);
        flow.fault = Some(fault);
        let board = Board {
            keys: Some(&keys),
            serial: Some(SERIAL),
        };
        let cut = flow
            .set(false, &[], board, &auth, &mut mark.apply())
            .unwrap_err();
        assert!(cut.1.starts_with("injected: power cut"), "{at}: {}", cut.1);
        assert_never_weaker(&mark, &record, &at);
        // The view is only ever rendered after the record: it never reads off while the record is on.
        let view = security::read_at(&dir.0.join(security::FILE_NAME));
        if record_on(&record) {
            assert_eq!(
                view,
                security::View::Set(Switch {
                    on: true,
                    generation: 1
                }),
                "{at}: the view read off while the record reads on"
            );
        }
        // A new daemon at the cut: the authorization is good exactly while the record reads on.
        let mut restarted = Flow::open(&record, &dir.0, false);
        let again = restarted.set(false, &[], board, &auth, &mut mark.apply());
        if fault == Step::Record {
            again.unwrap_or_else(|e| panic!("{at}: the retry is refused: {e:?}"));
            assert_eq!(
                restarted.switch(),
                Switch {
                    on: false,
                    generation: 2
                },
                "{at}"
            );
        } else {
            let (status, text) = again.unwrap_err();
            assert_eq!(status, Status::NOT_PERMITTED, "{at}");
            assert_eq!(
                text,
                "made for generation 1, the switch is at 2: the switch changed after it was signed",
                "{at}"
            );
        }
        restart_repairs(&dir, &record, &mark, &at);
    }
}
