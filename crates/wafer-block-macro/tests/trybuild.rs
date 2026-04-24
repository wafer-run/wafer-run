//! Macro expansion checks via `trybuild`.

#[test]
fn pass_with_new() {
    let t = trybuild::TestCases::new();
    t.pass("tests/fixtures/pass_with_new.rs");
}

#[test]
fn fail_missing_new() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/fixtures/fail_missing_new.rs");
}
