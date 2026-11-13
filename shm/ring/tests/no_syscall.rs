//! Publishing and reading make no system call.
//!
//! A child process installs a seccomp filter that kills it on any system call other than `exit`,
//! then publishes and reads. It can only exit cleanly if neither made one. The child and its
//! filter are `shm_measure::sched::without_system_calls`.

#![allow(unsafe_code)]

use shm_measure::sched::{SystemCalls, without_system_calls};
use shm_ring::{FieldId, Flags, Next, Ring, SampleTime, Spec};

#[test]
fn publish_and_read_make_no_system_call() {
    let ring = Ring::create(&Spec {
        fields: 2,
        history: 8,
        max_payload: 512,
        expiry_ms: 0,
    })
    .expect("ring");
    let mut writer = ring.writer().expect("writer");
    let mut reader = ring.reader().expect("reader");
    let payload = [5_u8; 512];

    let ended = without_system_calls(|| {
        let mut bytes = 0_usize;
        for seq in 0..20_000_u64 {
            let field = FieldId((seq % 2) as u16);
            if writer
                .publish(field, SampleTime::from_nanos(seq), Flags::NONE, &payload)
                .is_err()
                || writer.subscribers() != 0
            {
                return false;
            }
            // Every third round the reader falls behind, so skipping is covered too.
            if seq % 3 == 0 {
                continue;
            }
            loop {
                match reader.next() {
                    Next::Sample(sample) => bytes += sample.payload.len(),
                    Next::Skipped(_) => {}
                    Next::Empty => break,
                }
            }
            bytes += reader
                .latest(field)
                .map_or(0, |sample| sample.payload.len());
        }
        bytes > 0
    });
    assert_eq!(ended, SystemCalls::None);
}

#[test]
fn the_filter_does_kill_on_a_system_call() {
    // The proof above means nothing unless a system call really ends the child.
    let ended = without_system_calls(|| {
        // SAFETY: a harmless system call.
        unsafe { libc::syscall(libc::SYS_getpid) };
        true
    });
    assert_eq!(ended, SystemCalls::Made);
}
