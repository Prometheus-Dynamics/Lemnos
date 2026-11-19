//! `current` is the composed ring: a generation that goes up with each change, and for each LED its
//! colour and the owner and priority that set it. Reading needs no ownership.

#[allow(dead_code)]
mod support;

use lemnos_contract_light::Light;
use lemnos_core::{CallerId, Lifecycle, Status};
use lemnos_driver_status_light::StatusLight;
use support::{FakePixels, driver, ms, plain, settle};

const DEFAULT: [u8; 4] = [0, 178, 0, 0];

/// One LED as `current` reports it.
#[derive(Debug, PartialEq, Eq)]
struct Seen {
    colour: [u8; 4],
    owner: Option<u64>,
    priority: u8,
}

fn by(light: &mut StatusLight, caller: u64) {
    <StatusLight as Lifecycle>::caller(light, CallerId(caller));
}

fn tick(light: &mut StatusLight, now_ms: u64) {
    <StatusLight as Lifecycle>::tick(light, ms(now_ms));
}

/// A frame of `(led, [r, g, b, w])` entries.
fn frame(entries: &[(u16, [u8; 4])]) -> Vec<u8> {
    let mut out = Vec::new();
    for (index, c) in entries {
        out.extend_from_slice(&index.to_le_bytes());
        out.extend_from_slice(c);
    }
    out
}

/// Reads `current`: the generation, then each LED.
fn current(light: &mut StatusLight) -> (u64, Vec<Seen>) {
    let count = <StatusLight as Light>::layout(light).unwrap().pixels as usize;
    let mut buf = vec![0u8; 10 + 14 * count];
    let n = light.current(&mut buf).unwrap();
    assert_eq!(n, buf.len());
    let generation = u64::from_le_bytes(buf[..8].try_into().unwrap());
    assert_eq!(u16::from_le_bytes([buf[8], buf[9]]) as usize, count);
    let leds = buf[10..]
        .as_chunks::<14>()
        .0
        .iter()
        .map(|e| Seen {
            colour: [e[0], e[1], e[2], e[3]],
            owner: (e[4] == 1).then(|| u64::from_le_bytes(e[6..14].try_into().unwrap())),
            priority: e[5],
        })
        .collect();
    (generation, leds)
}

#[test]
fn each_led_reports_the_owner_and_priority_that_won_it() {
    let fake = FakePixels::leak(4);
    let mut light = driver(fake, &plain(4));
    by(&mut light, 5);
    light
        .show_frame(
            0,
            1,
            &frame(&[
                (0, [255, 0, 0, 0]),
                (1, [255, 0, 0, 0]),
                (2, [255, 0, 0, 0]),
            ]),
        )
        .unwrap();
    by(&mut light, 7);
    light
        .show_frame(1, 2, &frame(&[(2, [0, 0, 255, 0]), (3, [0, 0, 255, 0])]))
        .unwrap();
    settle(&mut light, 0, 1_000);

    let (_, leds) = current(&mut light);
    assert_eq!(
        leds,
        vec![
            Seen {
                colour: [255, 0, 0, 0],
                owner: Some(5),
                priority: 1
            },
            Seen {
                colour: [255, 0, 0, 0],
                owner: Some(5),
                priority: 1
            },
            Seen {
                colour: [0, 0, 255, 0],
                owner: Some(7),
                priority: 2
            },
            Seen {
                colour: [0, 0, 255, 0],
                owner: Some(7),
                priority: 2
            },
        ]
    );
}

#[test]
fn an_led_nobody_set_reports_no_owner_and_the_default_colour() {
    let fake = FakePixels::leak(2);
    let mut light = driver(fake, &plain(2));
    settle(&mut light, 0, 1_000);
    let (_, leds) = current(&mut light);
    assert!(
        leds.iter()
            .all(|l| l.owner.is_none() && l.colour == DEFAULT)
    );
}

#[test]
fn the_generation_goes_up_with_each_change_and_not_on_a_quiet_tick() {
    let fake = FakePixels::leak(2);
    let mut light = driver(fake, &plain(2));
    settle(&mut light, 0, 1_000);
    let (before, _) = current(&mut light);
    assert!(before > 0);

    // A tick with nothing changed, and a read, change nothing.
    tick(&mut light, 500);
    assert_eq!(current(&mut light).0, before);

    by(&mut light, 5);
    light
        .show_frame(0, 1, &frame(&[(0, [9, 0, 0, 0])]))
        .unwrap();
    settle(&mut light, 600, 1_000);
    assert!(current(&mut light).0 > before);
}

#[test]
fn reading_needs_no_ownership_and_makes_the_reader_no_owner() {
    let fake = FakePixels::leak(2);
    let mut light = driver(fake, &plain(2));
    by(&mut light, 5);
    light
        .show_frame(0, 1, &frame(&[(0, [1, 2, 3, 0])]))
        .unwrap();
    settle(&mut light, 0, 1_000);
    by(&mut light, 9);
    let (_, leds) = current(&mut light);
    assert_eq!(leds[0].owner, Some(5), "the owner is still caller 5");
    assert_eq!(leds[1].owner, None);
}

#[test]
fn a_buffer_too_small_for_the_ring_is_refused() {
    let fake = FakePixels::leak(2);
    let mut light = driver(fake, &plain(2));
    let mut small = vec![0u8; 10 + 14 * 2 - 1];
    assert_eq!(light.current(&mut small), Err(Status::BUFFER_TOO_SMALL));
}

#[test]
fn a_burst_faster_than_a_reader_samples_gives_its_newest_frame_and_a_gap_in_the_generation() {
    let fake = FakePixels::leak(2);
    let mut light = driver(fake, &plain(2));
    by(&mut light, 5);
    // 100 frames, one per millisecond. A reader samples every 20 ms, as the daemon's cap allows.
    let mut samples = Vec::new();
    for k in 0..100u64 {
        let colour = [k as u8, 0, 0, 0];
        light.show_frame(0, 1, &frame(&[(0, colour)])).unwrap();
        tick(&mut light, k);
        if k % 20 == 19 {
            let (generation, leds) = current(&mut light);
            samples.push((generation, leds[0].colour));
        }
    }
    assert_eq!(samples.len(), 5);
    // The last sample is the final frame, and the generation skipped the frames in between.
    assert_eq!(samples.last().unwrap().1, [99, 0, 0, 0]);
    let gaps: Vec<u64> = samples.windows(2).map(|w| w[1].0 - w[0].0).collect();
    assert!(gaps.iter().all(|&g| g >= 1));
    assert!(
        gaps.iter().any(|&g| g > 1),
        "the generation shows the frames a reader did not see: {gaps:?}"
    );
}
