#[test]
fn compile() {
    let cases = trybuild::TestCases::new();
    cases.pass("tests/pass/*.rs");
    cases.compile_fail("tests/fail/*.rs");
}
