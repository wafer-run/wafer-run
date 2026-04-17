//! Runtime validators: block interface action checks and required-config presence checks.
//!
//! Pure functions — no mutation of runtime state. Called from `Wafer::resolve()`
//! (config presence) and `RuntimeContext::call_block()` (interface action).
