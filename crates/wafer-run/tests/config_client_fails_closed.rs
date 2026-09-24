//! The typed config client falls back to a default only when the key is not
//! set, driven end to end: a caller block reads through
//! `wafer_core::clients::config` and `ctx.call_block` into the REAL
//! `wafer-run/config` block, whose handler authorizes the caller against the
//! REAL `RuntimeContext` before it looks the key up.
//!
//! A read the runtime refuses must reach the caller as that refusal. A
//! default standing in for it would hand a block the fallback value of a
//! setting it was never allowed to read, and hide the refusal from the
//! operator.
//!
//! The same holds when there is no config block to ask: the runtime's
//! `Unimplemented` for an unregistered block is not the config block's
//! `NotFound` for an unset key.

use std::sync::Arc;

use async_trait::async_trait;
use wafer_block::{
    core_types::{LifecycleEvent, Message, WaferError},
    streams::{
        input::InputStream,
        output::{OutputStream, TerminalNotResponse},
    },
    Block, BlockInfo, ErrorCode,
};
use wafer_core::{
    interfaces::config::service::ConfigService, service_blocks::config::EnvConfigService,
};
use wafer_run::{Context, Wafer};

/// The calling block. It owns the `ACME__READER__*` config keys.
const READER: &str = "acme/reader";
/// Set, and in the reader's own namespace.
const OWN_SET: &str = "ACME__READER__GREETING";
/// Not set, in the reader's own namespace.
const OWN_UNSET: &str = "ACME__READER__MISSING";
/// Set, in another block's namespace; the reader holds no grant for it.
const FOREIGN: &str = "ACME__VAULT__CLIENT_SECRET";
const DEFAULT: &str = "fallback";

/// Reads the config key named by the request's `key` meta. `default` answers
/// with [`get_default`](wafer_core::clients::config::get_default)'s value;
/// `optional` answers `some:<value>` or `none` from
/// [`get_optional`](wafer_core::clients::config::get_optional). A failed
/// read answers with the error the client returned.
struct Reader;

#[async_trait]
impl Block for Reader {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(READER, "0.1.0", "test/iface@v1", "reads config")
    }
    async fn lifecycle(&self, _ctx: &dyn Context, _e: LifecycleEvent) -> Result<(), WaferError> {
        Ok(())
    }
    async fn handle(&self, ctx: &dyn Context, msg: Message, _input: InputStream) -> OutputStream {
        let key = msg.get_meta("key");
        let answer = match msg.kind.as_str() {
            "default" => wafer_core::clients::config::get_default(ctx, key, DEFAULT).await,
            "optional" => wafer_core::clients::config::get_optional(ctx, key)
                .await
                .map(|v| v.map_or_else(|| "none".to_string(), |v| format!("some:{v}"))),
            other => Err(WaferError::new(ErrorCode::Unimplemented, other)),
        };
        match answer {
            Ok(value) => OutputStream::respond(value.into_bytes()),
            Err(e) => OutputStream::error(e),
        }
    }
}

async fn build() -> Wafer {
    let mut wafer = Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .build()
        .expect("Wafer::build");
    let config = EnvConfigService::new();
    config.set(OWN_SET, "hello");
    config.set(FOREIGN, "s3cret");
    wafer_core::service_blocks::config::register_with(&mut wafer, Arc::new(config))
        .expect("register wafer-run/config");
    wafer
        .register_block(READER, Arc::new(Reader))
        .expect("register reader");
    wafer.seal().await.expect("seal");
    wafer
}

/// Run `kind` for `key` as the reader: the response body as text, or the
/// code it failed with.
async fn read(wafer: &Wafer, kind: &str, key: &str) -> Result<String, ErrorCode> {
    let mut msg = Message::new(kind);
    msg.set_meta("key", key);
    let out = wafer
        .run_block(READER, msg, InputStream::from_bytes(Vec::new()))
        .await;
    match out.collect_buffered().await {
        Ok(resp) => Ok(String::from_utf8(resp.body).expect("utf-8 body")),
        Err(TerminalNotResponse::Error(e)) => Err(e.code),
        Err(other) => panic!("no response or error: {other:?}"),
    }
}

/// The finding: a refused read came back as the default.
#[tokio::test]
async fn get_default_returns_a_refusal_instead_of_the_default() {
    let wafer = build().await;
    assert_eq!(
        read(&wafer, "default", FOREIGN).await,
        Err(ErrorCode::PermissionDenied)
    );
}

#[tokio::test]
async fn get_default_falls_back_only_when_the_key_is_not_set() {
    let wafer = build().await;
    assert_eq!(read(&wafer, "default", OWN_UNSET).await, Ok(DEFAULT.into()));
    assert_eq!(read(&wafer, "default", OWN_SET).await, Ok("hello".into()));
}

#[tokio::test]
async fn get_optional_returns_a_refusal_instead_of_none() {
    let wafer = build().await;
    assert_eq!(
        read(&wafer, "optional", FOREIGN).await,
        Err(ErrorCode::PermissionDenied)
    );
}

#[tokio::test]
async fn get_optional_is_none_only_when_the_key_is_not_set() {
    let wafer = build().await;
    assert_eq!(read(&wafer, "optional", OWN_UNSET).await, Ok("none".into()));
    assert_eq!(
        read(&wafer, "optional", OWN_SET).await,
        Ok("some:hello".into())
    );
}

/// The reader registered with no `wafer-run/config` block at all. The reader
/// declares no `requires`, so `seal()` has nothing to check it against: the
/// absence surfaces on the call.
async fn build_without_config() -> Wafer {
    let mut wafer = Wafer::builder()
        .disable_inventory()
        .disable_lockfile()
        .build()
        .expect("Wafer::build");
    wafer
        .register_block(READER, Arc::new(Reader))
        .expect("register reader");
    wafer.seal().await.expect("seal");
    wafer
}

/// With the config block absent the runtime has nothing to dispatch to. That
/// is not "the key is not set", so it must not become the default.
#[tokio::test]
async fn get_default_fails_when_the_config_block_is_absent() {
    let wafer = build_without_config().await;
    assert_eq!(
        read(&wafer, "default", OWN_UNSET).await,
        Err(ErrorCode::Unimplemented)
    );
}

#[tokio::test]
async fn get_optional_fails_when_the_config_block_is_absent() {
    let wafer = build_without_config().await;
    assert_eq!(
        read(&wafer, "optional", OWN_UNSET).await,
        Err(ErrorCode::Unimplemented)
    );
}
