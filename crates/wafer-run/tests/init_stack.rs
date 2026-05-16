//! Init-stack cycle detection.
//!
//! Spec: docs/superpowers/specs/2026-05-15-lazy-block-init-design.md §3
//! "cycle handling"

use wafer_run::runtime::init_stack::InitStack;

#[test]
fn fresh_stack_is_empty() {
    let stack = InitStack::new();
    assert_eq!(stack.snapshot(), Vec::<String>::new());
}

#[test]
fn push_pop_round_trip() {
    let stack = InitStack::new();
    let _g = stack.push("a/b").expect("a/b should not be on stack");
    assert_eq!(stack.snapshot(), vec!["a/b".to_string()]);
    drop(_g);
    assert_eq!(stack.snapshot(), Vec::<String>::new());
}

#[test]
fn nested_pushes_preserve_order() {
    let stack = InitStack::new();
    let _a = stack.push("a/b").expect("a/b not present");
    let _b = stack.push("c/d").expect("c/d not present");
    let _c = stack.push("e/f").expect("e/f not present");
    assert_eq!(
        stack.snapshot(),
        vec!["a/b".to_string(), "c/d".to_string(), "e/f".to_string()],
    );
}

#[test]
fn re_pushing_same_block_returns_cycle_path() {
    let stack = InitStack::new();
    let _a = stack.push("a/b").expect("a/b not present");
    let _b = stack.push("c/d").expect("c/d not present");

    let cycle_err = stack.push("a/b").expect_err("re-push should fail");
    assert_eq!(
        cycle_err,
        vec!["a/b".to_string(), "c/d".to_string(), "a/b".to_string()],
    );

    // Stack unchanged on cycle detection
    assert_eq!(stack.snapshot(), vec!["a/b".to_string(), "c/d".to_string()],);
}
