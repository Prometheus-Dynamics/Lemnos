//! The daemon's reading of the boot's grow state. The daemon never opens the disk: these tests give it
//! no disk at all, and check each state the boot can leave.

use super::*;

/// A data root with `board/slots` in it, removed when the test ends.
struct Data(PathBuf);

impl Data {
    fn new(name: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("lemnos-grow-state-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("board").join("slots")).unwrap();
        Self(root)
    }

    fn write(&self, text: &str) {
        fs::write(self.0.join("board/slots").join(STATE_FILE), text).unwrap();
    }

    fn task(&self) -> Task {
        // No disk: a tick that reached for one would fail with `source`'s reason.
        Task::new(Err("the daemon has no disk to open".into()), self.0.clone())
    }
}

impl Drop for Data {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn the_grow_is_on_by_default_and_a_board_can_opt_out() {
    assert!(enabled(None), "unset: on");
    assert!(enabled(Some(true)));
    assert!(!enabled(Some(false)), "grow_data = false opts out");
}

#[test]
fn no_state_yet_is_pending_and_is_read_again_without_a_note() {
    let data = Data::new("pending");
    let mut task = data.task();
    assert!(matches!(task.tick_real(0), Tick::At(_)));
    assert!(task.refused().is_none());
    assert!(task.progress().is_none());
}

#[test]
fn a_grown_or_not_needed_partition_finishes_the_task_without_a_disk() {
    for text in ["grown\n", "not-needed\n"] {
        let data = Data::new("finished");
        data.write(text);
        let mut task = data.task();
        assert_eq!(task.tick_real(0), Tick::Finished, "{text:?}");
    }
}

#[test]
fn a_refusal_is_named_and_noted_once() {
    let data = Data::new("refused");
    data.write("refused data-read-only data-read-only: /data is mounted read-only\n");
    let mut task = data.task();
    let first = task.tick_real(0);
    assert_eq!(
        first,
        Tick::Refused {
            why: "data-read-only: /data is mounted read-only".into(),
            fresh: true,
        }
    );
    assert_eq!(task.refused().unwrap().state, "data-read-only");
    assert_eq!(
        task.tick_real(1),
        Tick::Refused {
            why: "data-read-only: /data is mounted read-only".into(),
            fresh: false,
        },
        "not noted again"
    );
}

#[test]
fn a_failed_boot_is_noted_once_and_read_again_later() {
    let data = Data::new("failed");
    data.write("failed busy\n");
    let mut task = data.task();
    let Tick::Failed { why, fresh, at } = task.tick_real(10) else {
        panic!("a failed boot is a failed tick");
    };
    assert_eq!(why, "busy");
    assert!(fresh);
    assert!(at > 10);
    assert_eq!(task.retry.last, "busy");
    assert!(matches!(
        task.tick_real(20),
        Tick::Failed { fresh: false, .. }
    ));
}

#[test]
fn a_grow_in_flight_is_progress_and_its_overrun_is_named() {
    let data = Data::new("running");
    let now_ms = unix_ms();
    data.write(&format!(
        "attempt started b1 {} 1000 4294967296\n",
        now_ms.saturating_sub(5_000)
    ));
    let mut task = data.task();
    assert!(matches!(task.tick_real(0), Tick::At(_)));
    let progress = task.progress().expect("progress while running");
    assert_eq!(progress.target_bytes, 4_294_967_296);
    assert_eq!(progress.bound_ns, 1_000_000_000);
    assert!(progress.overrun, "5 s elapsed past a 1 s bound");

    data.write(&format!("attempt started b1 {now_ms} 60000 4294967296\n"));
    assert!(matches!(task.tick_real(1), Tick::At(_)));
    assert!(!task.progress().unwrap().overrun);
}

#[test]
fn an_unconfirmed_resize_is_kept_with_its_sizes() {
    let data = Data::new("unconfirmed");
    data.write("unconfirmed 100 200 grown-unconfirmed: the view shows 200 bytes\n");
    let mut task = data.task();
    assert!(matches!(task.tick_real(0), Tick::At(_)));
    let u = task.unconfirmed().expect("unconfirmed is kept");
    assert_eq!((u.baseline, u.seen), (Some(100), 200));
    assert_eq!(u.reason, "grown-unconfirmed: the view shows 200 bytes");
}

#[test]
fn a_state_that_does_not_decode_is_a_failed_tick_not_a_grow() {
    let data = Data::new("damaged");
    data.write("who knows\n");
    let mut task = data.task();
    assert!(matches!(task.tick_real(0), Tick::Failed { .. }));
}
