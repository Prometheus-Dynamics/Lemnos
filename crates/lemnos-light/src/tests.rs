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
    // 3.5 LEDs of 16: three full, the fourth half lit.
    let fraction = ONE * 35 / 160;
    a.set(
        Look::progress(fraction, green, Rgbw::OFF),
        Transition::CUT,
        0,
    );
    let frame = a.render(0).unwrap();
    assert_eq!(frame[2], green);
    assert!(frame[3].g > 100 && frame[3].g < 155, "{:?}", frame[3]);
    assert_eq!(frame[4], Rgbw::OFF);
    // A new fraction advances (the fifth LED fills in over the fade).
    a.set(
        Look::progress(ONE * 5 / 16, green, Rgbw::OFF),
        Transition::new(100, Easing::Linear),
        0,
    );
    let mid = a.render(50).unwrap();
    assert!(mid[3].g > 200, "{:?}", mid[3]);
    assert!(mid[4].g < 100);
    let end = a.render(100).unwrap();
    assert_eq!(end[4], green);
    assert!(a.render(150).is_none());
}

#[test]
fn spinner_moves_a_comet_and_keeps_rendering() {
    let mut a: Animator<8> = Animator::new(8);
    a.set(
        Look::spinner(Rgbw::rgb(0xffffff), Rgbw::OFF, 800, 3),
        Transition::CUT,
        0,
    );
    let at0 = a.render(0).unwrap().to_owned_array();
    assert_eq!(at0[0].r, 255);
    assert!(at0[7].r > 0 && at0[7].r < 255);
    assert_eq!(at0[4], Rgbw::OFF);
    let at100 = a.render(100).unwrap().to_owned_array();
    assert_eq!(at100[1].r, 255);
    assert_eq!(a.next_frame_ms(100), Some(120));
}

trait Owned {
    fn to_owned_array(&self) -> [Rgbw; 8];
}

impl Owned for [Rgbw] {
    fn to_owned_array(&self) -> [Rgbw; 8] {
        let mut out = [Rgbw::OFF; 8];
        out[..self.len()].copy_from_slice(self);
        out
    }
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
