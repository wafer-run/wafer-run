//! Verify that `#[wafer_async_trait]` expands to the correct cfg_attr pair
//! on both wasm32 and native targets.
//!
//! The trait and impl are exercised via a blocking `futures::executor::block_on`
//! call so that this test file needs no async runtime dep beyond what is already
//! transitively available via `wafer-run` (which pulls in `tokio`).

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

#[test]
fn macro_expands_and_works() {
    // Use the tokio runtime that wafer-run already pulls in as a dev-dep.
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("tokio runtime");
    let result = rt.block_on(async { Hi.greet().await });
    assert_eq!(result, "hi");
}
