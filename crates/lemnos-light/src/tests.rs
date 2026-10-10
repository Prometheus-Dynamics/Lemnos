use super::*;

/// The first layer's block.
fn block(look: &LookSpec) -> Block {
    look.layers[0].expect("a layer").block
}

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
    let red = LookSpec::fill(Rgbw::rgb(0xff0000));
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
    let (look, fade) = Intent::<4>::new(Show::Status(Status::Ok)).resolve_builtin(&defaults);
    assert!(matches!(
        look.envelope,
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
    let (look, fade) = solid.resolve_builtin(&defaults);
    // The request's 128 is half of full, and the ring-wide scale is half
    // again.
    assert_eq!(
        (look.envelope, fade.duration_ms, look.brightness),
        (Effect::Solid, 0, 64)
    );
    let (look, _) = Intent::<4>::new(Show::Status(Status::Error)).resolve_builtin(&defaults);
    assert!(matches!(look.envelope, Effect::Blink { .. }));
}

#[test]
fn progress_fills_with_a_partial_leading_led_and_advances_eased() {
    let green = Rgbw::rgb(0x00ff00);
    let mut a: Animator<16> = Animator::new(16);
    // 3.5 LEDs of 16: three full, the fourth half lit (the head, whitened).
    let fraction = 219;
    a.set(
        LookSpec::progress(fraction, green, Rgbw::OFF),
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
        LookSpec::progress(313, green, Rgbw::OFF),
        Transition::new(100, Easing::Linear),
        0,
    );
    let mid = a.render(50).unwrap();
    assert!(mid[3].g > 200, "{:?}", mid[3]);
    assert!(mid[4].g < 150, "{:?}", mid[4]);
    let end = a.render(100).unwrap();
    // The new leading LED (4) is the full head now.
    assert_eq!(end[4].g, 255);
    // Just past 5/16: LED 5 is only just lit, at its 25% shade.
    assert!(end[5].g > 40 && end[5].g < 80, "{:?}", end[5]);
    assert_eq!(end[6], Rgbw::OFF);
    // A gauge's sheen keeps it moving.
    assert!(a.is_animating(150));
}

fn frame16(frame: &[Rgbw]) -> [Rgbw; 16] {
    let mut out = [Rgbw::OFF; 16];
    out[..frame.len()].copy_from_slice(frame);
    out
}

#[test]
fn comet_moves_its_head_round_with_a_tail() {
    let mut a: Animator<16> = Animator::new(16);
    // 1.6 s a turn, a 3-LED tail, no floor.
    a.set(
        LookSpec::comet(Rgbw::rgb(0xffffff), 1600, 3000, 1, 0),
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
    // sits between LEDs 0 and 1: LED 0 is part lit, and LED 1 (the leading
    // edge) is lit by the half that the head has reached.
    let half = frame16(a.render(50).unwrap());
    // (1 - 0.5/3)^2.2 of full.
    assert!(half[0].r > 165 && half[0].r < 175, "{:?}", half[0]);
    assert!(half[1].r.abs_diff(128) <= 2, "{:?}", half[1]);
    assert_eq!(a.next_frame_ms(400), Some(420));
}

#[test]
fn twin_comets_sit_opposite_and_a_floor_keeps_the_ring_lit() {
    let mut a: Animator<16> = Animator::new(16);
    a.set(
        LookSpec::comet(Rgbw::rgb(0xffffff), 1000, 2000, 2, 50),
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
    a.set(LookSpec::progress(219, blue, track), Transition::CUT, 0);
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
    full.set(LookSpec::progress(1000, blue, track), Transition::CUT, 0);
    let g = frame16(full.render(0).unwrap());
    assert!(g[0].r > g[4].r + 40, "{:?} {:?}", g[0], g[4]);
    assert!(full.is_animating(0));
}

#[test]
fn ripple_runs_down_from_the_top_and_settles() {
    let green = Rgbw::rgb(0x00ff00);
    let mut a: Animator<16> = Animator::new(16);
    a.set(LookSpec::ripple(green), Transition::CUT, 0);
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
    let (look, _) =
        Intent::<16>::new(Show::System(SystemState::Confirmed)).resolve_builtin(&defaults);
    // A flash of the whole ring (a wash shaped by its own pulse), a ripple
    // burst, and the green drain over both: filled from the top, then drained
    // toward the bottom.
    assert!(matches!(
        block(&look),
        Block::Wash { color, effect: Effect::Pulse { attack_ms: 80, repeat: 1, .. } }
            if color == Rgbw::rgb(0xc8ffd2)
    ));
    assert_eq!(look.envelope, Effect::Solid);
    assert!(matches!(
        look.layers[1].expect("a drain").block,
        Block::Drain { color, fill_ms: 700, start_ms: 1_100, duration_ms: 2_100, easing: Easing::EaseIn }
            if color == defaults.confirmed
    ));
    assert!(matches!(
        look.layers[2].expect("a ripple").block,
        Block::Ripple { color, glow: 0, .. } if color == defaults.confirmed
    ));
    assert!(look.wants_bottom());
    assert_eq!(SystemState::Confirmed.name(), "confirmed");

    let (look, _) =
        Intent::<16>::new(Show::System(SystemState::Rebooting)).resolve_builtin(&defaults);
    assert!(matches!(
        block(&look),
        Block::Fill { color } if color == defaults.rebooting
    ));
    assert_eq!(look.envelope, Effect::Solid);
    assert!(!look.is_moving());
    // 44% of the amber: 112 of 255, about 22% on a default (half) ring.
    assert_eq!(EMBER, 112);
    let mut a: Animator<16> = Animator::new(16);
    a.set(look, Transition::new(1200, Easing::EaseInOut), 0);
    a.render(0);
    let end = frame16(a.render(1200).unwrap());
    // At the default ring-wide brightness (half) the ember is about 22%:
    // 56 of 255 on the red channel.
    assert_eq!(end[0], Rgbw::rgb(0xff8000).scaled(56));
    assert_eq!(end[0].r, 56);
    assert!(a.render(1300).is_none());
    assert_eq!(a.next_frame_ms(1300), None);
}

#[test]
fn switching_from_a_breathe_to_an_orbit_fades_from_what_is_shown() {
    let mut a: Animator<16> = Animator::new(16);
    a.set(
        LookSpec::fill(Rgbw::rgb(0x00ff00)).with_envelope(Effect::Breathe {
            period_ms: 2000,
            depth: 600,
            easing: Easing::EaseInOut,
        }),
        Transition::CUT,
        0,
    );
    // Half a period: the breathe's trough.
    let before = frame16(a.render(1_000).unwrap());
    let orbit = LookSpec::comet(Rgbw::rgb(0x8a5cff), 1200, 7000, 1, 50);
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
    let (look, _) = intent.resolve_builtin(&defaults);
    assert_eq!(
        block(&look),
        Block::Comet {
            color: green,
            period_ms: 1_200,
            tail: 6_000,
            heads: 1,
            base: 60,
            reverse: false,
        }
    );
    let mut timed = Intent::<16>::new(orbit);
    timed.period_ms = Some(1_600);
    let (look, _) = timed.resolve_builtin(&defaults);
    assert!(matches!(
        block(&look),
        Block::Comet {
            period_ms: 1_600,
            ..
        }
    ));
    // The defaults: the progress colour, two heads, no floor.
    let (look, _) = Intent::<16>::new(Show::Orbit {
        color: None,
        tail: None,
        heads: 2,
        base: None,
    })
    .resolve_builtin(&defaults);
    assert!(matches!(
        block(&look),
        Block::Comet { color, tail: 5_000, heads: 2, base: 180, .. }
            if color == defaults.progress
    ));
}

#[test]
fn system_looks_match_the_update_states() {
    let defaults = Defaults::default();
    let resolve = |state| {
        Intent::<16>::new(Show::System(state))
            .resolve_builtin(&defaults)
            .0
    };
    // Unknown amount: the purple comet, 8 LEDs, 18% floor.
    let verifying = resolve(SystemState::Updating {
        progress: None,
        phase: Phase::Verifying,
    });
    assert_eq!(
        block(&verifying),
        Block::Comet {
            color: Rgbw::rgb(0x8a5cff),
            period_ms: 1_200,
            tail: 8_000,
            heads: 1,
            base: 180,
            reverse: false,
        }
    );
    assert_eq!(verifying.layers[0].map(|l| l.brightness), Some(255));
    // Staged: the green breathe, 2.2 s, down to 55%.
    let staged = resolve(SystemState::Updating {
        progress: Some(1000),
        phase: Phase::Staged,
    });
    assert_eq!(
        block(&staged),
        Block::Fill {
            color: Rgbw::rgb(0x00ff20)
        }
    );
    assert_eq!(
        staged.envelope,
        Effect::Breathe {
            period_ms: 2_200,
            depth: 450,
            easing: Easing::EaseInOut,
        }
    );
    // Trial boot: a faint white breathe (3 s) under a warm-white sparkle.
    let trying = resolve(SystemState::Booting);
    assert!(matches!(
        block(&trying),
        Block::Wash { color, effect: Effect::Breathe { period_ms: 3_000, .. } }
            if color == Rgbw::rgb(0xffffff)
    ));
    assert!(matches!(
        trying.layers[1].expect("a sparkle").block,
        Block::Sparkle(s) if s.colors[0] == Rgbw::rgb(0xfff4e6) && s.base == 30 && !s.fall
    ));
    // Failed: a red breathe, 2.4 s, down to 10%.
    let failed = resolve(SystemState::UpdateFailed);
    assert_eq!(
        block(&failed),
        Block::Fill {
            color: Rgbw::rgb(0xff3b3b)
        }
    );
    assert_eq!(
        failed.envelope,
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
    let (look, _) = updating.resolve_builtin(&defaults);
    assert!(matches!(
        block(&look),
        Block::Arc { color, .. } if color == defaults.updating
    ));
    let (look, _) =
        Intent::<16>::new(Show::System(SystemState::Booting)).resolve_builtin(&defaults);
    assert!(look.is_moving());
    let (look, _) =
        Intent::<16>::new(Show::System(SystemState::RolledBack)).resolve_builtin(&defaults);
    assert!(matches!(look.envelope, Effect::Breathe { .. }));
    let chase = Defaults {
        locate_effect: EffectKind::Chase,
        ..defaults
    };
    let (look, _) = Intent::<16>::new(Show::Locate).resolve_builtin(&chase);
    assert!(look.is_moving());
    let (look, _) =
        Intent::<16>::new(Show::Indeterminate { color: None }).resolve_builtin(&defaults);
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

#[test]
fn layers_composite_by_max_or_add_with_their_own_brightness() {
    let mut a: Animator<4> = Animator::new(4);
    let mut look = LookSpec::fill(Rgbw::rgb(0x404040));
    assert!(
        look.push(
            LayerSpec::new(Block::Fill {
                color: Rgbw::rgb(0x404040),
            })
            .with_mode(Mode::Add)
        )
    );
    a.set(look, Transition::CUT, 0);
    assert_eq!(a.render(0).unwrap()[0], Rgbw::rgb(0x808080));
    // Max keeps the brighter channel of the two.
    let mut look = LookSpec::fill(Rgbw::rgb(0x800000));
    look.push(LayerSpec::new(Block::Fill {
        color: Rgbw::rgb(0x008000),
    }));
    a.set(look, Transition::CUT, 0);
    assert_eq!(a.render(0).unwrap()[0], Rgbw::new(0x80, 0x80, 0, 0));
    // A layer's brightness scales its own colour, not the others.
    let mut look = LookSpec::fill(Rgbw::rgb(0x0000ff));
    look.push(
        LayerSpec::new(Block::Fill {
            color: Rgbw::rgb(0xff0000),
        })
        .with_brightness(128)
        .with_mode(Mode::Add),
    );
    a.set(look, Transition::CUT, 0);
    let p = a.render(0).unwrap()[0];
    assert_eq!((p.r, p.b), (128, 255));
    // A look's layers stop at the first gap, and a full look refuses a fifth.
    let mut full = LookSpec::OFF;
    for _ in 0..MAX_LAYERS - 1 {
        assert!(full.push(LayerSpec::new(Block::Fill { color: Rgbw::OFF })));
    }
    assert!(!full.push(LayerSpec::new(Block::Fill { color: Rgbw::OFF })));
}

#[test]
fn a_comet_can_run_the_other_way_round() {
    let mut a: Animator<16> = Animator::new(16);
    let mut look = LookSpec::comet(Rgbw::rgb(0xffffff), 1600, 1000, 1, 0);
    if let Some(layer) = look.layers[0].as_mut()
        && let Block::Comet { reverse, .. } = &mut layer.block
    {
        *reverse = true;
    }
    a.set(look, Transition::CUT, 0);
    let f = frame16(a.render(0).unwrap());
    // The head is at LED 0; going backward, the tail is on the far side of
    // it, so LEDs 1 and 15 are dark.
    assert_eq!(f[0].r, 255);
    assert_eq!(f[1].r, 0);
    assert_eq!(f[15].r, 0);
}

#[test]
fn validation_names_the_first_bad_value() {
    assert_eq!(LookSpec::OFF.validate(), Ok(()));
    assert_eq!(
        LookSpec::comet(Rgbw::rgb(1), 1600, 3000, 2, 0).validate(),
        Ok(())
    );
    assert_eq!(
        LookSpec::comet(Rgbw::rgb(1), 1600, 3000, 3, 0).validate(),
        Err("a comet's heads must be 1 or 2")
    );
    assert_eq!(
        LookSpec::comet(Rgbw::rgb(1), 50, 3000, 1, 0).validate(),
        Err("a comet's period_ms must be 100 to 600000")
    );
    assert_eq!(
        LookSpec::comet(Rgbw::rgb(1), 1600, 0, 1, 0).validate(),
        Err("a comet's tail must be above 0 and at most 64 LEDs")
    );
    assert_eq!(
        LookSpec::progress(1001, Rgbw::rgb(1), Rgbw::OFF).validate(),
        Err("an arc's fraction must be 0 to 1")
    );
    assert_eq!(
        LookSpec::fill(Rgbw::rgb(1))
            .with_envelope(Effect::Breathe {
                period_ms: 2000,
                depth: 1200,
                easing: Easing::Linear,
            })
            .validate(),
        Err("a breathe's depth must be 0 to 1")
    );
    let empty = LookSpec {
        layers: [None; MAX_LAYERS],
        ..LookSpec::OFF
    };
    assert_eq!(empty.validate(), Err("a look needs at least one layer"));
}

#[test]
fn look_names_are_short_lowercase_words() {
    assert_eq!(LookName::new("pv.targets").unwrap().as_str(), "pv.targets");
    assert!(LookName::new("system.rolled-back").is_some());
    assert!(LookName::new("").is_none());
    assert!(LookName::new("Pv.targets").is_none());
    assert!(LookName::new("has space").is_none());
    assert!(LookName::new(&"a".repeat(MAX_LOOK_NAME + 1)).is_none());
    assert!(LookName::new(&"a".repeat(MAX_LOOK_NAME)).is_some());
}

#[test]
fn a_named_look_comes_from_the_table_before_the_built_ins() {
    let defaults = Defaults::default();
    let custom = LookSpec::fill(Rgbw::rgb(0x123456));
    let table = |name: &str| (name == "status.ok").then_some(custom);
    let (look, _) = Intent::<16>::new(Show::Status(Status::Ok)).resolve(&defaults, &table);
    assert_eq!(
        look.layers[0].map(|l| l.block),
        Some(Block::Fill {
            color: Rgbw::rgb(0x123456),
        })
    );
    // Names the table does not hold keep their built-in look.
    let (look, _) = Intent::<16>::new(Show::Status(Status::Error)).resolve(&defaults, &table);
    assert!(matches!(look.envelope, Effect::Blink { .. }));
    // An unknown name shows nothing.
    let name = LookName::new("nothing.here").unwrap();
    let (look, _) = Intent::<16>::new(Show::Look {
        name,
        progress: None,
    })
    .resolve(&defaults, &table);
    assert_eq!(block(&look), Block::Fill { color: Rgbw::OFF });
    // An inline look is used as given, whatever the table holds.
    let inline = LookSpec::fill(Rgbw::rgb(0x00ff00));
    let (look, _) = Intent::<16>::new(Show::Inline {
        spec: inline,
        progress: None,
    })
    .resolve(&defaults, &table);
    assert_eq!(look, inline.with_brightness(128));
}

#[test]
fn a_named_arc_takes_the_request_progress() {
    let defaults = Defaults::default();
    let name = LookName::new("system.writing").unwrap();
    let (look, _) = Intent::<16>::new(Show::Look {
        name,
        progress: Some(750),
    })
    .resolve_builtin(&defaults);
    assert!(matches!(
        block(&look),
        Block::Arc {
            fraction: Fraction::Fixed(750),
            ..
        }
    ));
    let (look, _) = Intent::<16>::new(Show::System(SystemState::Updating {
        progress: Some(300),
        phase: Phase::Writing,
    }))
    .resolve_builtin(&defaults);
    assert!(matches!(
        block(&look),
        Block::Arc {
            fraction: Fraction::Fixed(300),
            ..
        }
    ));
}

#[test]
fn the_ring_wide_brightness_scales_every_look_and_the_ember_keeps_a_floor() {
    let mut defaults = Defaults {
        look_brightness: 128,
        ..Defaults::default()
    };
    let (color, _) = Intent::<16>::new(Show::Color(Rgbw::rgb(0xffffff))).resolve_builtin(&defaults);
    // A fill is capped at 0.7 of full, then halved by the ring.
    assert_eq!(color.brightness, 89);
    // A request's brightness is still scaled by the ring.
    let mut asked = Intent::<16>::new(Show::Color(Rgbw::rgb(0xffffff)));
    asked.brightness = Some(255);
    let (look, _) = asked.resolve_builtin(&defaults);
    assert_eq!(look.brightness, 128);
    // The ember does not go below its floor however dim the ring is.
    defaults.look_brightness = 10;
    let (ember, _) =
        Intent::<16>::new(Show::System(SystemState::Rebooting)).resolve_builtin(&defaults);
    assert_eq!(ember.brightness, 40);
    // Half the ring-wide brightness is half a full fill (rounded).
    defaults.look_brightness = 128;
    let (fill, _) = Intent::<16>::new(Show::Status(Status::Ok)).resolve_builtin(&defaults);
    assert_eq!(fill.brightness, 89);
}

#[test]
fn system_states_map_to_named_looks_and_every_name_is_built_in() {
    let updating = |progress, phase| SystemState::Updating { progress, phase };
    let cases = [
        (updating(None, Phase::Verifying), "system.verifying"),
        (updating(Some(400), Phase::Writing), "system.writing"),
        (updating(None, Phase::Writing), "system.writing-unknown"),
        (updating(None, Phase::Staged), "system.staged"),
        (updating(Some(900), Phase::Staged), "system.staged"),
        (updating(None, Phase::Applying), "system.rebooting"),
        (SystemState::Booting, "system.booting"),
        (SystemState::Rebooting, "system.rebooting"),
        (SystemState::UpdateFailed, "system.failed"),
        (SystemState::RolledBack, "system.rolled-back"),
        (SystemState::Confirmed, "system.confirmed"),
    ];
    for (state, name) in cases {
        assert_eq!(state.look_name(), name, "{state:?}");
        assert!(BUILTIN_LOOK_NAMES.contains(&name), "{name} is built in");
    }
    assert_eq!(Status::Busy.look_name(), "status.busy");
    assert!(BUILTIN_LOOK_NAMES.contains(&"system.locate"));
    assert!(BUILTIN_LOOK_NAMES.contains(&"status.off"));
    // Each built-in name resolves to a look, and only to those names.
    let defaults = Defaults::default();
    for name in BUILTIN_LOOK_NAMES {
        assert!(builtin_look(name, &defaults).is_some(), "{name}");
    }
    assert!(builtin_look("pv.nothing", &defaults).is_none());
}

#[test]
fn named_arcs_and_inline_looks_share_the_one_render_path() {
    // A named look and the same look inline render alike, frame for frame.
    let defaults = Defaults::default();
    let name = LookName::new("system.writing").unwrap();
    let (named, _) = Intent::<16>::new(Show::Look {
        name,
        progress: Some(500),
    })
    .resolve_builtin(&defaults);
    let (system, _) = Intent::<16>::new(Show::System(SystemState::Updating {
        progress: Some(500),
        phase: Phase::Writing,
    }))
    .resolve_builtin(&defaults);
    assert_eq!(named, system);
    // The same look given in full (unscaled, as a client sends it), with the
    // request's progress, resolves to the same look.
    let spec = builtin_look("system.writing", &defaults).unwrap();
    let (inline, _) = Intent::<16>::new(Show::Inline {
        spec,
        progress: Some(500),
    })
    .resolve_builtin(&defaults);
    assert_eq!(inline, named);
}

/// The largest change in any LED's red between two frames 1 ms apart, over
/// one turn of `look` (and across its wrap).
fn largest_step_per_ms(look: &LookSpec, count: usize) -> (u8, u64) {
    let period = match look.layers[0].expect("a layer").block {
        Block::Comet { period_ms, .. } => u64::from(period_ms),
        _ => panic!("a comet look"),
    };
    assert!(count <= 16);
    let idle = crate::sparkle::SparkleState::new(0, &crate::Sparkle::DEFAULT);
    let frame = |t: u64| -> [u8; 16] {
        let mut out = [0u8; 16];
        for (i, slot) in out.iter_mut().enumerate().take(count) {
            *slot = look.color(i, count, t, 0, &idle, t, 0).r;
        }
        out
    };
    let mut worst = (0u8, 0u64);
    let mut prev = frame(0);
    for t in 1..=period {
        let next = frame(t);
        for (a, b) in prev.iter().zip(&next) {
            let step = a.abs_diff(*b);
            if step > worst.0 {
                worst = (step, t);
            }
        }
        prev = next;
    }
    worst
}

#[test]
fn comet_leading_edges_move_smoothly_at_one_ms_steps() {
    // A comet's head crosses 16 LEDs in its period: at most a few steps of
    // 255 in a millisecond, never a whole LED's jump (255 in one step).
    let reversed = LookSpec::of(LayerSpec::new(Block::Comet {
        color: Rgbw::rgb(0xffffff),
        period_ms: 1200,
        tail: 5_000,
        heads: 1,
        base: 0,
        reverse: true,
    }));
    let cases = [
        (
            "one head, the default",
            LookSpec::comet(Rgbw::rgb(0xffffff), 1200, 5000, 1, 0),
        ),
        (
            "one head, a short tail",
            LookSpec::comet(Rgbw::rgb(0xffffff), 1200, 1000, 1, 0),
        ),
        (
            "one head, a long tail",
            LookSpec::comet(Rgbw::rgb(0x00ff20), 1600, 6000, 1, 60),
        ),
        (
            "two heads, amber",
            LookSpec::comet(Rgbw::rgb(0xffa424), 2400, 5000, 2, 50),
        ),
        (
            "two heads, blue",
            LookSpec::comet(Rgbw::rgb(0x2f7bff), 2400, 5000, 2, 80),
        ),
        (
            "two heads, short",
            LookSpec::comet(Rgbw::rgb(0xffffff), 1000, 2000, 2, 0),
        ),
        ("one head, reversed", reversed),
    ];
    for (what, look) in cases {
        let (step, at) = largest_step_per_ms(&look, 16);
        assert!(step <= 9, "{what}: a LED moved {step} at {at} ms");
    }
}

#[test]
fn a_pulse_flashes_holds_then_glows_down_once() {
    let pulse = Effect::Pulse {
        attack_ms: 100,
        hold_ms: 200,
        decay_ms: 400,
        repeat: 1,
    };
    assert_eq!(pulse.level(0), 0);
    // Half way up the attack.
    assert!(
        pulse.level(50).abs_diff(ONE / 2) < 600,
        "{}",
        pulse.level(50)
    );
    assert_eq!(pulse.level(100), ONE);
    assert_eq!(pulse.level(299), ONE);
    // Half way down the decay: a quadratic ease-out, a quarter of full.
    assert!(
        pulse.level(500).abs_diff(ONE / 4) < 600,
        "{}",
        pulse.level(500)
    );
    assert_eq!(pulse.level(700), 0);
    assert_eq!(pulse.level(5_000), 0);
    // Repeat 0 keeps going.
    let every = Effect::Pulse {
        attack_ms: 100,
        hold_ms: 200,
        decay_ms: 400,
        repeat: 0,
    };
    assert!(every.level(750).abs_diff(ONE / 2) < 600);
    assert!(pulse.is_animated());
}

/// LED levels (red channel, 0..=255) of a drain look at `t` ms, for `count`
/// LEDs with the ring's bottom at `bottom` (thousandths).
fn drain_frame(count: usize, bottom: u32, t: u64) -> [u8; 16] {
    let look = confirmed_drain();
    let mut anim = Animator::<16>::new(count);
    anim.set_bottom(bottom);
    anim.set(look, Transition::CUT, 0);
    // Render at the time asked for: the frame is what the animator shows then.
    let mut out = [0u8; 16];
    if let Some(frame) = anim.render(t) {
        for (slot, px) in out.iter_mut().zip(frame) {
            *slot = px.g;
        }
    }
    out
}

/// A drain look alone: green, fill 700 ms, drain from 1100 ms over 2100 ms.
fn confirmed_drain() -> LookSpec {
    LookSpec::of(LayerSpec::new(Block::Drain {
        color: Rgbw::rgb(0x00ff00),
        fill_ms: 700,
        start_ms: 1_100,
        duration_ms: 2_100,
        easing: Easing::Linear,
    }))
}

#[test]
fn a_drain_fills_from_the_top_then_recedes_toward_the_bottom_on_either_arc() {
    for bottom in [4_000u32, 12_000] {
        // Full ring once filled, before the drain starts.
        let full = drain_frame(16, bottom, 1_000);
        assert!(full.iter().all(|&g| g >= 250), "bottom {bottom}: {full:?}");
        // Halfway through the drain: the bottom side lit, the top dark.
        let half = drain_frame(16, bottom, 1_100 + 1_050);
        let b = (bottom / 1_000) as usize;
        assert!(half[b] > 200, "bottom {bottom} lit: {half:?}");
        assert_eq!(half[(b + 8) % 16], 0, "bottom {bottom} top dark: {half:?}");
        // The lit region only shrinks: an LED lit later was lit earlier.
        let mut prev = [255u8; 16];
        for t in (1_100..=3_200).step_by(50) {
            let now = drain_frame(16, bottom, t);
            for i in 0..16 {
                assert!(now[i] <= prev[i], "bottom {bottom} LED {i} at {t} ms relit");
            }
            prev = now;
        }
        // Drained: the ring is dark.
        assert!(drain_frame(16, bottom, 3_250).iter().all(|&g| g == 0));
    }
}

#[test]
fn a_drain_is_eased_in_so_it_speeds_up() {
    // Halfway through the time, an ease-in has drained less than linear.
    let linear = drain_frame(16, 0, 1_100 + 1_050);
    let eased = LookSpec::of(LayerSpec::new(Block::Drain {
        color: Rgbw::rgb(0x00ff00),
        fill_ms: 700,
        start_ms: 1_100,
        duration_ms: 2_100,
        easing: Easing::EaseIn,
    }));
    let mut anim = Animator::<16>::new(16);
    anim.set_bottom(0);
    anim.set(eased, Transition::CUT, 0);
    let mut out = [0u8; 16];
    if let Some(frame) = anim.render(1_100 + 1_050) {
        for (slot, px) in out.iter_mut().zip(frame) {
            *slot = px.g;
        }
    }
    let lit_linear = linear.iter().filter(|&&g| g > 0).count();
    let lit_eased = out.iter().filter(|&&g| g > 0).count();
    // Slow start: halfway the eased drain has passed less of the ring.
    assert!(
        lit_eased > lit_linear,
        "eased {lit_eased} vs linear {lit_linear}"
    );
}

#[test]
fn a_drain_without_a_gravity_read_drains_to_the_default_bottom() {
    // No set_bottom: the animator's default is the middle of the ring (LED 8).
    let look = confirmed_drain();
    let mut anim = Animator::<16>::new(16);
    anim.set(look, Transition::CUT, 0);
    let mut out = [0u8; 16];
    if let Some(frame) = anim.render(1_100 + 1_900) {
        for (slot, px) in out.iter_mut().zip(frame) {
            *slot = px.g;
        }
    }
    // Nearly drained: only the LEDs next to the default bottom remain.
    assert!(out[8] >= out[7] && out[8] >= out[9], "{out:?}");
    assert!(out[0] == 0, "the top is dark: {out:?}");
}

#[test]
fn a_drain_is_flagged_as_needing_the_bottom_and_round_trips_its_fields() {
    assert!(confirmed_drain().wants_bottom());
    assert!(!LookSpec::fill(Rgbw::rgb(0xffffff)).wants_bottom());
}

#[test]
fn an_over_layer_keeps_its_head_pure_and_shows_the_glow_elsewhere() {
    let mut a: Animator<4> = Animator::new(4);
    let glow = LayerSpec::fill(Rgbw::rgb(0x28c8ff)).with_brightness(64);
    // A full-level head covers the glow: its own colour, nothing of the glow.
    let mut look = LookSpec::EMPTY;
    look.push(glow);
    look.push(LayerSpec::fill(Rgbw::rgb(0xff5a00)).with_mode(Mode::Over));
    a.set(look, Transition::CUT, 0);
    assert_eq!(a.render(0).unwrap()[0], Rgbw::rgb(0xff5a00));

    // No head (brightness 0 on the upper layer): the glow alone.
    let mut only_glow = LookSpec::EMPTY;
    only_glow.push(glow);
    a.set(only_glow, Transition::CUT, 0);
    let glow_only = a.render(0).unwrap()[0];
    let mut none = LookSpec::EMPTY;
    none.push(glow);
    none.push(
        LayerSpec::fill(Rgbw::rgb(0xff5a00))
            .with_brightness(0)
            .with_mode(Mode::Over),
    );
    a.set(none, Transition::CUT, 0);
    assert_eq!(a.render(0).unwrap()[0], glow_only);
    // The glow is cyan (28c8ff): blue and green, with little red, not orange.
    assert!(
        glow_only.b > glow_only.r && glow_only.g > glow_only.r,
        "{glow_only:?}"
    );
}

#[test]
fn an_over_layer_blends_a_tail_without_hue_mixing_from_a_clip() {
    let mut a: Animator<4> = Animator::new(4);
    let mut look = LookSpec::EMPTY;
    look.push(LayerSpec::fill(Rgbw::rgb(0x28c8ff)));
    // Half the head's level: the glow shows through by the other half.
    look.push(
        LayerSpec::fill(Rgbw::rgb(0xff5a00))
            .with_brightness(128)
            .with_mode(Mode::Over),
    );
    a.set(look, Transition::CUT, 0);
    let p = a.render(0).unwrap()[0];
    // Red comes from the head alone (the glow has none), so the mix is
    // half the head's red and half the glow's blue: the head scaled by
    // its level plus the glow where the head is not.
    assert!(p.r > 0 && p.r < 255, "{p:?}");
    assert!(p.b > 0 && p.b < 255, "{p:?}");
    // Max would give the glow's blue at full on top of the head's red; over
    // keeps the blue below full.
    let mut max = LookSpec::EMPTY;
    max.push(LayerSpec::fill(Rgbw::rgb(0x28c8ff)));
    max.push(LayerSpec::fill(Rgbw::rgb(0xff5a00)).with_brightness(128));
    a.set(max, Transition::CUT, 0);
    let m = a.render(0).unwrap()[0];
    assert!(p.b < m.b, "over {p:?} vs max {m:?}");
}
