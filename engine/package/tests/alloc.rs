//! Steady-state calls into a package allocate nothing, on either side of the boundary, and a
//! package's driver gives back everything it was given.
//!
//! On its own in this file: the count is the whole process's.

mod common;

use std::sync::atomic::Ordering;

use common::{count, fixtures, host_on, index, method};
use lemnos_core::Status;
use lemnos_host::Options;
use lemnos_measure::test_alloc::{Counting, TOTAL};
use lemnos_package::{Loader, Mode, resources_held};
use lemnos_sim::Layout;
use lemnos_sim::SimBoard;

#[global_allocator]
static ALLOCATOR: Counting = Counting;

const DEVICES: &str = r#"
[[devices]]
id = "user"
driver = "package-fixture"
uses = { peer = "chip" }

[[devices]]
id = "chip"
driver = "package-fixture"
uses = { bus = "i2c0" }
"#;

/// The package's own count of its allocations.
fn package_allocations(host: &lemnos_host::Host, device: u32) -> u64 {
    let mut reply = [0; 8];
    assert_eq!(host.call(device, method(5), &[], &mut reply), Ok(8));
    u64::from_le_bytes(reply)
}

#[test]
fn steady_state_calls_through_a_package_allocate_nothing() {
    let mut loader = Loader::new(Mode::Open);
    loader.load(&fixtures().good).unwrap();
    let sim = SimBoard::new();
    let bus = sim.add_i2c("i2c0");
    bus.attach(0x48, Layout::Bytes);
    bus.set_bytes(0x48, 0x10, &[0xa5]);
    let (host, _) = host_on(sim, DEVICES, &loader.entries(), Options::default());
    let (user, chip) = (index(&host, "user"), index(&host, "chip"));
    // One device taken and one bus.
    assert_eq!(resources_held(), 2);

    let mut args = Vec::new();
    args.extend_from_slice(&40u32.to_le_bytes());
    args.extend_from_slice(&2u32.to_le_bytes());
    let mut reply = [0; 8];
    let round = |reply: &mut [u8; 8]| {
        assert_eq!(count(&host, chip).map(|_| ()), Ok(()));
        assert_eq!(host.call(chip, method(2), &args, reply), Ok(4));
        // Through a device handle, and through a bus handle.
        assert_eq!(host.call(user, method(6), &[], reply), Ok(4));
        assert_eq!(host.call(chip, method(7), &[0x48, 0x10], reply), Ok(1));
        // A failure is steady state too.
        assert_eq!(
            host.call(chip, method(7), &[0x21, 0x10], reply),
            Err(Status::IO)
        );
        host.run_ticks();
        assert_eq!(host.device("chip").map(|_| ()), Some(()));
    };
    // Warm up: lanes start, thread-locals are made.
    for _ in 0..3 {
        round(&mut reply);
    }
    let _ = host.devices();

    let inside = package_allocations(&host, chip);
    let before = TOTAL.load(Ordering::Relaxed);
    for _ in 0..1000 {
        assert_eq!(count(&host, chip).map(|_| ()), Ok(()));
        assert_eq!(host.call(chip, method(2), &args, &mut reply), Ok(4));
        assert_eq!(host.call(user, method(6), &[], &mut reply), Ok(4));
        assert_eq!(host.call(chip, method(7), &[0x48, 0x10], &mut reply), Ok(1));
        assert_eq!(
            host.call(chip, method(7), &[0x21, 0x10], &mut reply),
            Err(Status::IO)
        );
        host.run_ticks();
    }
    let after = TOTAL.load(Ordering::Relaxed);
    assert_eq!(after - before, 0, "the host allocated in steady state");
    assert_eq!(
        package_allocations(&host, chip) - inside,
        0,
        "the package allocated in steady state"
    );

    // Letting go of the drivers gives back what they held.
    host.shutdown();
    assert_eq!(resources_held(), 0);
}
