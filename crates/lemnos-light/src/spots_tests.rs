//! Spots: where they land between LEDs, wrap round the ring, glide to new
//! inputs the short way, fade in and out, and keep a look's time.

use crate::{
    Animator, Block, LayerSpec, LookSpec, Mode, Rgbw, Spot, SpotList, SpotSource, SpotsSpec,
    Transition,
};

const COUNT: usize = 16;
const RED: Rgbw = Rgbw::rgb(0xff0000);

/// A spots look of `spots` (fixed in the look).
fn spots_look(spots: &[Spot]) -> LookSpec {
    LookSpec::of(LayerSpec::spots(
        RED,
        SpotSource::Fixed(SpotList::new(spots)),
    ))
}

/// Renders `anim` at `t` and returns the ring as it is then (a frame is
/// returned only when it changed, so `last` keeps the ring between them).
fn frame(anim: &mut Animator<COUNT>, t: u64, last: &mut [Rgbw; COUNT]) -> [Rgbw; COUNT] {
    if let Some(f) = anim.render(t) {
        last[..f.len()].copy_from_slice(f);
    }
    *last
}

/// The red of `frame` at `led`.
fn red(frame: &[Rgbw; COUNT], led: usize) -> u8 {
    frame[led].r
}

#[test]
fn a_spot_on_an_led_lights_only_it_and_one_between_two_lights_both() {
    // On LED 8 (position 0.5 of 16): the falloff reaches 0.75 LEDs, so the
    // neighbours at a whole LED away stay dark.
    let mut anim = Animator::<COUNT>::new(COUNT);
    anim.set(spots_look(&[Spot::new(500, 1000)]), Transition::CUT, 0);
    let mut last = [Rgbw::OFF; COUNT];
    let on = frame(&mut anim, 3_000, &mut last);
    assert_eq!(red(&on, 8), 255);
    assert_eq!(red(&on, 7), 0);
    assert_eq!(red(&on, 9), 0);

    // Between LEDs 8 and 9 (position 8.5): both light, the same, and not
    // full; the rest stay dark.
    let mut anim = Animator::<COUNT>::new(COUNT);
    anim.set(spots_look(&[Spot::new(531, 1000)]), Transition::CUT, 0);
    let mut last = [Rgbw::OFF; COUNT];
    let between = frame(&mut anim, 3_000, &mut last);
    // (Just left of the midpoint, so LED 8 is a little brighter.)
    let (a, b) = (red(&between, 8), red(&between, 9));
    assert!(a > 0 && b > 0, "{a} {b}");
    assert!(a.abs_diff(b) <= 6 && a >= b, "{a} {b}");
    assert!(a < 128 && b < 128, "{a} {b}");
    assert_eq!(red(&between, 7), 0);
    assert_eq!(red(&between, 10), 0);
}

#[test]
fn a_spot_across_the_wrap_lights_both_sides_of_led_zero() {
    // Position 0.99 (LED 15.84), 3 LEDs wide: LEDs 15, 0 and 1 are in reach,
    // 14 and 2 are not, and 0 is the brightest of the three.
    let look = LookSpec::of(LayerSpec::new(Block::Spots(SpotsSpec {
        width: 3_000,
        ..SpotsSpec::new(
            RED,
            SpotSource::Fixed(SpotList::new(&[Spot::new(990, 1000)])),
        )
    })));
    let mut anim = Animator::<COUNT>::new(COUNT);
    anim.set(look, Transition::CUT, 0);
    let mut last = [Rgbw::OFF; COUNT];
    let f = frame(&mut anim, 3_000, &mut last);
    assert!(red(&f, 15) > 0 && red(&f, 0) > 0 && red(&f, 1) > 0);
    assert_eq!(red(&f, 14), 0);
    assert_eq!(red(&f, 2), 0);
    assert!(red(&f, 0) > red(&f, 1));
    assert!(red(&f, 0) > red(&f, 15));
}

#[test]
fn spots_ease_to_a_new_position_without_overshoot_and_the_short_way_round() {
    let mut anim = Animator::<COUNT>::new(COUNT);
    let mut last = [Rgbw::OFF; COUNT];
    anim.set(spots_look(&[Spot::new(0, 1000)]), Transition::CUT, 0);
    frame(&mut anim, 1_000, &mut last);
    assert_eq!(red(&last, 0), 255);

    // Moved to LED 4 at 1 s: LED 4 rises, LED 0 falls, monotonically, and
    // neither overshoots.
    anim.set(spots_look(&[Spot::new(250, 1000)]), Transition::CUT, 1_000);
    let (mut prev4, mut prev0) = (0u8, 255u8);
    for t in (1_010..=1_300).step_by(10) {
        let f = frame(&mut anim, t, &mut last);
        assert!(red(&f, 4) >= prev4, "LED 4 fell at {t}");
        assert!(red(&f, 0) <= prev0, "LED 0 rose at {t}");
        prev4 = red(&f, 4);
        prev0 = red(&f, 0);
    }
    // At 40 ms (about half of its 120 ms follow) the spot is about two LEDs
    // along its way: LED 2 is lit, LED 0 behind it is dark, LED 4 not reached.
    let mut mid_anim = Animator::<COUNT>::new(COUNT);
    let mut mid_last = [Rgbw::OFF; COUNT];
    mid_anim.set(spots_look(&[Spot::new(0, 1000)]), Transition::CUT, 0);
    frame(&mut mid_anim, 1_000, &mut mid_last);
    mid_anim.set(spots_look(&[Spot::new(250, 1000)]), Transition::CUT, 1_000);
    let mid = frame(&mut mid_anim, 1_040, &mut mid_last);
    assert!(red(&mid, 2) > 0, "{mid:?}");
    assert_eq!(red(&mid, 0), 0, "{mid:?}");
    assert_eq!(red(&mid, 4), 0, "{mid:?}");
    let f = frame(&mut anim, 1_700, &mut last);
    assert_eq!(red(&f, 4), 255);
    assert_eq!(red(&f, 0), 0);

    // From 0 to 0.938 (LED 15) is 0.062 of a turn back: at 30 ms the spot is
    // on LED 15's side, not LED 1..12, and it ends on LED 15.
    let mut anim = Animator::<COUNT>::new(COUNT);
    let mut last = [Rgbw::OFF; COUNT];
    anim.set(spots_look(&[Spot::new(0, 1000)]), Transition::CUT, 0);
    frame(&mut anim, 1_000, &mut last);
    anim.set(spots_look(&[Spot::new(938, 1000)]), Transition::CUT, 1_000);
    let early = frame(&mut anim, 1_030, &mut last);
    assert!(red(&early, 15) > 0 || red(&early, 0) > 0);
    for led in 1..=12 {
        assert_eq!(red(&early, led), 0, "LED {led} at 30 ms");
    }
    let late = frame(&mut anim, 2_500, &mut last);
    assert!(red(&late, 15) >= 250, "{}", red(&late, 15));
    assert_eq!(red(&late, 14), 0);
    assert_eq!(red(&late, 0), 0);
}

#[test]
fn a_spot_that_appears_fades_in_and_one_dropped_fades_out() {
    let mut anim = Animator::<COUNT>::new(COUNT);
    let mut last = [Rgbw::OFF; COUNT];
    // The block with no spots yet, then the spot: it fades in at its place.
    anim.set(spots_look(&[]), Transition::CUT, 0);
    frame(&mut anim, 0, &mut last);
    anim.set(spots_look(&[Spot::new(500, 1000)]), Transition::CUT, 0);
    let early = frame(&mut anim, 40, &mut last);
    assert!(
        red(&early, 8) > 0 && red(&early, 8) < 255,
        "{}",
        red(&early, 8)
    );
    let full = frame(&mut anim, 1_000, &mut last);
    assert_eq!(red(&full, 8), 255);

    // Dropped from the list: it fades out where it is.
    anim.set(spots_look(&[]), Transition::CUT, 1_000);
    let fading = frame(&mut anim, 1_040, &mut last);
    assert!(
        red(&fading, 8) > 0 && red(&fading, 8) < 255,
        "{}",
        red(&fading, 8)
    );
    let gone = frame(&mut anim, 2_000, &mut last);
    assert_eq!(red(&gone, 8), 0);
}

#[test]
fn an_input_update_keeps_the_comet_phase_and_moves_only_the_spot() {
    // A comet (blue) with a spot on LED 8 (red) over it: an update of the
    // spot's level keeps the comet's turn, so every LED but the spot's is the
    // same as if the look had never changed.
    let comet = |spot_level: u16| {
        let mut look = LookSpec::EMPTY;
        look.push(LayerSpec::comet(Rgbw::rgb(0x0000ff), 1_600, 4_000, 1, 0));
        look.push(LayerSpec::spots(
            RED,
            SpotSource::Fixed(SpotList::new(&[Spot::new(500, spot_level)])),
        ));
        look
    };
    let at = |anim: &mut Animator<COUNT>, t: u64| -> [Rgbw; COUNT] {
        let mut last = [Rgbw::OFF; COUNT];
        frame(anim, t, &mut last)
    };

    // A: the spot at 0.4 from the start.
    let mut a = Animator::<COUNT>::new(COUNT);
    a.set(comet(400), Transition::CUT, 0);
    let reference = at(&mut a, 800);

    // B: the spot at full, then updated to 0.4 at 500 ms.
    let mut b = Animator::<COUNT>::new(COUNT);
    b.set(comet(1_000), Transition::CUT, 0);
    at(&mut b, 500);
    b.set(comet(400), Transition::CUT, 500);
    let updated = at(&mut b, 800);
    for led in (0..COUNT).filter(|&l| l != 8) {
        assert_eq!(updated[led], reference[led], "LED {led}");
    }

    // C: the same look started afresh at 500 ms: its comet is at another turn.
    let mut c = Animator::<COUNT>::new(COUNT);
    c.set(comet(400), Transition::CUT, 500);
    let restarted = at(&mut c, 800);
    assert!(
        (0..COUNT)
            .filter(|&l| l != 8)
            .any(|l| restarted[l] != reference[l]),
        "a restart should move the comet"
    );
}

#[test]
fn the_list_is_capped_at_eight_and_a_spots_look_takes_its_inputs_from_the_request() {
    let nine: [Spot; 9] = core::array::from_fn(|i| Spot::new(i as u16 * 100, 1000));
    let list = SpotList::new(&nine);
    assert_eq!(usize::from(list.len), 8);
    assert_eq!(list.iter().count(), 8);

    let mut look = LookSpec::of(LayerSpec::spots(RED, SpotSource::Input));
    assert!(look.takes_spots());
    look.fill_spots(&list);
    assert!(!look.takes_spots());
    assert_eq!(
        look.spots_block().map(|s| s.source),
        Some(SpotSource::Fixed(list))
    );
    assert_eq!(look.spots_block().map(|s| s.max_spots), Some(8));
    assert_eq!(look.layers[0].map(|l| l.mode), Some(Mode::Max));
}

#[test]
fn spots_validation_names_the_bad_value() {
    let with = |spots: SpotsSpec| LookSpec::of(LayerSpec::new(Block::Spots(spots)));
    let base = SpotsSpec::new(RED, SpotSource::Input);
    assert!(with(base).validate().is_ok());
    let cases = [
        (SpotsSpec { width: 0, ..base }, "width"),
        (
            SpotsSpec {
                follow_ms: 5,
                ..base
            },
            "follow_ms",
        ),
        (
            SpotsSpec {
                max_spots: 9,
                ..base
            },
            "max_spots",
        ),
        (
            SpotsSpec {
                source: SpotSource::Fixed(SpotList {
                    len: 1,
                    spots: [Spot::new(1_001, 1_000); 8],
                }),
                ..base
            },
            "at and level",
        ),
    ];
    for (spots, word) in cases {
        let reason = with(spots).validate().unwrap_err();
        assert!(reason.contains(word), "{reason}");
    }
    let mut two = with(base);
    assert!(two.push(LayerSpec::spots(RED, SpotSource::Input)));
    assert_eq!(two.validate(), Err("a look has at most one spots layer"));
}

#[test]
fn set_says_whether_the_look_changed_or_only_its_inputs_did() {
    let mut anim = Animator::<COUNT>::new(COUNT);
    // A new look is a change; the same spots with a moved spot are not; a
    // new colour is; setting the very same look again counts as a change.
    assert!(anim.set(spots_look(&[Spot::new(0, 1000)]), Transition::CUT, 0));
    assert!(!anim.set(spots_look(&[Spot::new(500, 1000)]), Transition::CUT, 10));
    let recoloured = LookSpec::of(LayerSpec::spots(
        Rgbw::rgb(0x00ff00),
        SpotSource::Fixed(SpotList::new(&[Spot::new(500, 1000)])),
    ));
    assert!(anim.set(recoloured, Transition::CUT, 20));
    assert!(anim.set(recoloured, Transition::CUT, 30));
}
