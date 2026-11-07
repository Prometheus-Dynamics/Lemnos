//! `Request::AllocStats` end to end: the service's allocation counts as `lemnos-ctl stats`
//! reads them. Built with `alloc-stats`, this test binary runs on the counter (as the
//! `lemnosd` binary does) and the service reports its counts; without it the service answers
//! that it does not count.

#[cfg(feature = "alloc-stats")]
#[global_allocator]
static ALLOCATOR: lemnos_linux_sys::alloc_stats::Counter = lemnos_linux_sys::alloc_stats::Counter;

use lemnos_ipc::ClientOptions;
use lemnosd::mock::{MockHardware, MockLemnosd};

const BOARD: &str = r#"
format = "lemnos.board"
schema_version = 1

[board]
id = "alloc-stats"
"#;

#[test]
fn the_service_reports_its_allocations_when_it_counts_them() {
    let service = MockLemnosd::start(BOARD, MockHardware::new()).unwrap();
    let mut client = ClientOptions::new(service.socket(), "stats")
        .devices()
        .unwrap();
    let first = client.alloc_stats().unwrap();
    let second = client.alloc_stats().unwrap();
    service.stop();
    if cfg!(feature = "alloc-stats") {
        let (a, b) = (first.unwrap(), second.unwrap());
        assert!(a.allocs > 0 && a.frees > 0 && a.live_bytes > 0, "{a:?}");
        assert!(a.live_bytes <= a.bytes, "{a:?}");
        assert!(
            b.allocs >= a.allocs && b.bytes >= a.bytes,
            "{a:?} then {b:?}"
        );
    } else {
        assert_eq!((first, second), (None, None));
    }
}
