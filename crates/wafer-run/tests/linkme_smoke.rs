//! Regression test for the `inventory` → `linkme` switch.
//!
//! The original bug: with LTO and `inventory::submit!`, LLVM could eliminate
//! the inventory `.init_array` entries as dead code when no reachable path
//! read the registered statics. Verified with `nm` on a release solobase
//! binary — zero of the 7 expected `wafer-run/*` registrations landed;
//! runtime panicked with `BlockNotFound { name: "wafer-run/router" }`.
//!
//! `linkme` uses a custom ELF section (`#[link_section]`). The linker merges
//! sections by name regardless of reachability analysis, so LTO cannot
//! eliminate the entries.
//!
//! This test asserts that `wafer-block-cors` — a standalone wafer-block-*
//! crate — registers its block via linkme when explicitly included as a
//! dependency. The `extern crate` declaration below is the replacement for
//! the deleted `register()` call: it forces the linker to include the crate's
//! object file, at which point linkme's section-based registration lands.
//!
//! In production binaries, each `wafer-block-*` crate appears in
//! `[dependencies]` in the consumer's Cargo.toml, which similarly forces
//! the linker to include the rlib.

// Force the linker to include wafer-block-cors's object file so that its
// linkme section entries are merged into this binary's section.
extern crate wafer_block_cors;

#[test]
fn standalone_crate_block_registers() {
    // wafer-block-cors is a standalone crate added as a dev-dep for this test.
    // The `extern crate` above ensures the linker includes its object file.
    // With linkme, the `register_static_block!` entry in wafer-block-cors
    // lands in STATIC_BLOCK_REGISTRATIONS regardless of any code-level
    // reference to the specific registration static — that's the invariant
    // linkme provides over inventory.
    let w = wafer_run::Wafer::builder()
        .disable_lockfile()
        .build()
        .expect("Wafer::new");
    assert!(
        w.has_block("wafer-run/cors"),
        "cors block must register via linkme once the crate is linked — \
         this is the inventory→linkme regression test"
    );
}
