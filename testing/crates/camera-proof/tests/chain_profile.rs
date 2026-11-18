//! The sim tick profile names the stage and the tick that held a slow tick, in the text a failing
//! chain run prints (`profile::failure`), and names a tick still in flight with its thread's state.
//! A fake stage sleeps: it stands in for a stuck wait in the simulated chain. One test, because the
//! profile is one table per process.

use std::time::Duration;

use lemnos_camera_sim::profile;

#[test]
fn a_stalled_stage_is_named_with_its_tick_and_a_stuck_one_with_its_thread() {
    profile::reset();
    profile::timed("sim-receiver", || {});
    profile::timed("sim-receiver", || {});
    // Tick 3: the fake stage sleeps long enough to be the run's slowest tick by far.
    profile::timed("fake-stage", || {
        std::thread::sleep(Duration::from_millis(150))
    });
    profile::timed("sim-receiver", || {});

    let text = profile::failure("the chain proof failed");
    assert!(text.starts_with("the chain proof failed\n"), "{text}");
    assert!(
        text.contains("slowest tick: tick 3 of stage fake-stage, 1"),
        "the slowest tick was not named by stage and tick:\n{text}"
    );
    assert!(
        text.contains("stage fake-stage: 1 ticks, slowest 1"),
        "{text}"
    );
    assert!(text.contains("at tick 3\n"), "{text}");
    assert!(text.contains("stage sim-receiver: 3 ticks"), "{text}");
    assert!(!text.contains("in flight"), "nothing is in flight:\n{text}");

    // A stuck wait: the stage is still inside its tick when the report is made.
    profile::reset();
    let stuck = std::thread::spawn(|| {
        profile::timed("fake-stage", || {
            std::thread::sleep(Duration::from_millis(1500))
        });
    });
    std::thread::sleep(Duration::from_millis(300));
    let text = profile::report();
    assert!(
        text.contains("in flight: tick 1 of stage fake-stage for "),
        "the stuck tick was not named:\n{text}"
    );
    assert!(text.contains("wait channel"), "{text}");
    if cfg!(target_os = "linux") {
        // The thread sleeps in the kernel: its state is S (sleeping), and it is readable here.
        assert!(text.contains(", state S, "), "{text}");
    }
    stuck.join().unwrap();
    profile::reset();
}
