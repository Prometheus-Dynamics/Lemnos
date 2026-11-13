use super::*;
use std::sync::atomic::{AtomicU32, Ordering};

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
    assert!(flow.state_lines().contains(&"switch on".to_string()));
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
        flow.state_lines()
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
    assert!(flow.state_lines().contains(&"view pending".to_string()));
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
        flow.state_lines()
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
    fs::write(&path, format!("# only a comment\n{}\n", "a".repeat(64))).unwrap();
    assert!(
        !trust_root(Some(&path)),
        "a key without an owner is no trust root"
    );
    fs::write(&path, format!("{} the board owner\n", "a".repeat(64))).unwrap();
    assert!(trust_root(Some(&path)));
}

#[test]
fn state_says_what_the_readers_read_when_the_view_is_malformed() {
    let dir = Dir::new("view-corrupt");
    let record = dir.0.join("record");
    // A valid record that says off, and a view someone tore by hand.
    let flow = Flow::open(&record, &dir.0, false);
    assert!(
        flow.state_lines()
            .iter()
            .any(|l| l == "view off generation 0: readers read off")
    );
    fs::write(
        dir.0.join(security::FILE_NAME),
        b"on\ngeneration 0\ngarbage\n",
    )
    .unwrap();
    let lines = flow.state_lines();
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
        flow.state_lines()
            .iter()
            .any(|l| l == "view absent: readers read off"),
        "{:?}",
        flow.state_lines()
    );
}

impl Flow {
    /// A change with no boot side: the record and the view alone.
    fn set_plain(&mut self, on: bool, answers: &[Answer]) -> Result<(), Refusal> {
        self.set(on, answers, &mut |_| Ok(()))
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
            flow.set(true, &[ok("slots")], &mut mark.apply()).is_err(),
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
        flow.set(true, &[ok("slots")], &mut mark.apply()).unwrap(); // on, marked
        assert!(mark.on() && record_on(&record), "{at}: the start state");
        flow.fault = Some(fault);
        assert!(flow.set(false, &[], &mut mark.apply()).is_err(), "{at}");
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
    flow.set(true, &[ok("slots")], &mut mark.apply()).unwrap();
    assert!(mark.on() && record_on(&record));
    assert_eq!(
        security::read_at(&dir.0.join(security::FILE_NAME)),
        security::View::Set(flow.switch())
    );
    flow.set(false, &[], &mut mark.apply()).unwrap();
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
        .set(true, &[ok("slots")], &mut |_| Err("disk gone".to_string()))
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
    flow.set(true, &[ok("slots")], &mut mark.apply()).unwrap();
    let refusal = flow
        .set(false, &[], &mut |_| Err("read-only".to_string()))
        .unwrap_err();
    assert!(refusal.1.contains("stays strict"), "{}", refusal.1);
    // The record is off, the mark is still on: stricter, never weaker.
    assert!(!record_on(&record));
    assert!(mark.on());
}
