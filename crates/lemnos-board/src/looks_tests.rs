//! Look definitions: parsing, every error path, and the file form round trip.

use crate::looks::{body_toml, from_json, from_toml, parse_file, to_toml};
use lemnos_light::{
    BUILTIN_LOOK_NAMES, Block, Defaults, Easing, Effect, Fraction, LayerSpec, LookSpec, Mode, Rgbw,
    builtin_look,
};

const PV_TARGETS: &str = r#"
[looks."pv.targets"]
envelope = { kind = "breathe", period_ms = 4000, depth = 0.18, easing = "ease-in-out" }
brightness = 0.7
layers = [
  { block = "fill", color = "2f7bff" },
]
"#;

/// The first error, as text.
fn first_error(text: &str) -> String {
    parse_file("looks.d/test.toml", text)
        .expect_err("should fail")
        .first()
        .expect("an error")
        .to_string()
}

fn body_error(text: &str) -> String {
    from_toml("--spec", text)
        .expect_err("should fail")
        .first()
        .expect("an error")
        .to_string()
}

#[test]
fn a_file_of_looks_parses_with_defaults() {
    let looks = parse_file("looks.d/pv.toml", PV_TARGETS).unwrap();
    assert_eq!(looks.len(), 1);
    let (name, spec) = &looks[0];
    assert_eq!(name, "pv.targets");
    assert_eq!(
        spec.envelope,
        Effect::Breathe {
            period_ms: 4000,
            depth: 180,
            easing: Easing::EaseInOut,
        }
    );
    assert_eq!(spec.brightness, 179);
    assert_eq!(spec.floor, 0);
    assert_eq!(spec.layers[0], Some(LayerSpec::fill(Rgbw::rgb(0x2f7bff))));
}

#[test]
fn every_block_parses_with_its_defaults_and_its_own_keys() {
    let look = from_toml(
        "--spec",
        r#"
layers = [
  { block = "comet", color = "2bd47d", period_ms = 1600, tail = 6, base = 0.06 },
  { block = "arc", fraction = 0.5, color = "2f7bff", track = "101012", head = 0.35, sheen = false, brightness = 0.5, mode = "add" },
  { block = "ripple", color = "2bd47d", origin = 4, speed = 12, width = 2.2, settle_ms = 1400, glow = 0.15 },
  { block = "frame", pixels = ["ff0000", "00ff00", "0000ff"] },
]
"#,
    )
    .unwrap();
    let blocks: Vec<Block> = look.iter().map(|l| l.block).collect();
    assert_eq!(blocks.len(), 4);
    assert_eq!(
        blocks[0],
        Block::Comet {
            color: Rgbw::rgb(0x2bd47d),
            period_ms: 1600,
            tail: 6000,
            heads: 1,
            base: 60,
            reverse: false,
        }
    );
    assert_eq!(look.layers[1].unwrap().mode, Mode::Add);
    assert_eq!(look.layers[1].unwrap().brightness, 128);
    assert!(matches!(
        blocks[1],
        Block::Arc {
            fraction: Fraction::Fixed(500),
            sheen: false,
            ..
        }
    ));
    assert!(matches!(
        blocks[2],
        Block::Ripple {
            origin: 4,
            speed: 12_000,
            width: 2_200,
            ..
        }
    ));
    assert!(matches!(blocks[3], Block::Frame { len: 3, .. }));
    // An arc alone: input fraction, the default track, a head and a sheen.
    let last = from_toml(
        "--spec",
        "layers = [{ block = \"arc\", color = \"00ff40\" }]",
    )
    .unwrap();
    assert!(matches!(
        last.layers[0].unwrap().block,
        Block::Arc {
            fraction: Fraction::Input,
            track,
            head: 350,
            sheen: true,
            ..
        } if track == Rgbw::rgb(0x101012)
    ));
}

#[test]
fn a_pulse_envelope_parses_with_its_defaults_and_checks_its_ranges() {
    let look = from_toml(
        "--spec",
        "envelope = { kind = \"pulse\", attack_ms = 60, hold_ms = 600, decay_ms = 1200 }\nlayers = [{ block = \"fill\", color = \"00ff20\" }]",
    )
    .unwrap();
    assert_eq!(
        look.envelope,
        Effect::Pulse {
            attack_ms: 60,
            hold_ms: 600,
            decay_ms: 1200,
            repeat: 1,
        }
    );
    // A bare name takes the defaults, and the file form round-trips.
    let bare = from_toml(
        "--spec",
        "envelope = \"pulse\"\nlayers = [{ block = \"fill\", color = \"00ff20\" }]",
    )
    .unwrap();
    assert_eq!(
        bare.envelope,
        Effect::Pulse {
            attack_ms: 80,
            hold_ms: 0,
            decay_ms: 1000,
            repeat: 1,
        }
    );
    let again = from_toml("--spec", &body_toml(&look)).unwrap();
    assert_eq!(again.envelope, look.envelope);
    for (text, want) in [
        (
            "envelope = { kind = \"pulse\", decay_ms = 0 }",
            "decay_ms: must be a whole number of milliseconds from 1 to 60000",
        ),
        (
            "envelope = { kind = \"pulse\", repeat = 300 }",
            "repeat: must be a whole number from 0",
        ),
        ("envelope = { kind = \"pulse\", flash = 1 }", "flash"),
    ] {
        let text = format!("{text}\nlayers = [{{ block = \"fill\", color = \"ff0000\" }}]");
        let error = body_error(&text);
        assert!(error.contains(want), "{error}");
    }
}

#[test]
fn a_bare_envelope_name_takes_its_defaults() {
    let look = from_toml(
        "--spec",
        "envelope = \"blink\"\nlayers = [{ block = \"fill\", color = \"ff0000\" }]",
    )
    .unwrap();
    assert_eq!(
        look.envelope,
        Effect::Blink {
            period_ms: 1000,
            duty: 500
        }
    );
}

#[test]
fn json_and_toml_give_the_same_look() {
    let toml = from_toml(
        "--spec",
        "layers = [{ block = \"comet\", color = \"2bd47d\", period_ms = 1600, tail = 6, base = 0.06 }]",
    )
    .unwrap();
    let json = from_json(
        "--json",
        r#"{"layers":[{"block":"comet","color":"2bd47d","period_ms":1600,"tail":6,"base":0.06}]}"#,
    )
    .unwrap();
    assert_eq!(toml, json);
}

#[test]
fn a_built_in_look_round_trips_through_its_file_form() {
    let defaults = Defaults::default();
    for name in BUILTIN_LOOK_NAMES {
        let spec = builtin_look(name, &defaults).unwrap();
        let text = to_toml(name, &spec);
        let back = parse_file(name, &text).unwrap_or_else(|e| panic!("{name}: {e:?}\n{text}"));
        assert_eq!(back, vec![(name.to_string(), spec)], "{name}\n{text}");
    }
}

#[test]
fn a_cubic_bezier_and_a_frame_round_trip() {
    let mut look = LookSpec::frame(&[Rgbw::rgb(0xff0000), Rgbw::rgb(0x00ff00)]);
    look.envelope = Effect::Breathe {
        period_ms: 2000,
        depth: 600,
        easing: Easing::parse("cubic-bezier(0.42, 0, 0.58, 1)").unwrap(),
    };
    let text = body_toml(&look);
    assert_eq!(from_toml("body", &text).unwrap(), look, "{text}");
}

#[test]
fn errors_name_the_file_and_the_key() {
    let e = first_error(
        "[looks.\"pv.bad\"]\nlayers = [{ block = \"comet\", color = \"ff0000\", heads = 3 }]\n",
    );
    assert!(
        e.starts_with("looks.d/test.toml: looks.pv.bad.layers[0].heads:"),
        "{e}"
    );
    let e = first_error("[looks.pv]\nlayers = [{ block = \"fill\", colour = \"ff0000\" }]\n");
    assert!(e.contains("looks.pv.layers[0].colour"), "{e}");
    assert!(e.contains("unknown key"), "{e}");
}

#[test]
fn unknown_top_level_keys_and_blocks_are_rejected_with_the_list() {
    let e = first_error("[looks.pv]\nlayer = []\n");
    assert!(e.contains("unknown key"), "{e}");
    let e = body_error("layers = [{ block = \"bogus\", color = \"ff0000\" }]");
    assert!(e.contains("unknown block \"bogus\""), "{e}");
    let e = body_error("layers = [{ block = \"fill\", color = \"ff0000\", period_ms = 100 }]");
    assert!(
        e.contains("unknown key (allowed: block, color, brightness, mode)"),
        "{e}"
    );
}

#[test]
fn layer_counts_and_missing_keys_are_checked() {
    assert!(body_error("layers = []").contains("has 0 layers"));
    let five = "layers = [{ block = \"fill\", color = \"ff0000\" }, { block = \"fill\", color = \"ff0000\" }, { block = \"fill\", color = \"ff0000\" }, { block = \"fill\", color = \"ff0000\" }, { block = \"fill\", color = \"ff0000\" }]";
    assert!(body_error(five).contains("has 5 layers; a look has 1 to 4"));
    assert!(body_error("brightness = 1.0").contains("needs layers"));
    assert!(body_error("layers = [{ color = \"ff0000\" }]").contains("needs block"));
    assert!(body_error("layers = [{ block = \"fill\" }]").contains("needs a colour"));
    assert!(
        body_error("layers = [{ block = \"fill\", color = \"zz0000\" }]")
            .contains("must be a colour")
    );
}

#[test]
fn numbers_out_of_range_are_named() {
    let base = "layers = [{ block = \"fill\", color = \"ff0000\" }]\n";
    let cases = [
        (format!("{base}brightness = 1.5"), "brightness: must be a number from 0 to 1"),
        (format!("{base}min_brightness = -0.1"), "min_brightness: must be a number from 0 to 1"),
        (
            "layers = [{ block = \"comet\", color = \"ff0000\", heads = 3 }]".to_string(),
            "heads: must be 1 or 2",
        ),
        (
            "layers = [{ block = \"comet\", color = \"ff0000\", period_ms = 50 }]".to_string(),
            "period_ms: must be a whole number of milliseconds from 100 to 600000",
        ),
        (
            "layers = [{ block = \"comet\", color = \"ff0000\", tail = 65 }]".to_string(),
            "tail: must be a number of LEDs above 0 and at most 64",
        ),
        (
            "layers = [{ block = \"arc\", color = \"ff0000\", fraction = 1.2 }]".to_string(),
            "fraction: must be a number from 0 to 1, or \"input\"",
        ),
        (
            "layers = [{ block = \"ripple\", color = \"ff0000\", origin = 64 }]".to_string(),
            "origin: must be an LED from 0 to 63",
        ),
        (
            "layers = [{ block = \"frame\", pixels = [] }]".to_string(),
            "has 0 pixels; a frame has 1 to 64",
        ),
        (
            "layers = [{ block = \"fill\", color = \"ff0000\", mode = \"multiply\" }]".to_string(),
            "mode: must be max or add",
        ),
        (
            "envelope = { kind = \"breathe\", depth = 1.5 }\nlayers = [{ block = \"fill\", color = \"ff0000\" }]".to_string(),
            "depth: must be a number from 0 to 1",
        ),
        (
            "envelope = { kind = \"wobble\" }\nlayers = [{ block = \"fill\", color = \"ff0000\" }]".to_string(),
            "kind: must be solid, blink, breathe or pulse",
        ),
        (
            "envelope = { kind = \"breathe\", easing = \"bounce\" }\nlayers = [{ block = \"fill\", color = \"ff0000\" }]".to_string(),
            "easing: must be linear",
        ),
    ];
    for (text, want) in cases {
        let e = body_error(&text);
        assert!(e.contains(want), "{text}\n{e}\nwant {want}");
    }
}

#[test]
fn a_look_name_is_checked_in_its_table() {
    let e =
        first_error("[looks.\"Pv Bad\"]\nlayers = [{ block = \"fill\", color = \"ff0000\" }]\n");
    assert!(e.contains("a look name is 1 to 40"), "{e}");
}

#[test]
fn a_broken_look_fails_the_file_and_the_rest_is_reported_too() {
    let text = "[looks.a]\nlayers = [{ block = \"fill\", color = \"ff0000\" }]\n[looks.b]\nlayers = []\n[looks.c]\nlayers = [{ block = \"nope\" }]\n";
    let errors = parse_file("f.toml", text).unwrap_err();
    assert_eq!(errors.len(), 2, "{errors:?}");
    assert!(errors[0].to_string().contains("looks.b.layers"));
    assert!(errors[1].to_string().contains("looks.c.layers[0].block"));
}

#[test]
fn text_that_is_not_toml_is_reported_with_the_file() {
    let e = first_error("[looks.pv\n");
    assert!(e.starts_with("looks.d/test.toml:"), "{e}");
}

#[test]
fn the_colour_forms_are_read() {
    let look = from_toml(
        "--spec",
        "layers = [{ block = \"fill\", color = \"#2f7bff\" }, { block = \"fill\", color = 0x802f7bff }]",
    )
    .unwrap();
    assert_eq!(
        look.layers[0].unwrap().block,
        Block::Fill {
            color: Rgbw::rgb(0x2f7bff)
        }
    );
    // 0xWWRRGGBB: white 0x80, then red, green, blue.
    assert_eq!(
        look.layers[1].unwrap().block,
        Block::Fill {
            color: Rgbw::new(0x2f, 0x7b, 0xff, 0x80),
        }
    );
}

fn board_text(looks: &str, light: &str) -> String {
    format!(
        r#"
format = "lemnos.board"
schema_version = 1

[board]
id = "raze"

[[devices]]
id = "ring"
driver = "ws2812"
path = "/dev/leds0"
config = {{ count = 16, {light} }}

{looks}
"#
    )
}

#[test]
fn a_board_carries_its_looks_and_validates_them_with_the_board() {
    let registry = crate::DriverRegistry::builtin();
    let ok = crate::BoardDefinition::from_toml_str(&board_text(
        "[looks.\"pv.x\"]\nlayers = [{ block = \"fill\", color = \"ff0000\" }]\n",
        "",
    ))
    .unwrap();
    assert_eq!(ok.looks.len(), 1);
    ok.validate(&registry).unwrap();

    let bad = crate::BoardDefinition::from_toml_str(&board_text(
        "[looks.\"pv.x\"]\nlayers = [{ block = \"comet\", color = \"ff0000\", heads = 3 }]\n",
        "",
    ))
    .unwrap();
    let message = bad.validate(&registry).unwrap_err().to_string();
    assert!(message.contains("looks.pv.x.layers[0].heads"), "{message}");
}

#[test]
fn look_brightness_is_the_ring_wide_scale_and_the_driver_byte_is_unchanged() {
    let board = crate::BoardDefinition::from_toml_str(&board_text(
        "",
        "look_brightness = 0.25, brightness = 1.0",
    ))
    .unwrap();
    let defaults = crate::light_defaults(&board.devices[0]).unwrap();
    assert_eq!(defaults.look_brightness, 64);
    // Unset: the default half.
    let plain = crate::BoardDefinition::from_toml_str(&board_text("", "")).unwrap();
    assert_eq!(
        crate::light_defaults(&plain.devices[0])
            .unwrap()
            .look_brightness,
        128
    );
    assert!(crate::LIGHT_KEYS.contains(&"look_brightness"));
}

#[test]
fn a_wash_and_a_sparkle_parse_with_their_defaults() {
    let spec = from_toml(
        "--spec",
        r#"
layers = [
  { block = "wash", color = "ffffff", envelope = { kind = "breathe", period_ms = 3000, depth = 0.55 } },
  { block = "sparkle", color = "fff4e6", base = 0.03 },
]
"#,
    )
    .expect("parses");
    let mut layers = spec.iter();
    assert!(matches!(
        layers.next().unwrap().block,
        Block::Wash {
            effect: Effect::Breathe {
                period_ms: 3_000,
                depth: 550,
                ..
            },
            ..
        }
    ));
    let Block::Sparkle(s) = layers.next().unwrap().block else {
        panic!("a sparkle");
    };
    assert_eq!(s.count, 1);
    assert_eq!(
        (s.density, s.density_end, s.fade_ms, s.start_ms),
        (1_200, 1_200, 0, 0)
    );
    assert_eq!((s.min_ms, s.max_ms, s.base, s.seed), (350, 950, 30, 0));
    assert!(!s.fall);
    assert_eq!((s.fall_speed, s.fall_accel), (5_000, 6_000));
}

#[test]
fn a_sparkle_takes_colors_or_a_colour_and_checks_its_ranges() {
    let spec = from_toml(
        "--spec",
        r#"
layers = [
  { block = "sparkle", colors = ["00ff20", "c8ffd2", "78ff8c"], density = 3.5, density_end = 0, fade_ms = 2600, start_ms = 300, fall = true },
]
"#,
    )
    .expect("parses");
    let Block::Sparkle(s) = spec.iter().next().unwrap().block else {
        panic!("a sparkle");
    };
    assert_eq!((s.count, s.density, s.density_end), (3, 3_500, 0));
    assert!(s.fall);

    let cases = [
        (
            r#"{ block = "sparkle", color = "00ff20", colors = ["00ff20"] }"#,
            "not both",
        ),
        (r#"{ block = "sparkle", density = 1.0 }"#, "needs color"),
        (
            r#"{ block = "sparkle", colors = ["00ff20","00ff20","00ff20","00ff20","00ff20"] }"#,
            "1 to 4",
        ),
        (
            r#"{ block = "sparkle", color = "00ff20", density = 70 }"#,
            "density",
        ),
        (
            r#"{ block = "sparkle", color = "00ff20", min_ms = 900, max_ms = 300 }"#,
            "min_ms",
        ),
        (
            r#"{ block = "sparkle", color = "00ff20", fall_speed = 100 }"#,
            "fall_speed",
        ),
        (
            r#"{ block = "sparkle", color = "00ff20", seed = -1 }"#,
            "seed",
        ),
        (
            r#"{ block = "sparkle", color = "00ff20", fall = "yes" }"#,
            "fall",
        ),
        (r#"{ block = "wash", color = "00ff20" }"#, "envelope"),
        (
            r#"{ block = "wash", color = "00ff20", envelope = "nope" }"#,
            "solid, blink",
        ),
    ];
    for (layer, want) in cases {
        let text = format!("layers = [{layer}]");
        let err = body_error(&text);
        assert!(err.contains(want), "{layer}: {err}");
    }
    // Two sparkles in one look are refused.
    let two = body_error(
        r#"layers = [
  { block = "sparkle", color = "00ff20" },
  { block = "sparkle", color = "ff0000" },
]"#,
    );
    assert!(two.contains("at most one sparkle"), "{two}");
}

#[test]
fn a_wash_and_a_sparkle_round_trip_through_the_file_form() {
    let spec = from_toml(
        "--spec",
        r#"
layers = [
  { block = "wash", color = "c8ffd2", envelope = { kind = "pulse", attack_ms = 80, decay_ms = 500, repeat = 1 } },
  { block = "sparkle", colors = ["00ff20", "78ff8c"], density = 3.5, density_end = 0, fade_ms = 2600, start_ms = 300, base = 0.25, seed = 9, fall = true, fall_speed = 5, fall_accel = 6 },
]
"#,
    )
    .expect("parses");
    let text = body_toml(&spec);
    let again = from_toml("round trip", &text).expect("the written form parses");
    assert_eq!(again, spec);
}

#[test]
fn a_drain_parses_with_its_defaults_round_trips_and_checks_its_ranges() {
    let spec = from_toml(
        "--spec",
        r#"layers = [{ block = "drain", color = "00ff20", fill_ms = 700, start_ms = 1100, duration_ms = 2100 }]"#,
    )
    .expect("parses");
    let Block::Drain {
        color,
        fill_ms,
        start_ms,
        duration_ms,
        easing,
    } = spec.iter().next().unwrap().block
    else {
        panic!("a drain");
    };
    assert_eq!(
        (color, fill_ms, start_ms, duration_ms),
        (Rgbw::rgb(0x00ff20), 700, 1_100, 2_100)
    );
    assert_eq!(easing, Easing::EaseIn, "ease-in by default");
    let again = from_toml("round trip", &body_toml(&spec)).expect("the written form parses");
    assert_eq!(again, spec);
    let err = body_error(r#"layers = [{ block = "drain", color = "00ff20", duration_ms = 0 }]"#);
    assert!(err.contains("duration_ms"), "{err}");
}
