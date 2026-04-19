#[test]
fn compile_fail_diagnostics_cover_invalid_driver_and_configured_device_shapes() {
    let tests = trybuild::TestCases::new();
    tests.compile_fail("tests/ui/*.rs");
}
