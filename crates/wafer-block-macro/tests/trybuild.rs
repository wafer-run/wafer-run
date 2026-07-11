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

#[test]
fn fail_invalid_block_name() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/fixtures/fail_invalid_block_name.rs");
}

#[test]
fn fail_caps_and_skill() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/fixtures/fail_caps_and_skill.rs");
}

#[test]
fn fail_requires_not_array() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/fixtures/fail_requires_not_array.rs");
}
