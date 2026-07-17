//! Unified service block implementations.
//!
//! Each block wraps an `Arc<dyn XService>` and delegates to the shared handler
//! in `wafer_core::interfaces`. Platform-specific code only provides the service
//! implementation; the block struct, `info()`, and message routing are shared.
//!
//! The per-interface modules below are thin [`service_block!`](crate::service_block)
//! invocations; the macro generates the block struct, the `Block` impl
//! (`info()` / `handle()` / optional `lifecycle()`), and `register_with`, so
//! each module only spells out what is unique to it: name, interface,
//! handler call shape, and optional info extras / lifecycle body.

/// Native auth service block wrapping an `Arc<dyn AuthService>`.
pub mod auth;
/// Native config service block wrapping an `Arc<dyn ConfigService>`, plus the
/// env-var-backed `EnvConfigService` implementation.
pub mod config;
/// Native crypto service block wrapping an `Arc<dyn CryptoService>`.
pub mod crypto;
/// Native database service block wrapping an `Arc<dyn DatabaseService>`.
pub mod database;
/// Native image service block wrapping an `Arc<dyn ImageService>`.
pub mod image;
/// Native LLM service block wrapping an `Arc<dyn LlmService>`.
pub mod llm;
/// Native logger service block wrapping an `Arc<dyn LoggerService>`, plus the
/// `tracing`-backed `TracingLogger` implementation.
pub mod logger;
/// Native network service block wrapping an `Arc<dyn NetworkService>`.
pub mod network;
/// Native storage service block wrapping an `Arc<dyn StorageService>`.
pub mod storage;
/// Native vector service block wrapping a vector + embedding service pair.
pub mod vector;

/// Support module re-exporting every item the [`service_block!`](crate::service_block)
/// expansion references through `$crate::…` paths. Hidden because it is an
/// expansion detail, not public API — consumers import these types from
/// `wafer_block` directly.
#[doc(hidden)]
pub mod __private {
    pub use std::sync::{Arc, OnceLock};

    pub use wafer_block::{
        Block, BlockCategory, BlockInfo, BlockRegistry, Context, ErrorCode, InputStream,
        LifecycleEvent, Message, OutputStream, RuntimeError, WaferError,
    };
    pub use wafer_block_macro::wafer_async_trait;
}

/// Generate a service block: the struct, its constructor, the
/// [`Block`](wafer_block::Block) impl (`info()` / `handle()` / optional
/// `lifecycle()`), and — for the wrapper form — a `register_with` function.
///
/// The `handle:` / `lifecycle:` / `info_extras:` parameter lists look like
/// closures but are *bindings*, not closures: each expression is inlined into
/// the generated method body, so `.await` and `?` work directly inside it.
/// Bindings the expression doesn't use must be spelled with a leading
/// underscore (`_ctx`), exactly like unused function parameters.
///
/// # Wrapper form
///
/// The block wraps service implementation(s) injected at construction time:
///
/// ```ignore
/// crate::service_block! {
///     /// Unified config block. Wraps any `ConfigService` implementation.
///     block: pub ConfigBlock,
///     name: "wafer-run/config",
///     version: "0.0.1",
///     interface: "config@v1",
///     description: "Configuration key-value access",
///     category: Service,
///     fields: { service: Arc<dyn ConfigService> },
///     handle: |this, _ctx, msg, body| handler::handle_message(this.service.as_ref(), &msg, &body),
/// }
/// ```
///
/// - `fields` become private struct fields and, in declaration order, the
///   parameters of the generated `new()` and `register_with()`.
/// - `extra_fields` (optional) are additional private fields initialized with
///   `Default::default()` in `new()` (e.g. `DatabaseBlock`'s `tables`); custom
///   constructors over them are written as plain code next to the invocation.
/// - `info_extras` (optional) receives `self` and the base
///   [`BlockInfo`](wafer_block::BlockInfo) (already carrying
///   name/version/interface/description/category) and returns the final
///   `BlockInfo` — use it for `.config_keys(…)`, `.grants(…)`, etc.
/// - `handle` receives `self`, the `&dyn Context`, the `Message`, and the
///   collected body bytes; its expression is the `handle()` body.
/// - `lifecycle` (optional) receives `self`, the `&dyn Context`, and the
///   `LifecycleEvent`; when omitted the `Block` trait's no-op default is used.
///
/// # Lazy form
///
/// For infrastructure blocks that construct their service during
/// `lifecycle(Init)` (from env vars / block config): the service lives in a
/// `OnceLock` field named `service`, `new()` takes no arguments (a `Default`
/// impl is also generated, as required by
/// [`register_static_block!`](wafer_block::register_static_block)), and
/// `handle` receives the resolved `&Arc<dyn …Service>` as its first binding.
/// A message dispatched before `lifecycle(Init)` uniformly yields a typed
/// [`ErrorCode::Internal`](wafer_block::ErrorCode::Internal) error terminal —
/// never a panic. No `register_with` is generated; lazy blocks register via
/// `register_static_block!`.
///
/// ```ignore
/// wafer_core::service_block! {
///     /// The S3-compatible storage block.
///     lazy block: pub(crate) S3StorageBlock,
///     name: "wafer-run/s3",
///     version: "0.0.1",
///     interface: "storage@v1",
///     description: "S3-compatible storage block",
///     category: Infrastructure,
///     service: dyn StorageService,
///     handle: |service, _this, _ctx, msg, body| {
///         handler::handle_message(service.as_ref(), &msg, &body).await
///     },
///     lifecycle: |this, _ctx, event| { /* build + this.service.set(…) */ },
/// }
/// ```
#[macro_export]
macro_rules! service_block {
    // ----- wrapper form: service(s) injected at construction time ----------
    (
        $(#[$attr:meta])*
        block: $vis:vis $block:ident,
        name: $name:literal,
        version: $version:literal,
        interface: $interface:literal,
        description: $description:literal,
        category: $category:ident,
        fields: { $($field:ident : $fty:ty),+ $(,)? },
        $(extra_fields: { $($efield:ident : $efty:ty),+ $(,)? },)?
        $(info_extras: |$ithis:ident, $info:ident| $iexpr:expr,)?
        handle: |$hthis:ident, $hctx:ident, $hmsg:ident, $hbody:ident| $hexpr:expr,
        $(stream_ingress: {
            op: $sop:expr,
            handle: |$sthis:ident, $sctx:ident, $smsg:ident, $sinput:ident| $sexpr:expr $(,)?
        },)?
        $(lifecycle: |$lthis:ident, $lctx:ident, $levent:ident| $lexpr:expr,)?
    ) => {
        $(#[$attr])*
        $vis struct $block {
            $($field: $fty,)+
            $($($efield: $efty,)+)?
        }

        impl $block {
            #[doc = concat!(
                "Construct a [`", stringify!($block),
                "`] wrapping the given service implementation(s)."
            )]
            pub fn new($($field: $fty),+) -> Self {
                Self {
                    $($field,)+
                    $($($efield: ::core::default::Default::default(),)+)?
                }
            }
        }

        #[$crate::service_blocks::__private::wafer_async_trait]
        impl $crate::service_blocks::__private::Block for $block {
            fn info(&self) -> $crate::service_blocks::__private::BlockInfo {
                $crate::service_block!(
                    @info self, $name, $version, $interface, $description, $category
                    $(, |$ithis, $info| $iexpr)?
                )
            }

            async fn handle(
                &self,
                ctx: &dyn $crate::service_blocks::__private::Context,
                msg: $crate::service_blocks::__private::Message,
                input: $crate::service_blocks::__private::InputStream,
            ) -> $crate::service_blocks::__private::OutputStream {
                $(
                    // Streaming-ingress op: hand the raw `InputStream` to the
                    // streaming handler WITHOUT `collect_to_bytes`, so a large
                    // request body (e.g. a file upload) streams into the
                    // backend instead of being buffered whole in isolate
                    // memory. Additive — every other op falls through to the
                    // buffered path below byte-for-byte unchanged. The bool
                    // binding drops the transient `&str` borrow of `msg`
                    // before the diverging branch moves `msg`/`input`.
                    let __wafer_stream_ingress = msg.kind.as_str() == $sop;
                    if __wafer_stream_ingress {
                        let $sthis = self;
                        let $sctx = ctx;
                        let $smsg = msg;
                        let $sinput = input;
                        return $sexpr;
                    }
                )?
                let $hbody = input.collect_to_bytes().await;
                let $hthis = self;
                let $hctx = ctx;
                let $hmsg = msg;
                $hexpr
            }

            $(
                async fn lifecycle(
                    &self,
                    ctx: &dyn $crate::service_blocks::__private::Context,
                    event: $crate::service_blocks::__private::LifecycleEvent,
                ) -> ::std::result::Result<(), $crate::service_blocks::__private::WaferError> {
                    let $lthis = self;
                    let $lctx = ctx;
                    let $levent = event;
                    $lexpr
                }
            )?
        }

        #[doc = concat!(
            "Register the `", $name,
            "` block, wrapping the given service implementation(s)."
        )]
        pub fn register_with(
            w: &mut dyn $crate::service_blocks::__private::BlockRegistry,
            $($field: $fty),+
        ) -> ::std::result::Result<(), $crate::service_blocks::__private::RuntimeError> {
            w.register_block(
                $name,
                $crate::service_blocks::__private::Arc::new($block::new($($field),+)),
            )
        }
    };

    // ----- lazy form: service built during lifecycle(Init) -----------------
    (
        $(#[$attr:meta])*
        lazy block: $vis:vis $block:ident,
        name: $name:literal,
        version: $version:literal,
        interface: $interface:literal,
        description: $description:literal,
        category: $category:ident,
        service: $sty:ty,
        $(extra_fields: { $($efield:ident : $efty:ty),+ $(,)? },)?
        $(info_extras: |$ithis:ident, $info:ident| $iexpr:expr,)?
        handle: |$hsvc:ident, $hthis:ident, $hctx:ident, $hmsg:ident, $hbody:ident| $hexpr:expr,
        $(stream_ingress: {
            op: $sop:expr,
            handle: |$ssvc:ident, $sthis:ident, $sctx:ident, $smsg:ident, $sinput:ident| $sexpr:expr $(,)?
        },)?
        lifecycle: |$lthis:ident, $lctx:ident, $levent:ident| $lexpr:expr,
    ) => {
        $(#[$attr])*
        $vis struct $block {
            service: $crate::service_blocks::__private::OnceLock<
                $crate::service_blocks::__private::Arc<$sty>,
            >,
            $($($efield: $efty,)+)?
        }

        impl $block {
            #[doc = concat!(
                "Construct an uninitialized [`", stringify!($block),
                "`]; the service is built during `lifecycle(Init)`."
            )]
            pub fn new() -> Self {
                Self {
                    service: $crate::service_blocks::__private::OnceLock::new(),
                    $($($efield: ::core::default::Default::default(),)+)?
                }
            }
        }

        impl ::core::default::Default for $block {
            fn default() -> Self {
                Self::new()
            }
        }

        #[$crate::service_blocks::__private::wafer_async_trait]
        impl $crate::service_blocks::__private::Block for $block {
            fn info(&self) -> $crate::service_blocks::__private::BlockInfo {
                $crate::service_block!(
                    @info self, $name, $version, $interface, $description, $category
                    $(, |$ithis, $info| $iexpr)?
                )
            }

            async fn handle(
                &self,
                ctx: &dyn $crate::service_blocks::__private::Context,
                msg: $crate::service_blocks::__private::Message,
                input: $crate::service_blocks::__private::InputStream,
            ) -> $crate::service_blocks::__private::OutputStream {
                // Reached if the runtime dispatches a message before
                // lifecycle(Init) completes — programmer error in the host,
                // but surface a typed error rather than panicking so the
                // requester gets a clean 500.
                let ::core::option::Option::Some(__wafer_svc) = self.service.get() else {
                    return $crate::service_blocks::__private::OutputStream::error(
                        $crate::service_blocks::__private::WaferError::new(
                            $crate::service_blocks::__private::ErrorCode::Internal,
                            concat!($name, ": not initialized — call lifecycle(Init) first"),
                        ),
                    );
                };
                $(
                    // Streaming-ingress op: hand the raw `InputStream` to the
                    // streaming handler WITHOUT `collect_to_bytes` (see the
                    // wrapper form for the full rationale). The resolved
                    // service is a shared reference (`Copy`), so both branches
                    // may bind it.
                    let __wafer_stream_ingress = msg.kind.as_str() == $sop;
                    if __wafer_stream_ingress {
                        let $ssvc = __wafer_svc;
                        let $sthis = self;
                        let $sctx = ctx;
                        let $smsg = msg;
                        let $sinput = input;
                        return $sexpr;
                    }
                )?
                let $hbody = input.collect_to_bytes().await;
                let $hsvc = __wafer_svc;
                let $hthis = self;
                let $hctx = ctx;
                let $hmsg = msg;
                $hexpr
            }

            async fn lifecycle(
                &self,
                ctx: &dyn $crate::service_blocks::__private::Context,
                event: $crate::service_blocks::__private::LifecycleEvent,
            ) -> ::std::result::Result<(), $crate::service_blocks::__private::WaferError> {
                let $lthis = self;
                let $lctx = ctx;
                let $levent = event;
                $lexpr
            }
        }
    };

    // ----- internal: build the BlockInfo, applying optional info extras ----
    (@info $self:ident, $name:literal, $version:literal, $interface:literal,
     $description:literal, $category:ident) => {
        $crate::service_blocks::__private::BlockInfo::new(
            $name,
            $version,
            $interface,
            $description,
        )
        .category($crate::service_blocks::__private::BlockCategory::$category)
    };
    (@info $self:ident, $name:literal, $version:literal, $interface:literal,
     $description:literal, $category:ident, |$ithis:ident, $info:ident| $iexpr:expr) => {{
        let $ithis = $self;
        let $info = $crate::service_blocks::__private::BlockInfo::new(
            $name,
            $version,
            $interface,
            $description,
        )
        .category($crate::service_blocks::__private::BlockCategory::$category);
        $iexpr
    }};
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use wafer_block::{
        error::AliasError, Block, BlockCategory, BlockRegistry, InputStream, Message, RuntimeError,
        TerminalNotResponse,
    };

    use super::config::{ConfigBlock, EnvConfigService};
    use crate::interfaces::config::service::ConfigService;

    /// `BlockRegistry` stub that records registered block names.
    #[derive(Default)]
    struct RecordingRegistry {
        names: Vec<String>,
    }

    impl BlockRegistry for RecordingRegistry {
        fn register_block(
            &mut self,
            name: &str,
            _block: Arc<dyn Block>,
        ) -> Result<(), RuntimeError> {
            self.names.push(name.to_string());
            Ok(())
        }

        fn add_alias(&mut self, _alias: &str, _target: &str) -> Result<(), AliasError> {
            Ok(())
        }

        fn add_block_config(&mut self, _name: &str, _config: serde_json::Value) {}
    }

    #[test]
    fn generated_info_carries_name_version_interface_category() {
        let block = ConfigBlock::new(Arc::new(EnvConfigService::new()));
        let info = block.info();
        assert_eq!(info.name, "wafer-run/config");
        assert_eq!(info.version, "0.0.1");
        assert_eq!(info.interface, "config@v1");
        assert_eq!(info.category, BlockCategory::Service);
    }

    #[test]
    fn generated_register_with_uses_declared_block_name() {
        let mut registry = RecordingRegistry::default();
        super::config::register_with(&mut registry, Arc::new(EnvConfigService::new()))
            .expect("register_with");
        assert_eq!(registry.names, vec!["wafer-run/config".to_string()]);
    }

    #[tokio::test]
    async fn generated_handle_routes_to_shared_handler() {
        // An unknown op reaching the shared handler proves the macro-generated
        // `handle()` collected the body and dispatched into `handler::handle_message`.
        let block = ConfigBlock::new(Arc::new(EnvConfigService::new()));
        let ctx = crate::test_support::noop_context();
        let out = block
            .handle(
                &*ctx,
                Message::new("config.unknown-op"),
                InputStream::empty(),
            )
            .await;
        match out.collect_buffered().await {
            Err(TerminalNotResponse::Error(e)) => {
                assert!(
                    e.message.contains("config.unknown-op"),
                    "handler should name the unknown op, got: {}",
                    e.message
                );
            }
            other => panic!("expected handler error terminal, got: {other:?}"),
        }
    }

    #[test]
    fn env_config_service_overrides_shadow_process_env() {
        let svc = EnvConfigService::new();
        assert_eq!(svc.get("WAFER_TEST_UNSET_KEY"), None);
        svc.set("WAFER_TEST_UNSET_KEY", "override");
        assert_eq!(svc.get("WAFER_TEST_UNSET_KEY"), Some("override".into()));
    }
}
