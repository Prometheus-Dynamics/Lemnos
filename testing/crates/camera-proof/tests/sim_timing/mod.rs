//! The latch's clock in the simulator: the clock moves only when the simulated sensor makes a start,
//! and a wait steps the sensor through every start due by the time waited for. Shared by the
//! latch tests, which call the same `latch::run` the board's `lemnos-sensor-proof latch` calls.

use lemnos_camera_proof::latch::Timing;

pub struct SimTiming;

impl Timing for SimTiming {
    fn now_ns(&self) -> u64 {
        lemnos_camera_sim::time::now()
    }

    fn wait_until(&self, ns: u64) {
        let period = lemnos_camera_sim::period_ns();
        while lemnos_camera_sim::time::peek_start(period) <= ns {
            lemnos_camera_sim::sensor_step();
        }
        lemnos_camera_sim::time::advance_to(ns);
    }

    fn before_frame(&self) {
        lemnos_camera_sim::sensor_step();
    }
}
