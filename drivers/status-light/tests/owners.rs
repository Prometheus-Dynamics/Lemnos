//! Each LED shows the value of the highest-priority owner that set it. A frame sets the LEDs it
//! names; a look sets all. An owner's layers end when its caller goes, and `clear` empties only the
//! caller's own layer.

#[allow(dead_code)]
mod support;

use lemnos_contract_light::Light;
use lemnos_core::{CallerId, Lifecycle};
use lemnos_driver_status_light::StatusLight;
use support::{FakePixels, driver, fill, led, plain, settle};

const RED: u32 = 0xff0000;
const BLUE: u32 = 0x0000ff;
/// The built-in status.ok at the board's settings: what an LED shows when no owner sets it.
const DEFAULT: [u8; 4] = [0, 178, 0, 0];

fn by(light: &mut StatusLight, caller: u64) {
    <StatusLight as Lifecycle>::caller(light, CallerId(caller));
}

fn gone(light: &mut StatusLight, caller: u64) {
    <StatusLight as Lifecycle>::caller_gone(light, CallerId(caller));
}

/// A frame of `(led, colour)` entries: each is its index (u16) then red, green, blue, white.
fn frame(entries: &[(u16, u32)]) -> Vec<u8> {
    let mut out = Vec::new();
    for &(index, colour) in entries {
        out.extend_from_slice(&index.to_le_bytes());
        out.extend_from_slice(&[(colour >> 16) as u8, (colour >> 8) as u8, colour as u8, 0]);
    }
    out
}

fn rgb(colour: u32) -> [u8; 4] {
    led((colour >> 16) as u8, (colour >> 8) as u8, colour as u8)
}

#[test]
fn two_owners_at_different_priorities_share_the_ring_led_by_led() {
    let fake = FakePixels::leak(4);
    let mut light = driver(fake, &plain(4));
    // Caller 5 owns LEDs 0 to 2 at priority 1; caller 7 owns LEDs 2 and 3 at priority 2.
    by(&mut light, 5);
    light
        .show_frame(0, 1, &frame(&[(0, RED), (1, RED), (2, RED)]))
        .unwrap();
    by(&mut light, 7);
    light
        .show_frame(1, 2, &frame(&[(2, BLUE), (3, BLUE)]))
        .unwrap();
    settle(&mut light, 0, 1_000);
    // LED 2 is the higher owner's; LEDs 0 and 1 are the lower's, where the higher did not set.
    assert_eq!(fake.last(), vec![rgb(RED), rgb(RED), rgb(BLUE), rgb(BLUE)]);
}

#[test]
fn the_higher_owners_caller_going_falls_back_to_the_lower_owner() {
    let fake = FakePixels::leak(4);
    let mut light = driver(fake, &plain(4));
    by(&mut light, 5);
    light
        .show_frame(0, 1, &frame(&[(0, RED), (1, RED), (2, RED)]))
        .unwrap();
    by(&mut light, 7);
    light
        .show_frame(1, 2, &frame(&[(2, BLUE), (3, BLUE)]))
        .unwrap();
    settle(&mut light, 0, 1_000);

    gone(&mut light, 7);
    settle(&mut light, 100, 1_000);
    // LED 2 is caller 5's now; LED 3 is set by nobody, so it shows the default.
    assert_eq!(fake.last(), vec![rgb(RED), rgb(RED), rgb(RED), DEFAULT]);
}

#[test]
fn one_owners_clear_does_not_clear_another_owners_look() {
    let fake = FakePixels::leak(4);
    let mut light = driver(fake, &plain(4));
    by(&mut light, 5);
    light.show(0, 10, &fill(RED)).unwrap();
    settle(&mut light, 0, 1_000);
    assert_eq!(fake.last(), vec![rgb(RED); 4]);

    by(&mut light, 7);
    light.clear(0).unwrap();
    settle(&mut light, 100, 1_000);
    assert_eq!(
        fake.last(),
        vec![rgb(RED); 4],
        "caller 7 cleared caller 5's look"
    );

    by(&mut light, 5);
    light.clear(0).unwrap();
    settle(&mut light, 200, 1_000);
    assert_eq!(
        fake.last(),
        vec![DEFAULT; 4],
        "the owner's own clear empties it"
    );
}

#[test]
fn one_owners_clear_does_not_clear_another_owners_frame() {
    let fake = FakePixels::leak(4);
    let mut light = driver(fake, &plain(4));
    by(&mut light, 5);
    light
        .show_frame(0, 3, &frame(&[(0, RED), (1, RED)]))
        .unwrap();
    by(&mut light, 7);
    light.clear(0).unwrap();
    settle(&mut light, 0, 1_000);
    assert_eq!(fake.last(), vec![rgb(RED), rgb(RED), DEFAULT, DEFAULT]);
}

#[test]
fn a_frame_under_a_higher_look_is_hidden_and_shows_when_the_look_goes() {
    let fake = FakePixels::leak(4);
    let mut light = driver(fake, &plain(4));
    by(&mut light, 5);
    light.show_frame(0, 1, &frame(&[(0, RED)])).unwrap();
    by(&mut light, 7);
    light.show(1, 9, &fill(BLUE)).unwrap();
    settle(&mut light, 0, 1_000);
    assert_eq!(fake.last(), vec![rgb(BLUE); 4]);

    gone(&mut light, 7);
    settle(&mut light, 100, 1_000);
    assert_eq!(fake.last(), vec![rgb(RED), DEFAULT, DEFAULT, DEFAULT]);
}

#[test]
fn a_frame_over_a_lower_look_shows_on_its_leds_and_the_look_elsewhere() {
    let fake = FakePixels::leak(4);
    let mut light = driver(fake, &plain(4));
    by(&mut light, 5);
    light.show(0, 1, &fill(BLUE)).unwrap();
    by(&mut light, 7);
    light.show_frame(1, 2, &frame(&[(3, RED)])).unwrap();
    settle(&mut light, 0, 1_000);
    assert_eq!(fake.last(), vec![rgb(BLUE), rgb(BLUE), rgb(BLUE), rgb(RED)]);
}

#[test]
fn a_frame_replaces_what_its_layer_had_and_an_led_it_leaves_out_shows_the_rest() {
    let fake = FakePixels::leak(4);
    let mut light = driver(fake, &plain(4));
    by(&mut light, 5);
    light
        .show_frame(0, 1, &frame(&[(0, RED), (1, RED)]))
        .unwrap();
    settle(&mut light, 0, 1_000);
    light.show_frame(0, 1, &frame(&[(1, BLUE)])).unwrap();
    settle(&mut light, 100, 1_000);
    assert_eq!(fake.last(), vec![DEFAULT, rgb(BLUE), DEFAULT, DEFAULT]);
}

#[test]
fn a_frame_entry_past_the_ring_is_ignored_and_a_ragged_frame_is_refused() {
    let fake = FakePixels::leak(4);
    let mut light = driver(fake, &plain(4));
    by(&mut light, 5);
    light
        .show_frame(0, 1, &frame(&[(9, RED), (2, RED)]))
        .unwrap();
    settle(&mut light, 0, 1_000);
    assert_eq!(fake.last(), vec![DEFAULT, DEFAULT, rgb(RED), DEFAULT]);

    let mut ragged = frame(&[(0, BLUE)]);
    ragged.push(7);
    assert!(light.show_frame(0, 1, &ragged).is_err());
    settle(&mut light, 200, 1_000);
    assert_eq!(
        fake.last(),
        vec![DEFAULT, DEFAULT, rgb(RED), DEFAULT],
        "a refused frame changes nothing"
    );
}

#[test]
fn a_frame_is_scaled_by_the_runtime_brightness_and_not_by_the_look_brightness() {
    let fake = FakePixels::leak(2);
    let mut light = driver(fake, &plain(2));
    by(&mut light, 5);
    light.show_frame(0, 1, &frame(&[(0, 0xff0000)])).unwrap();
    light.set_brightness(128).unwrap();
    settle(&mut light, 0, 1_000);
    assert_eq!(fake.last()[0][0], 128);
    assert_eq!(fake.last()[0], [128, 0, 0, 0]);
}

#[test]
fn a_frame_on_a_layer_whose_index_is_out_of_range_is_refused() {
    let fake = FakePixels::leak(4);
    let mut light = driver(fake, &plain(4));
    assert!(light.show_frame(4, 1, &frame(&[(0, RED)])).is_err());
}
