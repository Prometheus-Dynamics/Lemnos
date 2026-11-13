use super::*;

#[test]
fn three_power_type_resets_during_boot_do_not_demote_and_record_no_episode() {
    let mut episodes = 0u8;
    for _ in 0..3 {
        let d = decide(true, cause(POWER_ON_RESET), episodes, DEFAULT_THRESHOLD);
        assert_eq!(
            d,
            Decision::NoEpisode,
            "a power-type reset is never an episode"
        );
        episodes = match d {
            Decision::Episode { episodes, .. } => episodes,
            _ => episodes,
        };
    }
    assert_eq!(episodes, 0, "no episode was recorded");
}

#[test]
fn a_warm_restart_demotes_after_the_threshold_and_not_before() {
    let threshold = threshold(None).unwrap();
    assert_eq!(threshold, 2);
    // First arrival: an episode, below the threshold, the default stays.
    let first = decide(true, cause(WARM_RESET), 0, threshold);
    assert_eq!(
        first,
        Decision::Episode {
            episodes: 1,
            reached: false
        }
    );
    // Second arrival, with the first recorded: the threshold is reached.
    let second = decide(true, cause(WARM_RESET), 1, threshold);
    assert_eq!(
        second,
        Decision::Episode {
            episodes: 2,
            reached: true
        }
    );
}

#[test]
fn changed_files_demote_at_once() {
    // Whatever the cause and the episode count: a manifest mismatch is not a transient.
    for cause_value in [POWER_ON_RESET, WARM_RESET, 0x0000_0040] {
        assert_eq!(
            decide(false, cause(cause_value), 0, DEFAULT_THRESHOLD),
            Decision::DemoteNow
        );
    }
}

#[test]
fn a_boot_good_clears_the_episodes() {
    assert_eq!(after_boot_good(), 0);
}

#[test]
fn the_threshold_is_a_board_file_key_with_default_2_and_minimum_1() {
    assert_eq!(threshold(None), Ok(2));
    assert_eq!(threshold(Some(1)), Ok(1));
    assert_eq!(threshold(Some(5)), Ok(5));
    assert!(threshold(Some(0)).is_err());
}

/// The three values read off a real board (the coordinator's reads, 2026-11-13).
#[test]
fn the_observed_values_classify_as_read() {
    assert_eq!(
        cause(0x0000_1000),
        Cause::Power,
        "power-on after a power loss"
    );
    assert_eq!(cause(0x0000_0020), Cause::Restart, "software restart");
    assert_eq!(
        cause(0x0000_1020),
        Cause::Restart,
        "tryboot boot: the power-on bit stays set"
    );
}

/// Each observed value, and the values next to it: the rule takes the full value, so a set bit
/// that no rule names makes the reading unknown, whatever else is set.
#[test]
fn the_full_value_decides_and_no_bit_is_masked() {
    assert_eq!(cause(POWER_ON_RESET), Cause::Power);
    assert_eq!(cause(WARM_RESET), Cause::Restart);
    assert_eq!(cause(TRYBOOT_RESET), Cause::Restart);
    // Bit 6 (0x40) is not a cause on its own, and no field is masked: with the power-on bit the
    // reading is not one of the observed values, so it is unknown.
    assert_eq!(cause(0x0000_0040), Cause::Unknown);
    assert_eq!(cause(POWER_ON_RESET | 0x0000_0040), Cause::Unknown);
    assert_eq!(cause(WARM_RESET | 0x0000_0040), Cause::Unknown);
    // A partition value beside the power-on bit is not a power reset: the full value must match.
    assert_eq!(cause(POWER_ON_RESET | 0x0000_0001), Cause::Unknown);
    assert_eq!(cause(POWER_ON_RESET | 0x0000_0080), Cause::Unknown);
    assert_eq!(cause(WARM_RESET | 0x0000_0080), Cause::Unknown);
    assert_eq!(cause(WARM_RESET | 0x0000_0555), Cause::Unknown);
}

/// Patterns no rule names are unknown, and unknown counts no episode.
#[test]
fn an_unnamed_pattern_is_unknown_and_counts_no_episode() {
    for value in [
        0u32,
        0x0000_0040,
        0x0000_0080,
        0x0000_0100,
        0x0000_2000,
        0x8000_0000,
    ] {
        assert_eq!(cause(value), Cause::Unknown, "{value:#x}");
        assert_eq!(
            decide(true, cause(value), 0, DEFAULT_THRESHOLD),
            Decision::NoEpisode,
            "an unknown pattern is no evidence against the slot: {value:#x}"
        );
    }
}

#[test]
fn the_board_file_threshold_is_parsed_with_its_range() {
    assert_eq!(threshold_from_board(None), Ok(2));
    assert_eq!(threshold_from_board(Some(1)), Ok(1));
    assert_eq!(threshold_from_board(Some(255)), Ok(255));
    assert!(threshold_from_board(Some(0)).is_err());
    assert!(threshold_from_board(Some(-3)).is_err());
    assert!(threshold_from_board(Some(256)).is_err());
}
