//! The example look set (`examples/looks`) reproduces the built-in looks it
//! replaced, frame for frame. The recorded frames are the golden frames of
//! `lemnos-light` (`golden.txt`), taken at the default settings: each named
//! look the example set defines is rendered through the look table a
//! daemon builds from the set (the look files, then the scheme preset) and
//! hashed at the same times as the golden test does.

use lemnos_board::BoardDefinition;
use lemnos_light::{Animator, Defaults, EffectKind, Intent, LookName, Phase, Show, SystemState};
use lemnosd::looks::LookTable;
use lemnosd::presets::PresetDir;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

const LEDS: usize = 16;
const TIMES: [u64; 11] = [0, 1, 40, 125, 250, 600, 1000, 1500, 2200, 3000, 4800];
const GOLDEN: &str = include_str!("../../lemnos-light/src/golden.txt");
const EXAMPLES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/looks");

fn fnv(bytes: &[u8], mut hash: u64) -> u64 {
    for b in bytes {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// One scenario's lines, as `golden.txt` writes them.
fn render(name: &str, intent: Intent<LEDS>, table: &LookTable, out: &mut String) {
    let defaults = Defaults::default();
    let (look, transition) = intent.resolve(&defaults, &|n: &str| table.get(n));
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

/// The golden scenarios that the example set defines (the named looks).
fn scenarios() -> Vec<(&'static str, Intent<LEDS>)> {
    let updating = |progress, phase| SystemState::Updating { progress, phase };
    let mut out = vec![
        (
            "sys-verifying",
            Intent::new(Show::System(updating(None, Phase::Verifying))),
        ),
        (
            "sys-writing-unknown",
            Intent::new(Show::System(updating(None, Phase::Writing))),
        ),
        (
            "sys-applying-unknown",
            Intent::new(Show::System(updating(None, Phase::Applying))),
        ),
        (
            "sys-writing-300",
            Intent::new(Show::System(updating(Some(300), Phase::Writing))),
        ),
        (
            "sys-applying-900",
            Intent::new(Show::System(updating(Some(900), Phase::Applying))),
        ),
        (
            "sys-staged",
            Intent::new(Show::System(updating(None, Phase::Staged))),
        ),
        (
            "sys-staged-500",
            Intent::new(Show::System(updating(Some(500), Phase::Staged))),
        ),
        (
            "sys-booting",
            Intent::new(Show::System(SystemState::Booting)),
        ),
        (
            "sys-rebooting",
            Intent::new(Show::System(SystemState::Rebooting)),
        ),
        (
            "sys-update-failed",
            Intent::new(Show::System(SystemState::UpdateFailed)),
        ),
        (
            "sys-rolled-back",
            Intent::new(Show::System(SystemState::RolledBack)),
        ),
        (
            "sys-confirmed",
            Intent::new(Show::System(SystemState::Confirmed)),
        ),
        ("locate", Intent::new(Show::Locate)),
    ];
    let mut locate_blink = Intent::new(Show::Locate);
    locate_blink.effect = Some(EffectKind::Blink);
    locate_blink.period_ms = Some(900);
    locate_blink.depth = Some(300);
    out.push(("locate-blink", locate_blink));
    for name in [
        "detect.targets",
        "detect.searching",
        "link.waiting",
        "link.waiting-targets",
        "detect.error",
        "detect.live",
    ] {
        out.push((
            name,
            Intent::new(Show::Look {
                name: LookName::new(name).unwrap(),
                progress: None,
            }),
        ));
    }
    out
}

/// The golden blocks, by scenario name.
fn golden() -> BTreeMap<String, String> {
    let mut blocks = BTreeMap::new();
    let mut current: Option<(String, String)> = None;
    for line in GOLDEN.lines() {
        if line.starts_with("  ") {
            if let Some((_, text)) = current.as_mut() {
                text.push_str(line);
                text.push('\n');
            }
        } else {
            if let Some((name, text)) = current.take() {
                blocks.insert(name, text);
            }
            current = Some((line.to_string(), format!("{line}\n")));
        }
    }
    if let Some((name, text)) = current {
        blocks.insert(name, text);
    }
    blocks
}

/// The example set as a daemon loads it: the look files, and the scheme B
/// preset saved and active.
fn example_table() -> LookTable {
    let board = BoardDefinition::from_toml_str(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"example\"\n",
    )
    .unwrap();
    static CALLS: AtomicUsize = AtomicUsize::new(0);
    let call = CALLS.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "lemnosd-example-looks-{}-{call}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let presets = root.join("presets");
    std::fs::create_dir_all(&presets).unwrap();
    std::fs::copy(
        format!("{EXAMPLES}/presets/scheme-b.toml"),
        presets.join("scheme-b.toml"),
    )
    .unwrap();
    let dir = PresetDir::new(&root);
    dir.set_active("scheme-b").unwrap();
    let looks = PathBuf::from(format!("{EXAMPLES}/looks.d"));
    let mut table = LookTable::new(&board, Some(&looks), None);
    table.use_presets(dir);
    assert_eq!(table.active_preset(), Some("scheme-b"));
    table
}

#[test]
fn the_example_set_reproduces_the_built_in_looks_frame_for_frame() {
    let table = example_table();
    let recorded = golden();
    let mut checked = 0;
    for (name, intent) in scenarios() {
        let mut out = String::new();
        render(name, intent, &table, &mut out);
        let expected = recorded
            .get(name)
            .unwrap_or_else(|| panic!("{name} is not in golden.txt"));
        assert_eq!(&out, expected, "{name} differs from the built-in look");
        checked += 1;
    }
    assert_eq!(checked, 20);
}

#[test]
fn the_example_set_defines_exactly_the_moved_looks() {
    let table = example_table();
    for (name, _) in scenarios() {
        if name.starts_with("sys-") || name == "locate" || name == "locate-blink" {
            continue;
        }
        assert!(table.knows(name), "{name}");
    }
    assert!(table.knows("system.locate"));
    assert!(table.knows("system.rolled-back"));
    assert!(table.get("status.ok").is_none(), "status.* stay built in");
}

#[test]
fn the_scheme_b_preset_is_saved_and_active() {
    let table = example_table();
    let listing = table.preset_list();
    assert!(listing.contains("* scheme-b (saved)"), "{listing}");
}
