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
    assert!(matches!(
        block(&look),
        Block::Ripple { color, .. } if color == defaults.confirmed
    ));
    assert_eq!(SystemState::Confirmed.name(), "confirmed");

    let (look, _) =
        Intent::<16>::new(Show::System(SystemState::Rebooting)).resolve_builtin(&defaults);
    assert!(matches!(
        block(&look),
        Block::Fill { color } if color == defaults.rebooting
    ));
    assert_eq!(look.envelope, Effect::Solid);
    assert!(!look.is_moving());
    // 12% of the amber: 31 of 255 on the red channel.
    assert_eq!(EMBER, 31);
    let mut a: Animator<16> = Animator::new(16);
    a.set(look, Transition::new(1200, Easing::EaseInOut), 0);
    a.render(0);
    let end = frame16(a.render(1200).unwrap());
    // At the default ring-wide brightness (half) the ember is held at its
    // floor, about 6%: 16 of 255.
    assert_eq!(end[0], Rgbw::rgb(0xff8000).scaled(16));
    assert_eq!(end[0].r, 16);
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
        Block::Comet { color, tail: 5_000, heads: 2, base: 0, .. }
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
    // Unknown amount: the purple comet, 7 LEDs, 5% floor.
    let verifying = resolve(SystemState::Updating {
        progress: None,
        phase: Phase::Verifying,
    });
    assert_eq!(
        block(&verifying),
        Block::Comet {
            color: Rgbw::rgb(0x8a5cff),
            period_ms: 1_200,
            tail: 7_000,
            heads: 1,
            base: 50,
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
            color: Rgbw::rgb(0x2bd47d)
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
    // Trial boot: the twin comet, 1.8 s, 5 LEDs, 4% floor.
    let trying = resolve(SystemState::Booting);
    assert_eq!(
        block(&trying),
        Block::Comet {
            color: Rgbw::rgb(0xfff4e6),
            period_ms: 1_800,
            tail: 5_000,
            heads: 2,
            base: 40,
            reverse: false,
        }
    );
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
    assert_eq!(ember.brightness, 16);
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
