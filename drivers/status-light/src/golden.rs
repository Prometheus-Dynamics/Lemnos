//! Golden frames: the built-in and example looks rendered on a 16-LED ring at fixed times, hashed
//! frame by frame, against `golden.txt`. The hashes are the ones the reference renderer records for
//! the same looks at the default settings, so a port that renders differently shows up here.

extern crate std;
use std::{format, string::String, vec::Vec};

use crate::animator::{Animator, Transition};
use crate::builtin::StatusLook;
use crate::easing::Easing;
use crate::look::{Envelope, Rgbw};
use crate::settings::Settings;
use crate::wire::{Spec, resolve};

const LEDS: usize = 16;
const TIMES: [u64; 11] = [0, 1, 40, 125, 250, 600, 1000, 1500, 2200, 3000, 4800];

fn fnv(bytes: &[u8], mut hash: u64) -> u64 {
    for b in bytes {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Renders `spec` at the default settings, and records each frame that changed.
fn run(name: &str, spec: Spec, out: &mut String) {
    let settings = Settings::from_config(&lemnos_core::Config::new(&[(
        "count",
        lemnos_core::ConfigValue::Int(LEDS as i64),
    )]))
    .unwrap();
    let look = resolve(&spec, &settings);
    let mut animator = Animator::new(LEDS);
    animator.set(look, Transition::new(settings.fade_ms, settings.easing), 0);
    out.push_str(&format!("{name}\n"));
    for t in TIMES {
        if let Some(frame) = animator.render(t) {
            let mut bytes = Vec::new();
            for p in frame {
                bytes.extend_from_slice(&[p.r, p.g, p.b, p.w]);
            }
            out.push_str(&format!(
                "  t={t} {:016x}\n",
                fnv(&bytes, 0xcbf2_9ce4_8422_2325)
            ));
        }
    }
}

/// The scenarios, in the order `golden.txt` records them.
fn scenarios(out: &mut String) {
    for (name, status) in [
        ("status-ok", StatusLook::Ok),
        ("status-warn", StatusLook::Warn),
        ("status-error", StatusLook::Error),
        ("status-busy", StatusLook::Busy),
        ("status-off", StatusLook::Off),
    ] {
        run(name, Spec::Status(status), out);
    }
    run(
        "status-error-breathe",
        Spec::Fill {
            color: Rgbw::rgb(0xff0000),
            brightness: crate::builtin::FULL_FILL,
            envelope: Envelope::Breathe {
                period_ms: 4000,
                depth: 180,
                easing: Easing::EaseInOut,
            },
        },
        out,
    );
    run(
        "color",
        Spec::Fill {
            color: Rgbw::rgb(0x2f7bff),
            brightness: crate::builtin::FULL_FILL,
            envelope: Envelope::Solid,
        },
        out,
    );
    run(
        "color-breathe",
        Spec::Fill {
            color: Rgbw::rgb(0x2f7bff),
            brightness: crate::builtin::FULL_FILL,
            envelope: Envelope::Breathe {
                period_ms: 4000,
                depth: 180,
                easing: Easing::EaseInOut,
            },
        },
        out,
    );
    let spinner = Spec::Comet {
        color: Rgbw::rgb(0x00ff40),
        brightness: 255,
        period_ms: 1200,
        tail: 5000,
        heads: 1,
        base: 180,
        envelope: Envelope::Solid,
    };
    run("indeterminate", spinner, out);
    run("orbit-one", spinner, out);
    run(
        "orbit-two",
        Spec::Comet {
            color: Rgbw::rgb(0x2bd47d),
            brightness: 255,
            period_ms: 1200,
            tail: 6000,
            heads: 2,
            base: 60,
            envelope: Envelope::Solid,
        },
        out,
    );
}

/// The looks render as recorded in `golden.txt`.
#[test]
fn looks_render_as_recorded() {
    let mut out = String::new();
    scenarios(&mut out);
    assert_eq!(out, include_str!("golden.txt"));
}
