//! A look is owned by the caller that showed it: when that caller goes, its looks end and the
//! light shows the next live look by precedence, or the built-in status. Another caller's looks
//! are untouched.

#[allow(dead_code)]
mod support;

use lemnos_contract_light::Light;
use lemnos_core::{CallerId, ConfigValue, Lifecycle};
use lemnos_driver_status_light::{Spec, StatusLight, StatusLook};
use support::{FakePixels, bytes, driver, fill, led, plain, settle};

const RED: u32 = 0xff0000;
const BLUE: u32 = 0x0000ff;
const GREEN: u32 = 0x00ff00;

fn all(count: usize, colour: [u8; 4]) -> Vec<[u8; 4]> {
    vec![colour; count]
}

fn gone(light: &mut StatusLight, caller: u64) {
    <StatusLight as Lifecycle>::caller_gone(light, CallerId(caller));
}

fn by(light: &mut StatusLight, caller: u64) {
    <StatusLight as Lifecycle>::caller(light, CallerId(caller));
}

#[test]
fn two_callers_one_goes_and_the_other_intent_stays() {
    let fake = FakePixels::leak(4);
    let mut light = driver(fake, &plain(4));
    by(&mut light, 5);
    light.show(0, 200, &fill(RED)).unwrap();
    by(&mut light, 7);
    light.show(1, 50, &fill(BLUE)).unwrap();
    settle(&mut light, 0, 1_000);
    assert_eq!(fake.last(), all(4, led(255, 0, 0)));

    // The higher intent is the one that goes: the lower one, of the other caller, shows.
    gone(&mut light, 5);
    settle(&mut light, 100, 1_000);
    assert_eq!(fake.last(), all(4, led(0, 0, 255)));

    // Caller 5 going again changes nothing; caller 7's intent is still there.
    gone(&mut light, 5);
    settle(&mut light, 200, 1_000);
    assert_eq!(fake.last(), all(4, led(0, 0, 255)));
}

#[test]
fn the_owners_intent_ends_and_the_light_shows_the_built_in_status() {
    let fake = FakePixels::leak(4);
    let mut light = driver(fake, &plain(4));
    by(&mut light, 5);
    light.show(2, 90, &fill(RED)).unwrap();
    settle(&mut light, 0, 1_000);
    assert_eq!(fake.last(), all(4, led(255, 0, 0)));

    gone(&mut light, 5);
    settle(&mut light, 100, 1_000);
    // Nothing else is on a layer: the default look (status.ok, green) shows again.
    assert_eq!(fake.last(), all(4, led(0, 178, 0)));
}

#[test]
fn a_look_another_caller_replaced_is_not_ended_by_the_first_caller_going() {
    let fake = FakePixels::leak(4);
    let mut light = driver(fake, &plain(4));
    by(&mut light, 5);
    light.show(0, 10, &fill(RED)).unwrap();
    // Caller 7 takes the same layer: the layer is now caller 7's.
    by(&mut light, 7);
    light.show(0, 10, &fill(GREEN)).unwrap();
    settle(&mut light, 0, 1_000);

    gone(&mut light, 5);
    settle(&mut light, 100, 1_000);
    assert_eq!(fake.last(), all(4, led(0, 255, 0)));
}

#[test]
fn a_caller_that_showed_nothing_going_changes_nothing() {
    let fake = FakePixels::leak(4);
    let mut light = driver(fake, &plain(4));
    by(&mut light, 7);
    light.show(3, 1, &fill(BLUE)).unwrap();
    settle(&mut light, 0, 1_000);
    gone(&mut light, 9);
    settle(&mut light, 100, 1_000);
    assert_eq!(fake.last(), all(4, led(0, 0, 255)));
}

#[test]
fn a_look_the_host_showed_is_not_ended_by_another_caller_going() {
    let fake = FakePixels::leak(4);
    // The error status blinks by default; a steady one settles, so the look can be read.
    let mut config = plain(4);
    config.push(("error_effect", ConfigValue::Str("solid")));
    let mut light = driver(fake, &config);
    // No `caller` call: the look is the host's own, and it is not any client's to end.
    light
        .show(0, 200, &bytes(Spec::Status(StatusLook::Error)))
        .unwrap();
    gone(&mut light, 7);
    settle(&mut light, 0, 1_000);
    assert_eq!(fake.last()[0][0], 178, "the error look is still shown");
}
