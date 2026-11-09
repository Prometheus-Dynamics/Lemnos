//! The steady-state frames: encoded and decoded with no allocation.

use lemnos_measure::test_alloc::{Counting, allocations};
use lemnos_wire::{Frame, append, decode, encode};

#[global_allocator]
static ALLOCATOR: Counting = Counting;

#[test]
fn calls_replies_and_values_allocate_nothing() {
    let args = [1u8; 32];
    let frames = [
        Frame::Call {
            id: 1,
            device: 2,
            contract: 0,
            method: 1,
            reply_capacity: 64,
            args: &args,
        },
        Frame::Reply {
            id: 1,
            bytes: &args,
        },
        Frame::Value {
            subscription: 1,
            stamp_ns: 9,
            bytes: &args,
        },
    ];
    let mut buf = [0u8; 256];
    let mut reused = Vec::with_capacity(4096);
    let (_, count) = allocations(|| {
        for _ in 0..1000 {
            for frame in &frames {
                let n = encode(frame, &mut buf).unwrap();
                let (back, _) = decode(&buf[..n]).unwrap().unwrap();
                assert_eq!(back.kind(), frame.kind());
                reused.clear();
                append(frame, &mut reused).unwrap();
            }
        }
    });
    assert_eq!(count, 0);
}
