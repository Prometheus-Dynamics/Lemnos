//! Steady state allocates nothing: a blink, a chase and a bar ticked for a long while.

mod support;

use lemnos_contract_indicator::{Indicator as _, NO_PROGRESS};
use lemnos_core::ConfigValue;
use lemnos_measure::test_alloc::{Counting, allocations};
use support::{FakePixels, driver, settle, tick};

#[global_allocator]
static ALLOCATOR: Counting = Counting;

#[test]
fn steady_state_allocates_nothing() {
    let fake = FakePixels::leak(24);
    let mut indicator = driver(
        fake,
        &[
            (
                "state.receiving",
                ConfigValue::Str("blink #0040ff period=800"),
            ),
            (
                "state.verifying",
                ConfigValue::Str("chase #ffc000 period=1200"),
            ),
            ("state.writing", ConfigValue::Str("bar #00c040 off=#001000")),
        ],
    );
    // Warm up: the first draw and every state's first change.
    settle(&mut indicator, 0, 10_000);
    let ((), count) = allocations(|| {
        for (state, progress) in [
            ("receiving", NO_PROGRESS),
            ("verifying", NO_PROGRESS),
            ("writing", 420),
        ] {
            indicator.set(state, progress).unwrap();
            let mut now = 0;
            let mut next = tick(&mut indicator, now);
            for _ in 0..2_000 {
                let Some(at) = next else { break };
                now = at;
                next = tick(&mut indicator, now);
            }
            let _ = now;
        }
    });
    assert_eq!(count, 0, "allocations in steady state");
}
