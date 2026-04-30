//! Integration test for `wafer dev`. Runs against a fake-app fixture that
//! emits the three tracing events the real runtime emits.

use std::{process::Stdio, time::Duration};

use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
    time::timeout,
};

const FIXTURE_DIR: &str = "tests/fixtures/dev-fake-app";

#[tokio::test(flavor = "multi_thread")]
async fn dev_emits_banner_against_fake_app() {
    // Pre-build the fixture so the dev loop's first cargo build is fast.
    let pre = Command::new("cargo")
        .arg("build")
        .current_dir(FIXTURE_DIR)
        .status()
        .await
        .expect("pre-build fixture");
    assert!(pre.success(), "fixture pre-build failed");

    // Run wafer-cli dev pointing into the fixture directory.
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_wafer"));
    cmd.arg("dev")
        .arg("--debounce")
        .arg("100")
        .arg("--kill-timeout")
        .arg("1")
        .current_dir(FIXTURE_DIR)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn().expect("spawn wafer dev");

    // Read stderr until we see the banner or the timeout fires.
    let stderr = child.stderr.take().expect("stderr");
    let mut reader = BufReader::new(stderr).lines();

    let banner_seen = timeout(Duration::from_secs(60), async {
        while let Ok(Some(line)) = reader.next_line().await {
            if line.contains("[wafer dev] ✓ wafer dev") && line.contains("9999") {
                return true;
            }
        }
        false
    })
    .await
    .unwrap_or(false);

    // Tear down.
    let _ = child.kill().await;
    let _ = child.wait().await;

    assert!(
        banner_seen,
        "expected boot banner with port 9999 in wafer dev stderr"
    );
}
