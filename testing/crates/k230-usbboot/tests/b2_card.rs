//! The B2 card writes rehearsed: the scratch build's own code (`lemnos_driver_k230_usbboot_sys::scratch`) over the
//! register models with the SD controller and a card of the card of record's size in its slot, and the host's
//! scratch steps (`lemnos_k230_usbboot::b2::scratch`), the plan's hashes taken from the model card before and after.
//! The same cases, with the board's bounds and a record, are `k230-b2 rehearse scratch-*`.

use std::time::Duration;

use lemnos_driver_k230_usbboot_sys::scratch::{NO_CONTROLLER_LINE, NOT_THE_CARD_LINE, PADS_LINE};
use lemnos_k230_usbboot::b2::{Bounds, Plan, Verdict};
use lemnos_k230_usbboot::b2sim::{Build, CardRun, Faults, Reset, card_run};
use lemnos_k230_usbboot::sdcard::SimCard;
use lemnos_k230_usbboot_stage::bounds::CARD_DECLARED;

fn plan() -> Plan {
    Plan {
        bounds: Bounds {
            enumerate: Duration::from_secs(3),
            back: Duration::from_secs(10),
            ..Bounds::BOARD
        },
        ..Plan::scratch()
    }
}

fn scratch() -> Faults {
    Faults {
        build: Build::Scratch,
        ..Faults::default()
    }
}

fn run(faults: Faults, card: SimCard) -> (CardRun, Vec<String>) {
    let mut lines = Vec::new();
    let run = card_run(faults, card, &plan(), &mut |t| lines.push(t.to_string()));
    (run, lines)
}

fn console_has(run: &CardRun, start: &str) -> bool {
    run.console.iter().any(|(_, l)| l.starts_with(start))
}

fn soc_reset_and_back(run: &CardRun) {
    assert!(run.outcome.back.is_some(), "the boot ROM came back");
    assert!(!run.resets.is_empty());
    assert!(
        run.resets.iter().all(|(_, r)| *r == Reset::Soc),
        "{:?}",
        run.resets
    );
}

#[test]
fn the_plans_sequence_writes_only_the_range_refuses_every_write_outside_and_leaves_every_hash_as_before()
 {
    let (run, lines) = run(scratch(), SimCard::of_record());
    assert_eq!(run.outcome.verdict, Verdict::Passed, "{lines:#?}");
    assert!(run.problems().is_empty(), "{:?}", run.problems());
    soc_reset_and_back(&run);
    // The edges, the range, the zeroing: nothing else reached the card.
    assert_eq!(run.writes.len(), 2 + 2 * 2048);
    assert!(
        run.writes
            .iter()
            .all(|l| (CARD_DECLARED.first..=CARD_DECLARED.last).contains(l))
    );
    // Every deliberate write outside was refused, unmarked and marked.
    assert_eq!(
        lines.iter().filter(|l| l.ends_with(", refused")).count(),
        22
    );
    assert!(console_has(&run, "b2 card-retries 00000000"));
    assert!(console_has(&run, "b2 card-cid0 03534453"));
    // One feed (a kick of each dog) per verified write round, and none for anything else: the edges and the 16
    // range and 16 zero writes, not the reads, refusals, idle waits or polls.
    assert_eq!(run.feed_kicks().len(), 2 * 34);
    assert!(console_has(&run, "b2 rounds 00000022"));
}

#[test]
fn a_round_that_stalls_before_its_compare_feeds_nothing_and_the_watchdog_resets_within_one_period()
{
    // The card takes the sixth range write's data and then stays busy for good. The host keeps the session busy
    // with reads and identifies, no END: only the watchdog can end it, and only if nothing but verified progress feeds.
    let stall = CARD_DECLARED.first + 5 * 128;
    let mut card = SimCard::of_record();
    card.stall_after_write = Some(stall);
    let plan = Plan {
        keep_asking: Some(Duration::from_secs(150)),
        bounds: Bounds {
            back: Duration::from_secs(200),
            ..plan().bounds
        },
        ..Plan::scratch()
    };
    let mut lines = Vec::new();
    let run = card_run(scratch(), card, &plan, &mut |t| lines.push(t.to_string()));
    assert!(
        matches!(&run.outcome.verdict, Verdict::Failed(f) if f.iter().any(|t| t.contains("status 2"))),
        "{lines:#?}"
    );
    let stalled = run.stalled_at.expect("the card stalled");
    let feeds = run.feed_kicks();
    assert_eq!(
        feeds.len(),
        2 * 7,
        "the two edges and five range writes, verified"
    );
    assert!(
        run.kicks.iter().all(|(at, _)| *at < stalled),
        "a kick after the stall"
    );
    let last = feeds.last().unwrap().0;
    let reset = run
        .resets_from_load
        .iter()
        .find(|(_, r)| *r == Reset::Watchdog)
        .map(|(at, _)| *at)
        .expect("the watchdog reset the board");
    assert!(
        reset - last <= 89.5,
        "{} s after the last kick",
        reset - last
    );
    assert_eq!(run.resets.len(), 1);
    assert!(run.outcome.back.is_some());
    assert!(
        run.problems_outside().is_empty(),
        "{:?}",
        run.problems_outside()
    );
    assert_eq!(run.writes.last(), Some(&stall));
}

#[test]
fn another_card_is_never_written_and_usb_never_comes_up() {
    let (run, lines) = run(scratch(), SimCard::another());
    assert!(
        matches!(run.outcome.verdict, Verdict::GaveUp(_)),
        "{lines:#?}"
    );
    assert!(run.writes.is_empty());
    assert!(run.problems().is_empty(), "{:?}", run.problems());
    assert!(console_has(&run, NOT_THE_CARD_LINE));
    soc_reset_and_back(&run);
}

#[test]
fn a_card_that_does_not_keep_a_write_stops_the_run_at_its_first_write() {
    let mut card = SimCard::of_record();
    card.drops_writes = true;
    let (run, lines) = run(scratch(), card);
    assert!(
        matches!(&run.outcome.verdict, Verdict::Failed(f) if f.iter().any(|t| t.contains("status 3"))),
        "{lines:#?}"
    );
    assert_eq!(run.writes, vec![CARD_DECLARED.first]);
    assert!(
        lines
            .iter()
            .any(|l| l.contains("END sent after the failure"))
    );
    soc_reset_and_back(&run);
}

#[test]
fn a_sector_whose_data_fails_its_crc_is_tried_again_and_counted() {
    let mut card = SimCard::of_record();
    card.crc_fails = Some((CARD_DECLARED.first + 1000, 2));
    let (run, lines) = run(scratch(), card);
    assert_eq!(run.outcome.verdict, Verdict::Passed, "{lines:#?}");
    assert!(run.problems().is_empty(), "{:?}", run.problems());
    assert!(console_has(&run, "b2 card-retries 00000002"));
}

#[test]
fn without_a_controller_or_with_the_pads_elsewhere_the_card_is_not_touched() {
    for (faults, line) in [
        (
            Faults {
                no_sd_parameter: true,
                ..scratch()
            },
            NO_CONTROLLER_LINE,
        ),
        (
            Faults {
                sd_pads_off: true,
                ..scratch()
            },
            PADS_LINE,
        ),
    ] {
        let (run, lines) = run(faults, SimCard::of_record());
        assert!(
            matches!(run.outcome.verdict, Verdict::GaveUp(_)),
            "{lines:#?}"
        );
        assert!(run.writes.is_empty());
        assert!(console_has(&run, line), "{:?}", run.console);
        soc_reset_and_back(&run);
    }
}

#[test]
fn the_check_build_answering_the_scratch_mode_gets_no_write() {
    let (run, lines) = run(Faults::default(), SimCard::of_record());
    assert!(
        matches!(&run.outcome.verdict, Verdict::Failed(f) if f.iter().any(|t| t.contains("not the scratch build"))),
        "{lines:#?}"
    );
    assert!(!lines.iter().any(|l| l.contains("status 0")));
    assert!(run.writes.is_empty());
    soc_reset_and_back(&run);
}
