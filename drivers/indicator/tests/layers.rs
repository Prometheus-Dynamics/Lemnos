//! The layers: idle, the application's frames, a state, the lamp test, locate. The top one is on
//! the LEDs, the others keep what they are given, and what is under shows when the top one ends.

mod support;

use lemnos_contract_indicator::{Indicator as IndicatorContract, MAX_LOCATE_SECONDS, NO_PROGRESS};
use lemnos_contract_pixels::Pixels;
use lemnos_contract_self_test::SelfTest;
use lemnos_core::{CallerId, ConfigValue, Lifecycle, Status};
use lemnos_driver_indicator::Indicator;
use support::{FakePixels, driver, led, settle, tick};

const LEDS: usize = 16;
const DARK: [u8; 4] = [0, 0, 0, 0];
const WHITE: [u8; 4] = [255, 255, 255, 0];
const RED: [u8; 4] = [255, 0, 0, 0];
const GREEN: [u8; 4] = [0, 255, 0, 0];

/// The application, another program, and the operator's tool: three callers.
const APP: CallerId = CallerId(7);
const OTHER: CallerId = CallerId(8);
const TOOL: CallerId = CallerId(9);

/// An indicator whose states are plain to see: `failed` solid red, `done` solid green, locate a
/// white blink of 400 ms (on for 200, off for 200).
fn indicator(fake: &'static FakePixels) -> Indicator {
    driver(
        fake,
        &[
            ("state.failed", ConfigValue::Str("solid #ff0000")),
            ("state.done", ConfigValue::Str("solid #00ff00")),
            ("locate", ConfigValue::Str("blink #ffffff period=400")),
        ],
    )
}

fn all(colour: [u8; 4]) -> Vec<[u8; 4]> {
    vec![colour; LEDS]
}

/// A frame of the application's: every LED `shade` of blue, so each frame is told from the last.
fn frame(shade: u8) -> Vec<[u8; 4]> {
    all(led(0, 0, shade))
}

/// The method that follows is `caller`'s, as the host says before each call.
fn as_caller(indicator: &mut Indicator, caller: CallerId) -> &mut Indicator {
    Lifecycle::caller(indicator, caller);
    indicator
}

/// The application sends a whole frame the way a client does: 14 LEDs a call, then `show`.
fn app_shows(indicator: &mut Indicator, shade: u8) -> Result<(), Status> {
    let bytes: Vec<u8> = frame(shade).concat();
    let app = as_caller(indicator, APP);
    for (chunk, leds) in bytes.chunks(14 * 4).enumerate() {
        Pixels::set(app, (chunk * 14) as u16, leds)?;
    }
    Pixels::show(app)
}

fn locating(indicator: &mut Indicator) -> bool {
    IndicatorContract::locating(indicator).unwrap()
}

/// Nothing is running: no application, no state. Locate shows over the dark idle and ends by
/// itself, and the LEDs are dark again.
#[test]
fn locate_shows_with_nothing_running_and_ends_at_its_time() {
    let fake = FakePixels::leak(LEDS);
    let mut indicator = indicator(fake);
    tick(&mut indicator, 0);
    assert_eq!(fake.last(), all(DARK));
    assert!(!locating(&mut indicator));

    as_caller(&mut indicator, TOOL).locate(3).unwrap();
    assert!(locating(&mut indicator), "said at once, before the tick");
    tick(&mut indicator, 1_000);
    assert_eq!(fake.last(), all(WHITE), "the blink starts on");
    tick(&mut indicator, 1_200);
    assert_eq!(fake.last(), all(DARK), "and goes off for its other half");
    tick(&mut indicator, 1_400);
    assert_eq!(fake.last(), all(WHITE));
    assert!(locating(&mut indicator));

    // Three seconds after the tick that began it, it is over, with no call to end it.
    let end = settle(&mut indicator, 1_400, 4_000);
    assert_eq!(end, 4_000, "the last tick it asked for is its end");
    assert_eq!(fake.last(), all(DARK));
    assert!(!locating(&mut indicator));
}

/// An application is animating. The board draws locate over it and never asks the application to
/// do anything: its frames keep being taken, and its latest one is back when locate ends.
#[test]
fn locate_shows_over_an_animating_application_and_its_latest_frame_comes_back() {
    let fake = FakePixels::leak(LEDS);
    let mut indicator = indicator(fake);
    tick(&mut indicator, 0);
    app_shows(&mut indicator, 10).unwrap();
    tick(&mut indicator, 10);
    assert_eq!(
        fake.last(),
        frame(10),
        "the application's frame is on the LEDs"
    );

    as_caller(&mut indicator, TOOL).locate(2).unwrap();
    tick(&mut indicator, 100);
    assert_eq!(fake.last(), all(WHITE));
    // The application goes on animating underneath: nothing of it reaches the LEDs.
    for (at, shade) in [(150, 20), (250, 30), (350, 40)] {
        assert_eq!(app_shows(&mut indicator, shade), Ok(()), "still taken");
        tick(&mut indicator, at);
        assert_ne!(fake.last(), frame(shade), "locate is on top at {at} ms");
    }
    let end = settle(&mut indicator, 350, 2_100);
    assert_eq!(end, 2_100);
    assert_eq!(
        fake.last(),
        frame(40),
        "the latest frame, not the one locate covered"
    );
    assert!(!locating(&mut indicator));
    // And the application carries on as before.
    app_shows(&mut indicator, 50).unwrap();
    tick(&mut indicator, 2_200);
    assert_eq!(fake.last(), frame(50));
}

/// Another program is running that drives no LEDs at all (it only holds a session). Locate works
/// the same, asked by anyone, and ends early on `locate(0)` or when the caller that asked goes.
#[test]
fn locate_shows_with_a_program_that_drives_no_leds_and_ends_on_zero_or_with_its_caller() {
    let fake = FakePixels::leak(LEDS);
    let mut indicator = indicator(fake);
    tick(&mut indicator, 0);
    // The other program reads the count and never draws: no application layer comes of it.
    assert_eq!(
        Pixels::pixel_count(as_caller(&mut indicator, OTHER)),
        Ok(16)
    );
    tick(&mut indicator, 10);
    assert_eq!(fake.last(), all(DARK));

    as_caller(&mut indicator, TOOL).locate(60).unwrap();
    tick(&mut indicator, 100);
    assert_eq!(fake.last(), all(WHITE));
    as_caller(&mut indicator, TOOL).locate(0).unwrap();
    assert!(!locating(&mut indicator));
    tick(&mut indicator, 150);
    assert_eq!(fake.last(), all(DARK), "ended at once on 0");

    as_caller(&mut indicator, TOOL).locate(60).unwrap();
    tick(&mut indicator, 200);
    assert_eq!(fake.last(), all(WHITE));
    // A caller that didn't ask going changes nothing; the one that asked takes it along.
    Lifecycle::caller_gone(&mut indicator, OTHER);
    tick(&mut indicator, 250);
    assert!(locating(&mut indicator));
    Lifecycle::caller_gone(&mut indicator, TOOL);
    tick(&mut indicator, 300);
    assert_eq!(fake.last(), all(DARK));
    assert!(!locating(&mut indicator));
}

/// An install is in progress. Locate shows over its state; the states set meanwhile are kept, and
/// the one in force shows when locate ends.
#[test]
fn locate_shows_over_an_install_and_the_state_in_force_comes_back() {
    let fake = FakePixels::leak(LEDS);
    let mut indicator = indicator(fake);
    IndicatorContract::set(&mut indicator, "receiving", NO_PROGRESS).unwrap();
    tick(&mut indicator, 0);
    as_caller(&mut indicator, TOOL).locate(1).unwrap();
    tick(&mut indicator, 100);
    assert_eq!(fake.last(), all(WHITE));
    // The install fails while locate shows.
    IndicatorContract::set(&mut indicator, "failed", NO_PROGRESS).unwrap();
    tick(&mut indicator, 200);
    assert_ne!(fake.last(), all(RED), "locate is still on top");
    settle(&mut indicator, 200, 1_100);
    assert_eq!(
        fake.last(),
        all(RED),
        "the failure shows once locate is over"
    );
}

#[test]
fn locate_is_bounded_and_refused_while_the_lamp_test_shows() {
    let fake = FakePixels::leak(LEDS);
    let mut indicator = indicator(fake);
    tick(&mut indicator, 0);
    assert_eq!(
        indicator.locate(MAX_LOCATE_SECONDS + 1),
        Err(Status::INVALID_ARGUMENT)
    );
    assert_eq!(indicator.locate(MAX_LOCATE_SECONDS), Ok(()));
    indicator.locate(0).unwrap();
    tick(&mut indicator, 10);

    SelfTest::run(&mut indicator, "lamp", true).unwrap();
    tick(&mut indicator, 20);
    assert_eq!(indicator.locate(5), Err(Status::BUSY));
    assert_eq!(indicator.locate(0), Ok(()), "ending one is never refused");
    assert!(!locating(&mut indicator));
}

/// The order of the layers under locate: idle, the application, a state. `failed` stays over the
/// application until another state is set; `done` shows for its time and then is gone.
#[test]
fn a_state_covers_the_application_failed_lasts_and_done_drops_by_itself() {
    let fake = FakePixels::leak(LEDS);
    let mut indicator = driver(
        fake,
        &[
            ("state.failed", ConfigValue::Str("solid #ff0000")),
            ("state.done", ConfigValue::Str("solid #00ff00")),
            ("done_ms", ConfigValue::Int(3_000)),
        ],
    );
    tick(&mut indicator, 0);
    app_shows(&mut indicator, 10).unwrap();
    tick(&mut indicator, 10);
    assert_eq!(fake.last(), frame(10));

    // A failed install: over the application, and it stays there.
    IndicatorContract::set(&mut indicator, "failed", NO_PROGRESS).unwrap();
    assert_eq!(
        tick(&mut indicator, 100),
        None,
        "a solid state asks for nothing"
    );
    assert_eq!(fake.last(), all(RED));
    app_shows(&mut indicator, 20).unwrap();
    tick(&mut indicator, 60_000);
    assert_eq!(
        fake.last(),
        all(RED),
        "a failure is not covered by the application"
    );

    // The next install starts, and ends well: done shows for its three seconds, then the
    // application's latest frame is back with no call from anyone.
    IndicatorContract::set(&mut indicator, "done", NO_PROGRESS).unwrap();
    assert_eq!(
        tick(&mut indicator, 61_000),
        Some(64_000),
        "asks for its end"
    );
    assert_eq!(fake.last(), all(GREEN));
    tick(&mut indicator, 63_999);
    assert_eq!(fake.last(), all(GREEN));
    assert_eq!(tick(&mut indicator, 64_000), None);
    assert_eq!(fake.last(), frame(20));

    // With no application, done drops to idle: dark.
    Lifecycle::caller_gone(&mut indicator, APP);
    IndicatorContract::set(&mut indicator, "done", NO_PROGRESS).unwrap();
    settle(&mut indicator, 70_000, 73_000);
    assert_eq!(fake.last(), all(DARK));
}

/// One application owns the layer. Another is refused, and when the owner goes the layer goes
/// with it: what is under shows, and the layer is free for the next.
#[test]
fn the_application_layer_has_one_owner_and_falls_away_when_it_goes() {
    let fake = FakePixels::leak(LEDS);
    let mut indicator = indicator(fake);
    tick(&mut indicator, 0);
    app_shows(&mut indicator, 10).unwrap();
    tick(&mut indicator, 10);

    let other = as_caller(&mut indicator, OTHER);
    assert_eq!(Pixels::set(other, 0, &[9, 9, 9, 0]), Err(Status::BUSY));
    assert_eq!(Pixels::show(other), Err(Status::BUSY));
    assert_eq!(Pixels::pixel_count(other), Ok(16), "reading is anyone's");
    tick(&mut indicator, 20);
    assert_eq!(fake.last(), frame(10), "the owner's frame is untouched");

    Lifecycle::caller_gone(&mut indicator, APP);
    tick(&mut indicator, 30);
    assert_eq!(
        fake.last(),
        all(DARK),
        "idle shows once the application is gone"
    );

    let other = as_caller(&mut indicator, OTHER);
    Pixels::set(other, 0, &[9, 9, 9, 0]).unwrap();
    Pixels::show(other).unwrap();
    tick(&mut indicator, 40);
    let mut want = all(DARK);
    want[0] = [9, 9, 9, 0];
    assert_eq!(
        fake.last(),
        want,
        "nothing of the last owner's frame is left"
    );
}

/// A frame takes two `set` calls on 16 LEDs. Nothing of it is shown until `show`, whatever ticks
/// come between, so a half-sent frame never reaches the LEDs.
#[test]
fn a_frame_sent_in_two_calls_is_never_shown_half_way() {
    let fake = FakePixels::leak(LEDS);
    let mut indicator = indicator(fake);
    tick(&mut indicator, 0);
    app_shows(&mut indicator, 10).unwrap();
    tick(&mut indicator, 10);
    let shows = fake.shows();

    let next: Vec<u8> = frame(200).concat();
    let app = as_caller(&mut indicator, APP);
    Pixels::set(app, 0, &next[..14 * 4]).unwrap();
    // A state comes and goes between the two halves, which redraws the application's layer.
    IndicatorContract::set(&mut indicator, "failed", NO_PROGRESS).unwrap();
    tick(&mut indicator, 20);
    IndicatorContract::set(&mut indicator, "idle", NO_PROGRESS).unwrap();
    tick(&mut indicator, 30);
    assert_eq!(
        fake.last(),
        frame(10),
        "the whole old frame, none of the new one"
    );
    assert!(fake.shows() > shows, "and it was written again in between");

    let app = as_caller(&mut indicator, APP);
    Pixels::set(app, 14, &next[14 * 4..]).unwrap();
    Pixels::show(app).unwrap();
    tick(&mut indicator, 40);
    assert_eq!(fake.last(), frame(200));
}

/// LEDs past the end are ignored and a partial LED is dropped, as a strip does it.
#[test]
fn the_application_layer_takes_frames_as_a_strip_does() {
    let fake = FakePixels::leak(LEDS);
    let mut indicator = indicator(fake);
    tick(&mut indicator, 0);
    let app = as_caller(&mut indicator, APP);
    // Two LEDs and a half at LED 15: one fits, the rest is past the end.
    Pixels::set(app, 15, &[1, 2, 3, 0, 4, 5, 6, 0, 7, 8]).unwrap();
    Pixels::set(app, 99, &[1, 1, 1, 1]).unwrap();
    Pixels::show(app).unwrap();
    tick(&mut indicator, 10);
    let mut want = all(DARK);
    want[15] = [1, 2, 3, 0];
    assert_eq!(fake.last(), want);
}
