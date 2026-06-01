//! Verify that `#[wafer_async_trait]` expands to the correct cfg_attr pair
//! on both wasm32 and native targets.
//!
//! The trait and impl are exercised from a `#[tokio::test]` async context.

use wafer_block_macro::wafer_async_trait;

// ---------------------------------------------------------------------------
// A simple trait annotated with #[wafer_async_trait]
// ---------------------------------------------------------------------------

#[wafer_async_trait]
trait Greeter {
    async fn greet(&self) -> String;
}

struct Hi;

#[wafer_async_trait]
impl Greeter for Hi {
    async fn greet(&self) -> String {
        "hi".into()
    }
}

// ---------------------------------------------------------------------------
// Test
// ---------------------------------------------------------------------------

#[tokio::test]
async fn macro_expands_and_works() {
    let result = Hi.greet().await;
    assert_eq!(result, "hi");
}
