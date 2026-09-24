//! A block IS the name it is registered under.
//!
//! `check_access` attributes every call to the caller's registration name, so
//! grant ownership and the admin-block match must be decided against that
//! same name. A block whose `info()` names it something else — for a WASM
//! guest, bytes the guest wrote — is refused at registration, and grant
//! validation never consults the reported name.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use async_trait::async_trait;
use wafer_block::{
    core_types::Message,
    streams::{input::InputStream, output::OutputStream},
    types::{ResourceAccess, ResourceGrant, ResourceType},
    wrap::check_access,
    Block, BlockInfo,
};
use wafer_run::{RuntimeError, StaticConfigSource, Wafer};

fn wafer() -> Wafer {
    Wafer::new(Arc::new(StaticConfigSource::default())).expect("Wafer::new")
}

/// A block that reports `info` verbatim.
struct Reports(BlockInfo);

#[async_trait]
impl Block for Reports {
    fn info(&self) -> BlockInfo {
        self.0.clone()
    }

    async fn handle(
        &self,
        _ctx: &dyn wafer_block::context::Context,
        _msg: Message,
        _input: InputStream,
    ) -> OutputStream {
        OutputStream::respond(Vec::new())
    }
}

fn info(name: &str, grants: Vec<ResourceGrant>) -> BlockInfo {
    BlockInfo::new(name, "0.0.1", "test/iface@v1", "grant identity fixture").grants(grants)
}

fn assert_name_mismatch(result: Result<(), RuntimeError>, registered: &str, reported: &str) {
    match result {
        Err(RuntimeError::BlockNameMismatch {
            registered: r,
            reported: p,
        }) => {
            assert_eq!(r, registered);
            assert_eq!(p, reported);
        }
        other => panic!("expected BlockNameMismatch, got {other:?}"),
    }
}

/// The CRITICAL case: a block registered as `x/attacker` claims to be
/// `a/victim` and grants itself read-write on `a/victim`'s tables. Were the
/// claim trusted, the grant's owner would match the reported name and
/// `check_access(Some("x/attacker"), "a__victim__users", Write)` would pass.
#[tokio::test]
async fn a_block_reporting_another_name_is_refused() {
    let mut w = wafer();
    let spoof = Reports(info(
        "a/victim",
        vec![ResourceGrant::read_write("x/attacker", "a__victim__*")],
    ));
    assert_name_mismatch(
        w.register_block("x/attacker", Arc::new(spoof)),
        "x/attacker",
        "a/victim",
    );
    assert!(!w.has_block("x/attacker"));
    w.seal()
        .await
        .expect("nothing was registered, so seal succeeds");
    assert!(
        check_access(
            Some("x/attacker"),
            "a__victim__users",
            ResourceAccess::Write,
            None,
            w.wrap_grants(),
            "",
        )
        .is_err(),
        "no grant reaches a__victim__ tables"
    );
}

/// The admin half: typed Network grants are admin-only, decided by comparing
/// the declaring block with the admin block id. A block claiming the admin's
/// name is refused before that comparison can be fooled.
#[test]
fn a_block_reporting_the_admin_name_is_refused() {
    let mut w = wafer();
    w.set_admin_block("acme/admin");
    let spoof = Reports(info(
        "acme/admin",
        vec![ResourceGrant::read_write("*", "*").typed(ResourceType::Network)],
    ));
    assert_name_mismatch(
        w.register_block("x/attacker", Arc::new(spoof)),
        "x/attacker",
        "acme/admin",
    );
}

/// The same refusal for the block a guest actually controls: a WASM module
/// whose `__wafer_info` names a block it is not registered as.
#[cfg(feature = "wasm")]
#[test]
fn a_wasm_guest_reporting_another_name_is_refused() {
    let info = r#"{"name":"a/victim","version":"0.1.0","interface":"handler@v1","summary":""}"#;
    let packed = (64u64 << 32) | info.len() as u64;
    let escaped = info.replace('"', "\\\"");
    let bytes = wat::parse_str(format!(
        r#"(module
            (memory (export "memory") 1)
            (data (i32.const 64) "{escaped}")
            (func (export "__wafer_info") (result i64) (i64.const {packed})))"#
    ))
    .expect("WAT parses");
    let guest = wafer_run::wasm::WasmiBlock::load_from_bytes(&bytes).expect("guest loads");
    let mut w = wafer();
    assert_name_mismatch(
        w.register_block("x/attacker", Arc::new(guest)),
        "x/attacker",
        "a/victim",
    );
}

/// Reports its registration name until `flip` is set, then `a/victim` — the
/// shape a block whose `info()` is not stable would take. Grant validation
/// re-reads `info()` when the admin block is set; it must still judge the
/// grants against the registration name, not whatever name `info()` now
/// reports.
struct Flips {
    flip: Arc<AtomicBool>,
}

#[async_trait]
impl Block for Flips {
    fn info(&self) -> BlockInfo {
        let name = if self.flip.load(Ordering::SeqCst) {
            "a/victim"
        } else {
            "x/attacker"
        };
        info(
            name,
            vec![ResourceGrant::read_write("x/attacker", "a__victim__*")],
        )
    }

    async fn handle(
        &self,
        _ctx: &dyn wafer_block::context::Context,
        _msg: Message,
        _input: InputStream,
    ) -> OutputStream {
        OutputStream::respond(Vec::new())
    }
}

#[tokio::test]
async fn grant_rescan_judges_grants_by_registration_name() {
    let flip = Arc::new(AtomicBool::new(false));
    let mut w = wafer();
    w.register_block("x/attacker", Arc::new(Flips { flip: flip.clone() }))
        .expect("the reported name matches at registration");
    flip.store(true, Ordering::SeqCst);
    // Re-collects every registered block's grants from a fresh `info()`.
    w.set_admin_block("acme/admin");

    match w.seal().await {
        Err(RuntimeError::GrantsRejected(errors)) => {
            assert!(
                errors
                    .iter()
                    .any(|e| e.block == "x/attacker" && e.grant.resource == "a__victim__*"),
                "the foreign-namespace grant is rejected for x/attacker: {errors:?}"
            );
        }
        other => panic!("expected GrantsRejected, got {:?}", other.map(|_| "Ok(_)")),
    }
}
