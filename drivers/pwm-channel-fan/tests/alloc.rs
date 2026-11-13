//! The steady state allocates nothing: duty changes, ticks, tachometer edges and reads, on a
//! fan that is running.

use std::sync::Arc;
use std::time::Duration;

use lemnos_core::MethodId;
use lemnos_driver_pwm_channel_fan::PwmChannelFan;
use lemnos_host::board::parse;
use lemnos_host::{Host, Options, entry};
use lemnos_measure::test_alloc::{Counting, allocations};
use lemnos_sim::SimBoard;

#[global_allocator]
static ALLOCATOR: Counting = Counting;

const MS: u64 = 1_000_000;

#[test]
fn a_running_fan_with_a_tachometer_allocates_nothing_in_steady_state() {
    let sim = SimBoard::new();
    let clock = sim.clock();
    let _pwm = sim.add_pwm("pwm0");
    let tach = sim.add_line("tach0");
    let text = "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"alloc\"\n\
                [[devices]]\nid = \"fan\"\ndriver = \"pwm-channel-fan\"\n\
                uses = { pwm = \"pwm0\", tach = \"tach0\" }\n\
                config = { kick_ms = 200, min_duty = 100 }\n";
    let host = Host::start(
        &parse(text).unwrap(),
        &[entry::<PwmChannelFan>()],
        Box::new(sim.clone()),
        Arc::new(clock.clone()),
        Options::default(),
    )
    .unwrap();
    let index = host.device("fan").unwrap().index;
    let set = MethodId {
        contract: 0,
        method: 1,
    };
    let rpm = MethodId {
        contract: 0,
        method: 3,
    };
    let mut reply = [0u8; 64];

    // Edges for the whole run, queued before it starts: 5 ms a pulse, rising and falling.
    let base = clock.now_ns();
    for k in 0..4000u64 {
        tach.edge(true, base + k * 5 * MS);
        tach.edge(false, base + k * 5 * MS + 2 * MS);
    }

    // Warm up: the first kick, the first writes and the first reads.
    for n in 0..20u16 {
        host.call(index, set, &(100 + n * 10).to_le_bytes(), &mut reply)
            .unwrap();
        clock.advance(Duration::from_millis(100));
        host.run_ticks();
        host.call(index, rpm, &[], &mut reply).unwrap();
    }

    let ((), count) = allocations(|| {
        for n in 0..1000u16 {
            let duty: u16 = if n % 2 == 0 { 300 } else { 700 };
            host.call(index, set, &duty.to_le_bytes(), &mut reply)
                .unwrap();
            clock.advance(Duration::from_millis(100));
            host.run_ticks();
            host.call(index, rpm, &[], &mut reply).unwrap();
        }
    });
    assert_eq!(count, 0, "allocations in 1000 steady-state rounds");
}
