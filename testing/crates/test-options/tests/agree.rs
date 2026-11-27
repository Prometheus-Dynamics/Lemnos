//! The copy engine/host's tests keep gives the same options as this crate.

mod host_copy {
    include!("../../../../engine/host/tests/test_options/mod.rs");
}

#[test]
fn engine_hosts_copy_has_the_same_deadline() {
    assert_eq!(host_copy::CALL_DEADLINE, lemnos_test_options::CALL_DEADLINE);
    assert_eq!(host_copy::options(), lemnos_test_options::options());
}

#[test]
fn only_the_deadline_differs_from_the_default() {
    let mut options = lemnos_test_options::options();
    assert_ne!(options, lemnos_host::Options::default());
    options.call_deadline = lemnos_host::Options::default().call_deadline;
    assert_eq!(options, lemnos_host::Options::default());
}
