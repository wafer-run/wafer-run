//! Test fixture for `wafer dev` integration tests. Emits the same structured
//! tracing events the real wafer-run runtime emits at boot, then sleeps so
//! the supervisor's "running" state is observable.

#[tokio::main]
async fn main() {
    // Emit to stderr (tracing's default), with ANSI off so the parser can
    // grep cleanly. Match the field shape the parser expects.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    // flow_registered fires BEFORE starting in the real runtime
    // (add_flow_json runs before Wafer::start()), so we mirror that order.
    for flow in ["flow-a", "flow-b"] {
        tracing::info!(
            target: "wafer.runtime",
            event = "flow_registered",
            flow = %flow,
            "registered flow"
        );
    }

    tracing::info!(
        target: "wafer.runtime",
        event = "starting",
        blocks = 4,
        "wafer runtime starting"
    );

    tracing::info!(
        target: "wafer.runtime",
        event = "listening",
        addr = %"0.0.0.0:9999",
        "wafer-run/http-listener listening"
    );

    // Stay alive so the supervisor sees us in RUNNING state.
    tokio::time::sleep(std::time::Duration::from_secs(60)).await;
}
