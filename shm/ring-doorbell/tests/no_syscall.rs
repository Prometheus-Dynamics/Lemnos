//! Ringing a subscriber that is awake makes no system call: a child under a seccomp filter that
//! kills it on any system call but `exit` publishes and rings, and exits cleanly.

use shm_measure::sched::{SystemCalls, without_system_calls};
use shm_ring::{FieldId, Flags, Ring, SampleTime, Spec};
use shm_ring_doorbell::Doorbell;

fn ring_of_one() -> Ring {
    Ring::create(&Spec {
        fields: 1,
        history: 8,
        max_payload: 64,
        expiry_ms: 0,
    })
    .expect("ring")
}

#[test]
fn publishing_to_an_awake_subscriber_makes_no_system_call() {
    let ring = ring_of_one();
    let mut writer = ring.writer().expect("writer");
    let doorbell = Doorbell::create().expect("doorbell");
    let mut bell = doorbell.bell().expect("bell");
    let ended = without_system_calls(|| {
        for seq in 0..20_000_u64 {
            if writer
                .publish(
                    FieldId(0),
                    SampleTime::from_nanos(seq),
                    Flags::NONE,
                    &[1; 64],
                )
                .is_err()
                || bell.ring()
            {
                return false;
            }
        }
        true
    });
    assert_eq!(ended, SystemCalls::None);
}

#[test]
fn ringing_a_sleeping_subscriber_is_the_one_system_call() {
    // The same child, but the subscriber is asleep: the ring is a `write`, and the filter kills.
    let ring = ring_of_one();
    let mut writer = ring.writer().expect("writer");
    let mut doorbell = Doorbell::create().expect("doorbell");
    let mut bell = doorbell.bell().expect("bell");
    doorbell.arm();
    let ended = without_system_calls(|| {
        let _ = writer.publish(FieldId(0), SampleTime::ZERO, Flags::NONE, &[1; 64]);
        // Reached only if the ring made no system call, which would be the failure.
        !bell.ring()
    });
    assert_eq!(ended, SystemCalls::Made);
}
