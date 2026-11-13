use super::*;

/// A modelled board. Each side effect is one step; a cut stops before the step runs and, as a power
/// loss does, clears the boot count. Steps are logged by name, so the order is checked.
#[derive(Debug, Default)]
struct Board {
    default: String,
    previous: String,
    not_good: Vec<String>,
    episodes: Option<(String, u8)>,
    count: u8,
    history: Vec<String>,
    indicator: Option<String>,
    log: Vec<&'static str>,
    steps: usize,
    cut_at: Option<usize>,
    cut: bool,
}

#[derive(Debug, PartialEq, Eq)]
struct Cut;

impl Board {
    fn new(default: &str, previous: &str, count: u8) -> Self {
        Self {
            default: default.into(),
            previous: previous.into(),
            count,
            ..Self::default()
        }
    }

    /// One step. Returns `Err` before the step runs when the cut is here.
    fn step(&mut self, name: &'static str) -> Result<(), Cut> {
        if self.cut {
            return Err(Cut);
        }
        if self.cut_at == Some(self.steps) {
            self.cut = true;
            self.count = 0;
            return Err(Cut);
        }
        self.steps += 1;
        self.log.push(name);
        Ok(())
    }
}

impl Actions for Board {
    type Error = Cut;

    fn report(&mut self, arrival: &Arrival, cause: Cause) -> Result<(), Cut> {
        self.step("report")?;
        self.history.push(format!(
            "count={} previous={} rsts={:#x} cause={:?} files_match={}",
            self.count, self.previous, arrival.rsts, cause, arrival.files_match
        ));
        self.indicator = Some("on-previous-by-count".into());
        Ok(())
    }

    fn mark_not_good(&mut self, slot: &str) -> Result<(), Cut> {
        self.step("mark_not_good")?;
        self.not_good.push(slot.into());
        Ok(())
    }

    fn record_episode(&mut self, slot: &str, episodes: u8) -> Result<(), Cut> {
        self.step("record_episode")?;
        self.episodes = Some((slot.into(), episodes));
        Ok(())
    }

    fn point_default(&mut self, to: &str) -> Result<(), Cut> {
        self.step("point_default")?;
        self.default = to.into();
        Ok(())
    }

    fn clear_episodes(&mut self, _slot: &str) -> Result<(), Cut> {
        self.step("clear_episodes")?;
        self.episodes = None;
        Ok(())
    }

    fn clear_count(&mut self) -> Result<(), Cut> {
        self.step("clear_count")?;
        self.count = 0;
        Ok(())
    }
}

fn arrival(files_match: bool, rsts: u32, episodes_before: u8) -> Arrival {
    Arrival {
        default: "os-1".into(),
        previous: "os-0".into(),
        files_match,
        rsts,
        episodes_before,
        threshold: 2,
    }
}

const POWER: u32 = 0x0000_1000;
/// A warm restart: the observed tryboot reading `0x1020` (the power-on bit with the warm-reset bit).
const NOT_POWER: u32 = 0x0000_1020;

/// Steps of a whole run of `arrival`, for the cut sweep.
fn steps_of(arr: &Arrival) -> usize {
    let mut b = Board::new("os-1", "os-0", 5);
    on_arrival(&mut b, arr).unwrap();
    b.steps
}

#[test]
fn a_mismatch_demotes_at_once_in_order() {
    let mut b = Board::new("os-1", "os-0", 5);
    let out = on_arrival(&mut b, &arrival(false, NOT_POWER, 0)).unwrap();
    assert_eq!(out, Outcome::DemotedAtOnce);
    assert_eq!(b.default, "os-0");
    assert_eq!(b.not_good, vec!["os-1".to_string()]);
    assert_eq!(b.count, 0);
    assert_eq!(
        b.log,
        vec!["report", "mark_not_good", "point_default", "clear_count"]
    );
    assert_eq!(b.history.len(), 1);
    assert_eq!(b.indicator.as_deref(), Some("on-previous-by-count"));
}

#[test]
fn three_power_type_resets_during_boot_do_not_demote_and_record_no_episode() {
    let mut b = Board::new("os-1", "os-0", 5);
    for _ in 0..3 {
        b.count = 5;
        let out = on_arrival(&mut b, &arrival(true, POWER, 0)).unwrap();
        assert_eq!(out, Outcome::NoEpisode);
    }
    assert_eq!(b.default, "os-1", "the default is kept");
    assert_eq!(b.episodes, None, "no episode was recorded");
    assert!(b.not_good.is_empty());
    assert_eq!(b.count, 0);
}

#[test]
fn a_hanging_kernel_demotes_after_the_threshold_and_not_before() {
    let mut b = Board::new("os-1", "os-0", 5);
    let first = on_arrival(&mut b, &arrival(true, NOT_POWER, 0)).unwrap();
    assert_eq!(first, Outcome::EpisodeRecorded { episodes: 1 });
    assert_eq!(b.default, "os-1", "below the threshold the default stays");
    assert!(b.not_good.is_empty());
    assert_eq!(b.episodes, Some(("os-1".into(), 1)));

    b.count = 5;
    let second = on_arrival(&mut b, &arrival(true, NOT_POWER, 1)).unwrap();
    assert_eq!(second, Outcome::Demoted { episodes: 2 });
    assert_eq!(b.default, "os-0");
    assert_eq!(b.not_good, vec!["os-1".to_string()]);
    assert_eq!(b.count, 0);
}

#[test]
fn changed_files_demote_at_once_whatever_the_cause() {
    for rsts in [POWER, NOT_POWER] {
        let mut b = Board::new("os-1", "os-0", 5);
        let out = on_arrival(&mut b, &arrival(false, rsts, 0)).unwrap();
        assert_eq!(out, Outcome::DemotedAtOnce, "rsts {rsts:#x}");
        assert_eq!(b.default, "os-0");
    }
}

#[test]
fn a_boot_good_clears_the_episodes_then_the_count() {
    let mut b = Board::new("os-1", "os-0", 5);
    on_arrival(&mut b, &arrival(true, NOT_POWER, 0)).unwrap();
    assert!(b.episodes.is_some());
    b.count = 2;
    on_boot_good(&mut b, "os-1").unwrap();
    assert_eq!(b.episodes, None);
    assert_eq!(b.count, 0);
    assert_eq!(
        &b.log[b.log.len() - 2..],
        &["clear_episodes", "clear_count"]
    );
}

#[test]
fn an_intended_restart_clears_the_count_before_it_happens() {
    let mut b = Board::new("os-1", "os-0", 6);
    before_own_restart(&mut b).unwrap();
    assert_eq!(b.count, 0);
    assert_eq!(b.log, vec!["clear_count"]);
}

/// Every cut point of each path: the default is the original or the previous; a demotion always
/// marks the original not good; the episode record moves by at most one, and only against the
/// original; the count is clear after any cut (a power loss clears it); and an arrival that was cut
/// is not run again by the same boot. A cut between the episode write and the count clear therefore
/// cannot count twice: the count is gone, so the next arrival needs fresh boots.
fn sweep(name: &str, before: Option<u8>, files_match: bool, rsts: u32) {
    let arr = arrival(files_match, rsts, before.unwrap_or(0));
    let total = steps_of(&arr);
    let mut completed = Board::new("os-1", "os-0", 5);
    on_arrival(&mut completed, &arr).unwrap();
    for cut in 0..=total {
        let mut b = Board::new("os-1", "os-0", 5);
        if let Some(n) = before {
            b.episodes = Some(("os-1".into(), n));
        }
        b.cut_at = Some(cut);
        let result = on_arrival(&mut b, &arr);
        if cut < total {
            assert_eq!(result, Err(Cut), "{name}: cut {cut} stops the sequence");
        }
        assert!(
            b.default == "os-1" || b.default == "os-0",
            "{name}: cut {cut} left default {}",
            b.default
        );
        if b.default == "os-0" {
            assert_eq!(b.not_good, vec!["os-1".to_string()], "{name}: cut {cut}");
        }
        let after = b.episodes.as_ref().map(|(_, n)| *n).unwrap_or(0);
        let start = before.unwrap_or(0);
        assert!(
            after == start || after == start + 1,
            "{name}: cut {cut} episodes {after} from {start}"
        );
        assert_eq!(b.count, 0, "{name}: cut {cut} leaves the count clear");
    }
    assert_eq!(completed.count, 0);
}

#[test]
fn a_cut_at_every_step_of_a_mismatch_leaves_a_consistent_state() {
    sweep("mismatch", None, false, NOT_POWER);
}

#[test]
fn a_cut_at_every_step_of_an_episode_below_threshold_leaves_a_consistent_state() {
    sweep("episode", None, true, NOT_POWER);
}

#[test]
fn a_cut_at_every_step_of_the_demoting_episode_leaves_a_consistent_state() {
    sweep("demoting episode", Some(1), true, NOT_POWER);
}

#[test]
fn a_cut_at_every_step_of_a_power_reset_leaves_a_consistent_state() {
    sweep("power", None, true, POWER);
}

#[test]
fn a_cut_between_the_episode_write_and_the_count_clear_does_not_double_count() {
    // The episode step is step 2 (report, record_episode, clear_count). A cut before the count clear.
    let arr = arrival(true, NOT_POWER, 0);
    let mut b = Board::new("os-1", "os-0", 5);
    b.cut_at = Some(2);
    assert_eq!(on_arrival(&mut b, &arr), Err(Cut));
    assert_eq!(b.episodes, Some(("os-1".into(), 1)), "recorded once");
    assert_eq!(
        b.count, 0,
        "the power loss cleared the count: no second arrival this boot"
    );
}

#[test]
fn a_no_previous_case_keeps_the_default_when_the_pointer_names_it() {
    // With no previous, the previous pointer holds the default's prefix: a demotion points at the
    // same slot, so the default stays where it was and only the not-good mark is kept.
    let mut b = Board::new("os-1", "os-1", 5);
    let arr = Arrival {
        previous: "os-1".into(),
        ..arrival(false, NOT_POWER, 0)
    };
    on_arrival(&mut b, &arr).unwrap();
    assert_eq!(b.default, "os-1");
}
