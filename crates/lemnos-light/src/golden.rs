//! Golden frames: every built-in look, rendered on a 16-LED ring at fixed
//! times, hashed frame by frame, against `golden.txt`. The hashes pin the
//! rendering, so a change to the look language or the defaults shows up here.

extern crate std;
use std::{format, string::String, vec::Vec};

use crate::Phase;
use crate::{Animator, Defaults, EffectKind, Intent, LookName, Rgbw, Show, Status, SystemState};

const LEDS: usize = 16;
const TIMES: [u64; 11] = [0, 1, 40, 125, 250, 600, 1000, 1500, 2200, 3000, 4800];

fn fnv(bytes: &[u8], mut hash: u64) -> u64 {
    for b in bytes {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn run(name: &str, intent: Intent<LEDS>, defaults: &Defaults, out: &mut String) {
    let (look, transition) = intent.resolve_builtin(defaults);
    let mut animator = Animator::<LEDS>::new(LEDS);
    animator.set(look, transition, 0);
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

fn scenarios(defaults: &Defaults, out: &mut String) {
    let d = defaults;
    for (name, status) in [
        ("status-ok", Status::Ok),
        ("status-warn", Status::Warn),
        ("status-error", Status::Error),
        ("status-busy", Status::Busy),
        ("status-off", Status::Off),
    ] {
        run(name, Intent::new(Show::Status(status)), d, out);
    }
    let mut breathe = Intent::new(Show::Status(Status::Error));
    breathe.effect = Some(EffectKind::Breathe);
    breathe.period_ms = Some(4000);
    breathe.depth = Some(180);
    run("status-error-breathe", breathe, d, out);
    run(
        "color",
        Intent::new(Show::Color(Rgbw::rgb(0x2f7bff))),
        d,
        out,
    );
    let mut color_breathe = Intent::new(Show::Color(Rgbw::rgb(0x2f7bff)));
    color_breathe.effect = Some(EffectKind::Breathe);
    color_breathe.period_ms = Some(4000);
    color_breathe.depth = Some(180);
    run("color-breathe", color_breathe, d, out);
    let mut frame = [Rgbw::OFF; LEDS];
    for (i, p) in frame.iter_mut().enumerate() {
        *p = Rgbw::rgb((i as u32 * 0x111111) & 0xffffff);
    }
    run(
        "frame",
        Intent::new(Show::Frame {
            pixels: frame,
            len: LEDS,
        }),
        d,
        out,
    );
    run(
        "progress-default",
        Intent::new(Show::Progress {
            fraction: 300,
            color: None,
            background: None,
        }),
        d,
        out,
    );
    run(
        "progress-colored",
        Intent::new(Show::Progress {
            fraction: 650,
            color: Some(Rgbw::rgb(0xff8000)),
            background: Some(Rgbw::rgb(0x202020)),
        }),
        d,
        out,
    );
    run(
        "indeterminate",
        Intent::new(Show::Indeterminate { color: None }),
        d,
        out,
    );
    run(
        "orbit-one",
        Intent::new(Show::Orbit {
            color: None,
            tail: None,
            heads: 1,
            base: None,
        }),
        d,
        out,
    );
    run(
        "orbit-two",
        Intent::new(Show::Orbit {
            color: Some(Rgbw::rgb(0x2bd47d)),
            tail: Some(6000),
            heads: 2,
            base: Some(60),
        }),
        d,
        out,
    );
    let states = [
        (
            "sys-verifying",
            SystemState::Updating {
                progress: None,
                phase: Phase::Verifying,
            },
        ),
        (
            "sys-writing-unknown",
            SystemState::Updating {
                progress: None,
                phase: Phase::Writing,
            },
        ),
        (
            "sys-applying-unknown",
            SystemState::Updating {
                progress: None,
                phase: Phase::Applying,
            },
        ),
        (
            "sys-writing-300",
            SystemState::Updating {
                progress: Some(300),
                phase: Phase::Writing,
            },
        ),
        (
            "sys-applying-900",
            SystemState::Updating {
                progress: Some(900),
                phase: Phase::Applying,
            },
        ),
        (
            "sys-staged",
            SystemState::Updating {
                progress: None,
                phase: Phase::Staged,
            },
        ),
        (
            "sys-staged-500",
            SystemState::Updating {
                progress: Some(500),
                phase: Phase::Staged,
            },
        ),
        ("sys-booting", SystemState::Booting),
        ("sys-rebooting", SystemState::Rebooting),
        ("sys-update-failed", SystemState::UpdateFailed),
        ("sys-rolled-back", SystemState::RolledBack),
        ("sys-confirmed", SystemState::Confirmed),
    ];
    for (name, state) in states {
        run(name, Intent::new(Show::System(state)), d, out);
    }
    run("locate", Intent::new(Show::Locate), d, out);
    let mut locate_blink = Intent::new(Show::Locate);
    locate_blink.effect = Some(EffectKind::Blink);
    locate_blink.period_ms = Some(900);
    locate_blink.depth = Some(300);
    run("locate-blink", locate_blink, d, out);
    for name in [
        "pv.targets",
        "pv.searching",
        "pv.no-nt",
        "pv.no-nt-targets",
        "pv.error",
        "pv.vision",
    ] {
        let look = LookName::new(name).unwrap();
        run(
            name,
            Intent::new(Show::Look {
                name: look,
                progress: None,
            }),
            d,
            out,
        );
    }
    let mut chase = Defaults {
        locate_effect: EffectKind::Chase,
        ..*d
    };
    chase.spinner_tail = 4;
    run("locate-chase", Intent::new(Show::Locate), &chase, out);
}

/// The built-in looks render as recorded in `golden.txt`. A deliberate change
/// to a built-in look is recorded with `LEMNOS_GOLDEN_UPDATE=<file>`.
#[test]
fn built_in_looks_render_as_recorded() {
    let mut out = String::new();
    scenarios(&Defaults::default(), &mut out);
    if let Ok(path) = std::env::var("LEMNOS_GOLDEN_UPDATE") {
        std::fs::write(path, &out).unwrap();
        return;
    }
    assert_eq!(out, include_str!("golden.txt"));
}
