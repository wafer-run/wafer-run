//! End-to-end pipeline test for the wafer.toml-converged flow:
//! `wafer new` → (simulated build) → `wafer package` → `wafer publish`.
//!
//! The real toolchain build is skipped (no cargo/wasm32 target in CI test
//! env); instead a valid block.wasm is synthesized from WAT with the same
//! exports + `__wafer_info` payload a real build would produce.

#[path = "util/mod.rs"]
mod util;

use std::io::Read;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_wafer")
}

#[test]
fn new_package_publish_pipeline_converges_on_wafer_toml() {
    let tmp = tempfile::tempdir().unwrap();

    // 1. `wafer new acme/widget` scaffolds wafer.toml (no manifest.json).
    let out = std::process::Command::new(bin())
        .current_dir(tmp.path())
        .args(["new", "acme/widget"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "wafer new failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let proj = tmp.path().join("widget");
    assert!(proj.join("wafer.toml").is_file(), "wafer.toml not written");
    assert!(
        !proj.join("manifest.json").exists(),
        "manifest.json must no longer be scaffolded"
    );
    let toml_body = std::fs::read_to_string(proj.join("wafer.toml")).unwrap();
    assert!(toml_body.contains("org = \"acme\""), "{toml_body}");
    assert!(toml_body.contains("name = \"widget\""), "{toml_body}");
    assert!(toml_body.contains("abi = 1"), "{toml_body}");

    // 2. Simulate `wafer build` output: a valid block.wasm whose
    //    __wafer_info name matches wafer.toml's {org}/{name}.
    std::fs::create_dir_all(proj.join("target")).unwrap();
    std::fs::write(
        proj.join("target/block.wasm"),
        util::block_wasm("acme/widget", ""),
    )
    .unwrap();
    std::fs::write(proj.join("README.md"), "# widget\n").unwrap();

    // 3. `wafer package` writes the tarball at the publish default path.
    let out = std::process::Command::new(bin())
        .current_dir(&proj)
        .arg("package")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "wafer package failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let tarball_path = proj.join("target/wafer/widget-0.1.0.wafer");
    assert!(
        tarball_path.is_file(),
        "tarball missing at the default publish path"
    );
    assert!(
        !proj.join("dist").exists(),
        "dist/ output is retired; package must not create it"
    );

    // 4. Tarball contents match what the registry server validates:
    //    wafer.toml + exactly one .wasm + optional README.md.
    let bytes = std::fs::read(&tarball_path).unwrap();
    let mut names = Vec::new();
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(&bytes[..]));
    for entry in archive.entries().unwrap() {
        let mut entry = entry.unwrap();
        names.push(entry.path().unwrap().display().to_string());
        if entry.path().unwrap().to_string_lossy() == "wafer.toml" {
            let mut s = String::new();
            entry.read_to_string(&mut s).unwrap();
            assert_eq!(s, toml_body, "wafer.toml must be shipped verbatim");
        }
    }
    names.sort();
    assert_eq!(names, ["README.md", "block.wasm", "wafer.toml"]);

    // 5. `wafer publish --dry-run` finds the tarball without --file.
    let out = std::process::Command::new(bin())
        .current_dir(&proj)
        .args(["publish", "--dry-run"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "wafer publish --dry-run failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("acme/widget@0.1.0"), "{stdout}");
}

#[test]
fn package_rejects_name_mismatch_against_wafer_toml() {
    let tmp = tempfile::tempdir().unwrap();
    let out = std::process::Command::new(bin())
        .current_dir(tmp.path())
        .args(["new", "acme/widget"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let proj = tmp.path().join("widget");

    std::fs::create_dir_all(proj.join("target")).unwrap();
    std::fs::write(
        proj.join("target/block.wasm"),
        util::block_wasm("other/name", ""),
    )
    .unwrap();

    let out = std::process::Command::new(bin())
        .current_dir(&proj)
        .arg("package")
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("name mismatch"), "{stderr}");
}
