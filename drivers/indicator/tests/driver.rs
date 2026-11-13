//! The indicator on a fake `pixels/1` device, ticked at times the test chooses.

mod support;

use lemnos_contract_indicator::{Indicator as _, NO_PROGRESS};
use lemnos_core::{ConfigValue, Status};
use support::{FakePixels, driver, led, plain, settle, start, tick};

/// `colour` on all `count` LEDs.
fn all(count: usize, colour: [u8; 4]) -> Vec<[u8; 4]> {
    vec![colour; count]
}

const RED: [u8; 4] = [255, 0, 0, 0];
const OFF: [u8; 4] = [0, 0, 0, 0];

#[test]
fn idle_is_drawn_from_the_start_and_is_dark_by_default() {
    let fake = FakePixels::leak(4);
    let mut indicator = driver(fake, &plain());
    assert_eq!(fake.shows(), 0, "starting writes nothing");
    settle(&mut indicator, 0, 1_000);
    assert_eq!(fake.last(), all(4, OFF));
    assert_eq!(fake.show_attempts(), 1, "an off state is drawn once");
}

#[test]
fn a_solid_state_draws_once_and_asks_for_no_more_ticks() {
    let fake = FakePixels::leak(4);
    let mut indicator = driver(fake, &[("state.done", ConfigValue::Str("solid #00ff00"))]);
    indicator.set("done", NO_PROGRESS).unwrap();
    assert_eq!(tick(&mut indicator, 0), None);
    assert_eq!(fake.last(), all(4, led(0, 255, 0)));
    assert_eq!(
        tick(&mut indicator, 5_000),
        None,
        "nothing to do when steady"
    );
    assert_eq!(fake.show_attempts(), 1);
}

#[test]
fn blink_is_on_for_half_its_period_and_off_for_half() {
    let fake = FakePixels::leak(3);
    let mut indicator = driver(
        fake,
        &[(
            "state.receiving",
            ConfigValue::Str("blink #ff0000 period=400"),
        )],
    );
    indicator.set("receiving", NO_PROGRESS).unwrap();
    assert_eq!(
        tick(&mut indicator, 0),
        Some(200),
        "the next change is at half the period"
    );
    assert_eq!(fake.last(), all(3, RED));
    assert_eq!(tick(&mut indicator, 200), Some(400));
    assert_eq!(fake.last(), all(3, OFF));
    assert_eq!(tick(&mut indicator, 400), Some(600));
    assert_eq!(fake.last(), all(3, RED));
}

#[test]
fn a_chase_moves_one_led_per_step_round_the_strip() {
    let count = 8;
    let fake = FakePixels::leak(count);
    // One period is one revolution: 800 ms, eight LEDs, so a step every 100 ms, one lit LED at a time.
    let mut indicator = driver(
        fake,
        &[(
            "state.verifying",
            ConfigValue::Str("chase #ffffff period=800"),
        )],
    );
    indicator.set("verifying", NO_PROGRESS).unwrap();
    let lit = |frame: &[[u8; 4]]| -> Vec<usize> {
        frame
            .iter()
            .enumerate()
            .filter(|(_, c)| **c != OFF)
            .map(|(i, _)| i)
            .collect()
    };
    let mut at = 0;
    for step in 0..count as u64 {
        let next = tick(&mut indicator, at);
        assert_eq!(
            lit(&fake.last()),
            vec![step as usize],
            "step {step} at {at} ms"
        );
        at = next.expect("a chase keeps moving");
        assert_eq!(at, (step + 1) * 100, "the step boundary");
    }
    // A full revolution brings it back to LED 0.
    tick(&mut indicator, at);
    assert_eq!(lit(&fake.last()), vec![0]);
}

#[test]
fn a_bar_lights_the_progress_across_the_strip() {
    let count = 8;
    let fake = FakePixels::leak(count);
    let green = [0, 200, 0, 0];
    let dim = [0, 16, 0, 0];
    let mut indicator = driver(
        fake,
        &[("state.writing", ConfigValue::Str("bar #00c800 off=#001000"))],
    );
    let on = |n: usize| -> Vec<[u8; 4]> {
        (0..count)
            .map(|i| if i < n { green } else { dim })
            .collect()
    };
    indicator.set("writing", 0).unwrap();
    tick(&mut indicator, 0);
    assert_eq!(fake.last(), on(0));
    indicator.set("writing", 500).unwrap();
    tick(&mut indicator, 10);
    assert_eq!(fake.last(), on(4), "half of eight LEDs");
    indicator.set("writing", 1000).unwrap();
    tick(&mut indicator, 20);
    assert_eq!(fake.last(), on(8), "the whole strip");
    indicator.set("writing", NO_PROGRESS).unwrap();
    tick(&mut indicator, 30);
    assert_eq!(
        fake.last(),
        all(count, dim),
        "no progress: the unlit colour everywhere"
    );
}

#[test]
fn brightness_scales_every_colour() {
    let fake = FakePixels::leak(2);
    let mut indicator = driver(
        fake,
        &[
            ("state.done", ConfigValue::Str("solid #ff8000")),
            ("brightness", ConfigValue::Int(128)),
        ],
    );
    indicator.set("done", NO_PROGRESS).unwrap();
    tick(&mut indicator, 0);
    assert_eq!(fake.last(), all(2, [128, 64, 0, 0]));
}

#[test]
fn the_states_are_listed_and_an_unknown_state_is_refused() {
    let mut indicator = driver(FakePixels::leak(2), &plain());
    let mut buf = [0u8; 128];
    let n = indicator.states(&mut buf).unwrap();
    assert_eq!(
        std::str::from_utf8(&buf[..n]).unwrap(),
        "idle,receiving,writing,verifying,done,failed"
    );
    let mut small = [0u8; 4];
    assert_eq!(indicator.states(&mut small), Err(Status::BUFFER_TOO_SMALL));

    assert_eq!(
        indicator.set("blinking", NO_PROGRESS),
        Err(Status::INVALID_ARGUMENT)
    );
    assert_eq!(
        indicator.set("writing", 1001),
        Err(Status::INVALID_ARGUMENT)
    );
    assert_eq!(indicator.set("writing", 1000), Ok(()));
    assert_eq!(indicator.set("writing", NO_PROGRESS), Ok(()));
}

#[test]
fn a_pattern_the_board_gives_that_is_not_a_pattern_is_refused_at_start() {
    let fake = FakePixels::leak(2);
    for text in [
        "",
        "sparkle #fff",
        "blink #fff",
        "blink #fff period=50",
        "solid #zzz",
        "bar #fff period=500",
    ] {
        let entries = [("state.failed", ConfigValue::Str(text))];
        let refused = start(fake, &entries);
        assert_eq!(refused.err(), Some(Status::INVALID_ARGUMENT), "{text:?}");
    }
}

#[test]
fn a_failed_write_is_retried_on_a_backoff_that_is_capped_and_then_succeeds() {
    let fake = FakePixels::leak(2);
    let mut indicator = driver(fake, &[("state.failed", ConfigValue::Str("solid #ff0000"))]);
    indicator.set("failed", NO_PROGRESS).unwrap();
    fake.fail_next_shows(12);
    // Twelve failed writes, then one that works. The gaps between attempts double from 100 ms and
    // are capped at 8 s; the driver never gives up.
    let mut attempts = vec![0u64];
    let mut next = tick(&mut indicator, 0);
    while let Some(at) = next {
        attempts.push(at);
        next = tick(&mut indicator, at);
        if attempts.len() == 2 {
            assert!(matches!(
                indicator_health(&indicator),
                lemnos_core::Health::Failed(_)
            ));
        }
    }
    let gaps: Vec<u64> = attempts.windows(2).map(|w| w[1] - w[0]).collect();
    assert_eq!(
        gaps,
        vec![
            100, 200, 400, 800, 1600, 3200, 6400, 8000, 8000, 8000, 8000, 8000
        ]
    );
    assert_eq!(fake.show_attempts(), 13);
    assert_eq!(fake.last(), all(2, RED));
    assert!(matches!(
        indicator_health(&indicator),
        lemnos_core::Health::Ok
    ));
}

fn indicator_health(indicator: &lemnos_driver_indicator::Indicator) -> lemnos_core::Health {
    <lemnos_driver_indicator::Indicator as lemnos_core::Lifecycle>::health(indicator)
}
