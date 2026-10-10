use super::*;

#[test]
fn easings_start_at_zero_end_at_one_and_stay_monotonic() {
    let curves = [
        Easing::Linear,
        Easing::EaseIn,
        Easing::EaseOut,
        Easing::EaseInOut,
        Easing::Sine,
        Easing::parse("cubic-bezier(0.42, 0, 0.58, 1)").unwrap(),
    ];
    for easing in curves {
        assert_eq!(easing.apply(0), 0, "{easing:?}");
        assert!(easing.apply(ONE) >= ONE - 64, "{easing:?}");
        let mut last = 0;
        for step in 0..=100 {
            let v = easing.apply(ONE * step / 100);
            assert!(v + 64 >= last, "{easing:?} at {step}");
            last = v;
        }
    }
    // Ease-in-out and sine are symmetric about the middle.
    for easing in [Easing::EaseInOut, Easing::Sine] {
        assert!(easing.apply(ONE / 2).abs_diff(ONE / 2) < 256, "{easing:?}");
    }
    assert!(Easing::EaseIn.apply(ONE / 4) < ONE / 4);
    assert!(Easing::EaseOut.apply(ONE / 4) > ONE / 4);
    assert_eq!(Easing::parse("ease-in-out"), Some(Easing::EaseInOut));
    assert_eq!(Easing::parse("cubic-bezier(1.5, 0, 0, 1)"), None);
}

#[test]
fn breathe_eases_between_full_and_depth_and_blink_is_hard() {
    let breathe = Effect::Breathe {
        period_ms: 1000,
        depth: 600,
        easing: Easing::Sine,
    };
    assert_eq!(breathe.level(0), ONE);
    let bottom = breathe.level(500);
    assert!(bottom.abs_diff(ONE * 4 / 10) < 64, "{bottom}");
    let quarter = breathe.level(250);
    assert!(quarter > bottom && quarter < ONE);
    let blink = Effect::Blink {
        period_ms: 100,
        duty: 300,
    };
    assert_eq!((blink.level(10), blink.level(40)), (ONE, 0));
}

#[test]
fn animator_fades_and_writes_only_while_moving() {
    let mut a: Animator<4> = Animator::new(3);
    let red = Look::fill(Rgbw::rgb(0xff0000));
    a.set(red, Transition::new(100, Easing::Linear), 0);
    let first = a.render(0).map(|f| f.to_vec_len());
    assert_eq!(first, Some(3));
    let mid = a.render(50).unwrap()[0];
    assert!(mid.r > 100 && mid.r < 155, "{mid:?}");
    assert_eq!(a.render(100).unwrap()[0], Rgbw::rgb(0xff0000));
    // Steady: nothing to write, no deadline.
    assert!(a.render(120).is_none());
    assert_eq!(a.next_frame_ms(120), None);
    // A brightness change fades too.
    a.set(
        red.with_brightness(0),
        Transition::new(100, Easing::EaseInOut),
        200,
    );
    assert_eq!(a.next_frame_ms(200), Some(220));
    let half = a.render(250).unwrap()[1];
    assert!(half.r > 100 && half.r < 155);
    assert_eq!(a.render(300).unwrap()[2], Rgbw::OFF);
}

trait Len {
    fn to_vec_len(&self) -> usize;
}

impl Len for [Rgbw] {
    fn to_vec_len(&self) -> usize {
        self.len()
    }
}

#[test]
fn arbiter_picks_locate_then_status_then_app() {
    let mut arb: Arbiter<4, 8> = Arbiter::new();
    assert!(arb.winner().is_none());
    arb.hold(1, 50, Intent::new(Show::Color(Rgbw::rgb(0x0000ff))), None);
    arb.hold(2, 10, Intent::new(Show::Status(Status::Warn)), None);
    assert_eq!(arb.winner().unwrap().owner, 2);
    arb.hold(3, 0, Intent::new(Show::Locate), Some(1000));
    assert_eq!(arb.winner().unwrap().layer, Layer::Locate);
    assert_eq!(arb.next_expiry(), Some(1000));
    assert!(arb.expire(1000));
    assert_eq!(arb.winner().unwrap().owner, 2);
    // Same layer: higher priority, then most recent.
    arb.hold(4, 10, Intent::new(Show::Status(Status::Ok)), None);
    assert_eq!(arb.winner().unwrap().owner, 4);
    arb.hold(5, 90, Intent::new(Show::Status(Status::Error)), None);
    assert_eq!(arb.winner().unwrap().owner, 5);
    arb.clear(5, None);
    arb.clear(4, Some(Layer::Status));
    assert_eq!(arb.winner().unwrap().owner, 2);
    let mut alert = Intent::new(Show::Status(Status::Error));
    alert.alert = true;
    arb.hold(0, 0, alert, None);
    assert_eq!(arb.winner().unwrap().layer, Layer::Alert);
    assert_eq!(arb.len(), 3);
}

#[test]
fn intents_take_board_defaults_unless_overridden() {
    let defaults = Defaults {
        status_effect: EffectKind::Breathe,
        fade_ms: 250,
        ..Defaults::default()
    };
    let (look, fade) = Intent::<4>::new(Show::Status(Status::Ok)).resolve(&defaults);
    assert!(matches!(
        look.effect,
        Effect::Breathe {
            period_ms: 2000,
            depth: 600,
            ..
        }
    ));
    assert_eq!(fade.duration_ms, 250);
    let mut solid = Intent::<4>::new(Show::Status(Status::Ok));
    solid.effect = Some(EffectKind::Solid);
    solid.fade_ms = Some(0);
    solid.brightness = Some(128);
    let (look, fade) = solid.resolve(&defaults);
    assert_eq!(
        (look.effect, fade.duration_ms, look.brightness),
        (Effect::Solid, 0, 128)
    );
    let (look, _) = Intent::<4>::new(Show::Status(Status::Error)).resolve(&defaults);
    assert!(matches!(look.effect, Effect::Blink { .. }));
}

#[test]
fn progress_fills_with_a_partial_leading_led_and_advances_eased() {
    let green = Rgbw::rgb(0x00ff00);
    let mut a: Animator<16> = Animator::new(16);
    // 3.5 LEDs of 16: three full, the fourth half lit (the head, whitened).
    let fraction = ONE * 35 / 160;
    a.set(
        Look::progress(fraction, green, Rgbw::OFF),
        Transition::CUT,
        0,
    );
    let frame = a.render(0).unwrap();
    // The full LED keeps its colour (the sheen only adds a little white).
    assert_eq!(frame[2].g, 255);
    assert!(frame[2].r < 20, "{:?}", frame[2]);
    // The half LED is at 62.5% and whitened: its red shows.
    assert!(frame[3].g > 150 && frame[3].g < 175, "{:?}", frame[3]);
    assert!(frame[3].r > 45 && frame[3].r < 70, "{:?}", frame[3]);
    assert_eq!(frame[4], Rgbw::OFF);
    // A new fraction advances (the fifth LED fills in over the fade).
    a.set(
        Look::progress(ONE * 5 / 16, green, Rgbw::OFF),
        Transition::new(100, Easing::Linear),
        0,
    );
    let mid = a.render(50).unwrap();
    assert!(mid[3].g > 200, "{:?}", mid[3]);
    assert!(mid[4].g < 150, "{:?}", mid[4]);
    let end = a.render(100).unwrap();
    // The new leading LED (4) is the full head now.
    assert_eq!(end[4].g, 255);
    assert_eq!(end[5], Rgbw::OFF);
    // A gauge's sheen keeps it moving.
    assert!(a.is_animating(150));
}

fn frame16(frame: &[Rgbw]) -> [Rgbw; 16] {
    let mut out = [Rgbw::OFF; 16];
    out[..frame.len()].copy_from_slice(frame);
    out
}

/// One Q16 brightness from a thousandths value.
fn permille_q16(thousandths: u32) -> u32 {
    (thousandths << 16) / 1000
}

#[test]
fn comet_moves_its_head_round_with_a_tail() {
    let mut a: Animator<16> = Animator::new(16);
    // 1.6 s a turn, a 3-LED tail, no floor.
    a.set(
        Look::comet(Rgbw::rgb(0xffffff), 1600, 3 << 16, 1, 0),
        Transition::CUT,
        0,
    );
    let at0 = frame16(a.render(0).unwrap());
    assert_eq!(at0[0].r, 255);
    // One LED behind the head: (1 - 1/3)^2.2 of full.
    assert!(at0[15].r > 90 && at0[15].r < 110, "{:?}", at0[15]);
    // Outside the tail: dark.
    assert_eq!(at0[13].r, 0);
    // A quarter turn on: the head is at LED 4.
    let at400 = frame16(a.render(400).unwrap());
    assert_eq!(at400[4].r, 255);
    assert!(at400[3].r > 0 && at400[3].r < 255);
    assert_eq!(at400[0].r, 0);
    // Sub-LED motion: half an LED on (50 ms of 1.6 s is 0.5 LED), the head
    // sits between LEDs 0 and 1: LED 0 is part lit and LED 1 is not.
    let half = frame16(a.render(50).unwrap());
    // (1 - 0.5/3)^2.2 of full.
    assert!(half[0].r > 165 && half[0].r < 175, "{:?}", half[0]);
    assert_eq!(half[1].r, 0);
    assert_eq!(a.next_frame_ms(400), Some(420));
}

#[test]
fn twin_comets_sit_opposite_and_a_floor_keeps_the_ring_lit() {
    let mut a: Animator<16> = Animator::new(16);
    a.set(
        Look::comet(Rgbw::rgb(0xffffff), 1000, 2 << 16, 2, permille_q16(50)),
        Transition::CUT,
        0,
    );
    let f = frame16(a.render(0).unwrap());
    assert_eq!(f[0].r, 255);
    assert_eq!(f[8].r, 255);
    // The floor (5%) away from the heads, and never darker than it.
    assert!(f[4].r.abs_diff(13) <= 1, "{:?}", f[4]);
    assert!(f.iter().all(|p| p.r >= 12));
    // A heads-2 comet at a quarter turn: the heads are at LEDs 4 and 12.
    let q = frame16(a.render(250).unwrap());
    assert_eq!(q[4].r, 255);
    assert_eq!(q[12].r, 255);
    assert_eq!(q[0].r, 13);
}

#[test]
fn arc_fills_with_a_bright_head_over_a_faint_track() {
    let blue = Rgbw::rgb(0x2f7bff);
    let track = Rgbw::rgb(0x101012);
    let mut a: Animator<16> = Animator::new(16);
    // 3.5 of 16 LEDs lit.
    a.set(
        Look::progress(ONE * 35 / 160, blue, track),
        Transition::CUT,
        0,
    );
    let f = frame16(a.render(0).unwrap());
    // Unfilled: the track itself, neither dim blue nor black.
    assert_eq!(f[8], track);
    assert_eq!(f[15], track);
    // The leading LED (3, half lit) is whitened: its red is far above the
    // blue's red, and it is dimmer than the full LED 2 in blue.
    assert!(f[3].r > 60 && f[3].r < 90, "{:?}", f[3]);
    assert!(f[3].r > f[2].r, "{:?} {:?}", f[2], f[3]);
    assert!(f[2].b > 240 && f[3].b < 180, "{:?} {:?}", f[2], f[3]);
    // The sheen travels: at a full arc the lit LED 0 is brighter at the
    // start than LED 4, which the sheen has not reached.
    let mut full: Animator<16> = Animator::new(16);
    full.set(Look::progress(ONE, blue, track), Transition::CUT, 0);
    let g = frame16(full.render(0).unwrap());
    assert!(g[0].r > g[4].r + 40, "{:?} {:?}", g[0], g[4]);
    assert!(full.is_animating(0));
}

#[test]
fn ripple_runs_down_from_the_top_and_settles() {
    let green = Rgbw::rgb(0x00ff00);
    let mut a: Animator<16> = Animator::new(16);
    a.set(Look::ripple(green), Transition::CUT, 0);
    let at0 = frame16(a.render(0).unwrap());
    assert_eq!(at0[0].g, 255);
    // Only the settling glow (15%) at the bottom.
    assert!(at0[8].g > 30 && at0[8].g < 50, "{:?}", at0[8]);
    // 12 LEDs a second: the front is 6 LEDs down, on both sides.
    let at500 = frame16(a.render(500).unwrap());
    assert_eq!(at500[6].g, 255);
    assert_eq!(at500[10].g, 255);
    assert!(at500[0].g < 40, "{:?}", at500[0]);
    // Past the bottom, the glow is fading; then dark.
    let at1000 = frame16(a.render(1000).unwrap());
    assert!(at1000.iter().all(|p| p.g < 20), "{at1000:?}");
    let at1500 = frame16(a.render(1500).unwrap());
    assert!(at1500.iter().all(|p| p.g == 0));
    assert!(a.render(1600).is_none());
}

#[test]
fn confirmed_is_a_ripple_and_a_reboot_holds_a_static_ember() {
    let defaults = Defaults::default();
    let (look, _) = Intent::<16>::new(Show::System(SystemState::Confirmed)).resolve(&defaults);
    assert_eq!(
        look.pixels,
        Pixels::Ripple {
            color: defaults.confirmed
        }
    );
    assert_eq!(SystemState::Confirmed.name(), "confirmed");

    let (look, _) = Intent::<16>::new(Show::System(SystemState::Rebooting)).resolve(&defaults);
    assert_eq!(look.pixels, Pixels::Fill(defaults.rebooting.scaled(EMBER)));
    assert_eq!(look.effect, Effect::Solid);
    assert!(!look.is_moving());
    // 12% of the amber: 31 of 255 on the red channel.
    assert_eq!(EMBER, 31);
    let mut a: Animator<16> = Animator::new(16);
    a.set(look, Transition::new(1200, Easing::EaseInOut), 0);
    a.render(0);
    let end = frame16(a.render(1200).unwrap());
    assert_eq!(end[0], defaults.rebooting.scaled(EMBER));
    assert_eq!(end[0].r, 31);
    assert!(a.render(1300).is_none());
    assert_eq!(a.next_frame_ms(1300), None);
}

#[test]
fn switching_from_a_breathe_to_an_orbit_fades_from_what_is_shown() {
    let mut a: Animator<16> = Animator::new(16);
    a.set(
        Look::fill(Rgbw::rgb(0x00ff00)).with_effect(Effect::Breathe {
            period_ms: 2000,
            depth: 600,
            easing: Easing::EaseInOut,
        }),
        Transition::CUT,
        0,
    );
    // Half a period: the breathe's trough.
    let before = frame16(a.render(1_000).unwrap());
    let orbit = Look::comet(Rgbw::rgb(0x8a5cff), 1200, 7 << 16, 1, permille_q16(50));
    a.set(orbit, Transition::new(250, Easing::EaseInOut), 1_000);
    // The first frame of the fade is what was on the ring: no pop.
    let first = frame16(a.render(1_000).unwrap());
    assert_eq!(first, before);
    // Then it moves toward the comet over the fade, not in one step.
    let mid = frame16(a.render(1_125).unwrap());
    assert_ne!(mid, before);
    assert!(a.is_animating(1_125));
    assert_eq!(a.next_frame_ms(1_000), Some(1_020));
}

#[test]
fn orbit_intents_take_their_shape_from_the_intent_and_the_defaults() {
    let defaults = Defaults::default();
    let green = Rgbw::rgb(0x00ff00);
    let orbit = Show::Orbit {
        color: Some(green),
        tail: Some(6_000),
        heads: 1,
        base: Some(60),
    };
    let intent = Intent::<16>::new(orbit);
    assert_eq!(intent.layer(), Layer::App);
    let (look, _) = intent.resolve(&defaults);
    assert_eq!(
        look.pixels,
        Pixels::Comet {
            color: green,
            period_ms: 1_200,
            tail: 6 << 16,
            heads: 1,
            base: permille_q16(60),
        }
    );
    let mut timed = Intent::<16>::new(orbit);
    timed.period_ms = Some(1_600);
    let (look, _) = timed.resolve(&defaults);
    assert!(matches!(
        look.pixels,
        Pixels::Comet {
            period_ms: 1_600,
            ..
        }
    ));
    // The defaults: the progress colour, one head, no floor.
    let (look, _) = Intent::<16>::new(Show::Orbit {
        color: None,
        tail: None,
        heads: 2,
        base: None,
    })
    .resolve(&defaults);
    assert!(matches!(
        look.pixels,
        Pixels::Comet { color, tail, heads: 2, base: 0, .. }
            if color == defaults.progress && tail == 5 << 16
    ));
}

#[test]
fn system_looks_match_the_update_states() {
    let defaults = Defaults::default();
    let resolve = |state| Intent::<16>::new(Show::System(state)).resolve(&defaults).0;
    // Unknown amount: the purple comet, 7 LEDs, 5% floor.
    let verifying = resolve(SystemState::Updating {
        progress: None,
        phase: Phase::Verifying,
    });
    assert_eq!(
        verifying.pixels,
        Pixels::Comet {
            color: Rgbw::rgb(0x8a5cff),
            period_ms: 1_200,
            tail: 7 << 16,
            heads: 1,
            base: permille_q16(50),
        }
    );
    // Staged: the green breathe, 2.2 s, down to 55%.
    let staged = resolve(SystemState::Updating {
        progress: Some(1000),
        phase: Phase::Staged,
    });
    assert_eq!(staged.pixels, Pixels::Fill(Rgbw::rgb(0x2bd47d)));
    assert_eq!(
        staged.effect,
        Effect::Breathe {
            period_ms: 2_200,
            depth: 450,
            easing: Easing::EaseInOut,
        }
    );
    // Trial boot: the twin comet, 1.8 s, 5 LEDs, 4% floor.
    let trying = resolve(SystemState::Booting);
    assert_eq!(
        trying.pixels,
        Pixels::Comet {
            color: Rgbw::rgb(0xfff4e6),
            period_ms: 1_800,
            tail: 5 << 16,
            heads: 2,
            base: permille_q16(40),
        }
    );
    // Failed: a red breathe, 2.4 s, down to 10%.
    let failed = resolve(SystemState::UpdateFailed);
    assert_eq!(failed.pixels, Pixels::Fill(Rgbw::rgb(0xff3b3b)));
    assert_eq!(
        failed.effect,
        Effect::Breathe {
            period_ms: 2_400,
            depth: 900,
            easing: Easing::EaseInOut,
        }
    );
}

#[test]
fn system_states_use_board_colours() {
    let defaults = Defaults::default();
    let updating = Intent::<16>::new(Show::System(SystemState::Updating {
        progress: Some(500),
        phase: Phase::Writing,
    }));
    assert_eq!(updating.layer(), Layer::System);
    let (look, _) = updating.resolve(&defaults);
    assert!(matches!(
        look.pixels,
        crate::Pixels::Progress { color, .. } if color == defaults.updating
    ));
    let (look, _) = Intent::<16>::new(Show::System(SystemState::Booting)).resolve(&defaults);
    assert!(look.is_moving());
    let (look, _) = Intent::<16>::new(Show::System(SystemState::RolledBack)).resolve(&defaults);
    assert!(matches!(look.effect, Effect::Breathe { .. }));
    let chase = Defaults {
        locate_effect: EffectKind::Chase,
        ..defaults
    };
    let (look, _) = Intent::<16>::new(Show::Locate).resolve(&chase);
    assert!(look.is_moving());
    let (look, _) = Intent::<16>::new(Show::Indeterminate { color: None }).resolve(&defaults);
    assert!(look.is_moving());
}

#[test]
fn test_layer_sits_between_status_and_alert() {
    let mut arb: Arbiter<4, 8> = Arbiter::new();
    arb.hold(1, 200, Intent::new(Show::Status(Status::Error)), None);
    let mut test = Intent::new(Show::Color(Rgbw::rgb(0x00ff00)));
    test.test = true;
    assert_eq!(test.layer(), Layer::Test);
    // A low-priority test beats a high-priority status.
    arb.hold(2, 0, test, Some(1_000));
    assert_eq!(arb.winner().unwrap().owner, 2);
    assert_eq!(arb.winner().unwrap().layer.name(), "test");
    // A host alert beats the test.
    let mut alert = Intent::new(Show::Status(Status::Warn));
    alert.alert = true;
    alert.test = true;
    assert_eq!(alert.layer(), Layer::Alert);
    arb.hold(3, 0, alert, None);
    assert_eq!(arb.winner().unwrap().owner, 3);
    arb.clear(3, None);
    // The test's lease runs out: back to the status below.
    assert!(arb.expire(1_000));
    assert_eq!(arb.winner().unwrap().owner, 1);
    assert!(Layer::Status < Layer::Test && Layer::Test < Layer::Alert);
}
