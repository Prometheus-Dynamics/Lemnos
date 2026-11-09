//! The driver on a fake `pixels/1` device, ticked directly at times the test chooses.

mod support;

use lemnos_contract_light::Light;
use lemnos_core::{ConfigValue, Lifecycle, Status};
use lemnos_driver_status_light::{Envelope, Rgbw, Spec, StatusLook};
use support::{FakePixels, bytes, driver, fill, led, ms, plain, settle, start};

const RED: u32 = 0xff0000;
const BLUE: u32 = 0x0000ff;

/// The ring's LEDs in a `ring` of `count` LEDs all `colour`.
fn all(count: usize, colour: [u8; 4]) -> Vec<[u8; 4]> {
    vec![colour; count]
}

#[test]
fn the_default_look_shows_from_the_start_and_nothing_is_written_before_a_tick() {
    let fake = FakePixels::leak(4);
    let mut light = driver(fake, &plain(4));
    assert_eq!(fake.shows(), 0, "starting writes nothing");

    settle(&mut light, 0, 1_000);
    // status.ok: the board's green at full look brightness.
    assert_eq!(fake.last(), all(4, led(0, 178, 0)));
}

#[test]
fn layout_reports_the_count_and_the_layers() {
    let mut light = driver(FakePixels::leak(4), &plain(4));
    let layout = light.layout().unwrap();
    assert_eq!((layout.pixels, layout.layers), (4, 4));
}

#[test]
fn a_higher_priority_look_shows_over_a_lower_one_and_clearing_it_reveals_the_lower() {
    let fake = FakePixels::leak(4);
    let mut light = driver(fake, &plain(4));
    light.show(0, 10, &fill(BLUE)).unwrap();
    light.show(1, 50, &fill(RED)).unwrap();
    settle(&mut light, 0, 1_000);
    assert_eq!(fake.last(), all(4, led(255, 0, 0)));

    light.clear(1).unwrap();
    settle(&mut light, 100, 1_000);
    assert_eq!(fake.last(), all(4, led(0, 0, 255)));
}

#[test]
fn on_equal_priority_the_higher_layer_shows() {
    let fake = FakePixels::leak(4);
    let mut light = driver(fake, &plain(4));
    light.show(2, 20, &fill(BLUE)).unwrap();
    light.show(1, 20, &fill(RED)).unwrap();
    settle(&mut light, 0, 1_000);
    assert_eq!(fake.last(), all(4, led(0, 0, 255)));
}

#[test]
fn a_built_in_status_is_a_look_on_a_layer_like_any_other() {
    let fake = FakePixels::leak(4);
    // The error status blinks by default; a steady one settles, so the look can be read.
    let mut config = plain(4);
    config.push(("error_effect", ConfigValue::Str("solid")));
    let mut light = driver(fake, &config);
    light
        .show(0, 5, &bytes(Spec::Status(StatusLook::Error)))
        .unwrap();
    settle(&mut light, 0, 1_000);
    // A status is a full fill at the built-in brightness (178), not full brightness.
    assert_eq!(fake.last(), all(4, led(178, 0, 0)));
}

#[test]
fn clear_all_reveals_the_default_look() {
    let fake = FakePixels::leak(4);
    let mut light = driver(fake, &plain(4));
    light.show(0, 90, &fill(RED)).unwrap();
    light.show(3, 99, &fill(BLUE)).unwrap();
    light.clear_all().unwrap();
    settle(&mut light, 0, 1_000);
    assert_eq!(fake.last(), all(4, led(0, 178, 0)));
}

#[test]
fn a_runtime_brightness_scales_the_ring_at_once_and_a_restart_forgets_it() {
    let fake = FakePixels::leak(4);
    let mut light = driver(fake, &plain(4));
    light.show(0, 1, &fill(RED)).unwrap();
    settle(&mut light, 0, 1_000);
    assert_eq!(fake.last()[0][0], 255);

    light.set_brightness(51).unwrap();
    settle(&mut light, 100, 1_000);
    assert_eq!(fake.last()[0][0], 51);

    // A new light on the same board starts from the board's settings: full again.
    let fresh_fake = FakePixels::leak(4);
    let mut fresh = driver(fresh_fake, &plain(4));
    fresh.show(0, 1, &fill(RED)).unwrap();
    settle(&mut fresh, 0, 1_000);
    assert_eq!(fresh_fake.last()[0][0], 255);
}

#[test]
fn a_look_that_is_not_a_look_is_refused_and_the_layers_keep_what_they_had() {
    let fake = FakePixels::leak(4);
    let mut light = driver(fake, &plain(4));
    light.show(0, 1, &fill(RED)).unwrap();

    assert_eq!(light.show(0, 1, &[]), Err(Status::INVALID_ARGUMENT));
    assert_eq!(light.show(0, 1, &[9]), Err(Status::INVALID_ARGUMENT));
    assert_eq!(light.show(0, 1, &[1, 5]), Err(Status::INVALID_ARGUMENT));
    // A fill with bytes over.
    let mut over = fill(BLUE);
    over.push(0);
    assert_eq!(light.show(0, 1, &over), Err(Status::INVALID_ARGUMENT));
    // A layer the light does not have.
    assert_eq!(light.show(4, 1, &fill(BLUE)), Err(Status::INVALID_ARGUMENT));
    assert_eq!(light.clear(4), Err(Status::INVALID_ARGUMENT));

    settle(&mut light, 0, 1_000);
    assert_eq!(fake.last(), all(4, led(255, 0, 0)));
}

#[test]
fn a_look_out_of_range_is_refused() {
    let mut light = driver(FakePixels::leak(4), &plain(4));
    let too_fast = bytes(Spec::Fill {
        color: Rgbw::rgb(RED),
        brightness: 255,
        envelope: Envelope::Blink {
            period_ms: 50,
            duty: 500,
        },
    });
    assert_eq!(light.show(0, 1, &too_fast), Err(Status::INVALID_ARGUMENT));
}

#[test]
fn a_board_must_name_a_count_and_a_status_default() {
    assert!(start(FakePixels::leak(4), &[]).is_err());
    let bad_default = [
        ("count", ConfigValue::Int(4)),
        ("default_look", ConfigValue::Str("status.nope")),
    ];
    assert!(start(FakePixels::leak(4), &bad_default).is_err());
}

#[test]
fn an_idle_ring_asks_for_no_ticks_and_writes_nothing() {
    let fake = FakePixels::leak(4);
    let mut light = driver(fake, &plain(4));
    settle(&mut light, 0, 1_000);
    let writes = fake.show_attempts();

    for t in [500, 2_000, 60_000] {
        assert_eq!(Lifecycle::tick(&mut light, ms(t)), None);
    }
    assert_eq!(fake.show_attempts(), writes, "an idle ring writes nothing");

    // A method call wakes it: the next tick shows the change, and a steady change needs no more
    // ticks after it.
    light.show(0, 1, &fill(RED)).unwrap();
    assert_eq!(Lifecycle::tick(&mut light, ms(60_001)), None);
    assert_eq!(fake.last(), all(4, led(255, 0, 0)));
}

#[test]
fn a_tick_that_comes_early_does_nothing_and_asks_for_the_same_time_again() {
    let fake = FakePixels::leak(4);
    let mut light = driver(fake, &plain(4));
    light
        .show(
            0,
            1,
            &bytes(Spec::Fill {
                color: Rgbw::rgb(RED),
                brightness: 255,
                envelope: Envelope::Blink {
                    period_ms: 1_000,
                    duty: 500,
                },
            }),
        )
        .unwrap();
    let first = Lifecycle::tick(&mut light, ms(0)).expect("a blink animates");
    let writes = fake.show_attempts();

    // Early: nothing is written, and the same time is asked for.
    let early = Lifecycle::tick(&mut light, ms(0) + 1);
    assert_eq!(early, Some(first));
    assert_eq!(fake.show_attempts(), writes);
}

#[test]
fn a_blink_writes_only_when_the_ring_changes() {
    let fake = FakePixels::leak(4);
    let mut light = driver(fake, &plain(4));
    light
        .show(
            0,
            1,
            &bytes(Spec::Fill {
                color: Rgbw::rgb(RED),
                brightness: 255,
                envelope: Envelope::Blink {
                    period_ms: 400,
                    duty: 500,
                },
            }),
        )
        .unwrap();
    let mut now = 0;
    let mut next = Lifecycle::tick(&mut light, ms(now));
    while let Some(at) = next {
        if at > ms(1_000) {
            break;
        }
        now = at / 1_000_000;
        next = Lifecycle::tick(&mut light, ms(now));
    }
    // Over a second of blinking there are on and off frames, and no more shows than frames.
    let shows: Vec<Vec<u8>> = fake.with(|s| s.shows.clone());
    let ons = shows.iter().filter(|f| f[0] == 255).count();
    let offs = shows.iter().filter(|f| f[0] == 0).count();
    assert!(ons > 0 && offs > 0, "on {ons} off {offs}");
    // Each show is a change, so no two shows in a row are equal.
    for pair in shows.windows(2) {
        assert_ne!(pair[0], pair[1]);
    }
}

#[test]
fn a_frame_longer_than_one_local_call_is_sent_in_runs_and_shown_whole() {
    let fake = FakePixels::leak(30);
    let mut light = driver(fake, &plain(30));
    light.show(0, 1, &fill(RED)).unwrap();
    settle(&mut light, 0, 1_000);
    assert_eq!(fake.last(), all(30, led(255, 0, 0)));
    let offsets = fake.with(|s| s.set_offsets.clone());
    assert!(offsets.contains(&0) && offsets.contains(&14) && offsets.contains(&28));
}

#[test]
fn a_failed_write_is_retried_with_a_backoff_capped_at_eight_seconds_and_never_given_up() {
    let fake = FakePixels::leak(4);
    // No fade: a steady look is written once, and the retries are the only later work.
    let mut config = plain(4);
    config.retain(|(key, _)| *key != "fade_ms");
    config.push(("fade_ms", ConfigValue::Int(0)));
    let mut light = driver(fake, &config);
    fake.fail_next_shows(10);
    light.show(0, 1, &fill(RED)).unwrap();

    let mut now = 0;
    let mut gaps = Vec::new();
    for _ in 0..10 {
        let at = Lifecycle::tick(&mut light, ms(now)).expect("a failed write is retried");
        gaps.push(at / 1_000_000 - now);
        now = at / 1_000_000;
        assert!(matches!(
            lemnos_core::Driver::health(&light),
            lemnos_core::Health::Failed(_)
        ));
    }
    assert_eq!(
        gaps,
        [100, 200, 400, 800, 1_600, 3_200, 6_400, 8_000, 8_000, 8_000]
    );
    // The eleventh try goes through: the frame is shown and the light is idle.
    assert_eq!(Lifecycle::tick(&mut light, ms(now)), None);
    assert_eq!(fake.last(), all(4, led(255, 0, 0)));
    assert!(matches!(
        lemnos_core::Driver::health(&light),
        lemnos_core::Health::Ok
    ));
}
