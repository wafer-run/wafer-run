//! Wafer::run_block triggers lazy init on the target block before dispatching.
//!
//! Spec: docs/superpowers/specs/2026-05-15-lazy-block-init-design.md §3

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use async_trait::async_trait;
use wafer_block::{
    context::Context,
    core_types::{LifecycleEvent, LifecycleType, Message, WaferError},
    streams::{input::InputStream, output::OutputStream},
    Block, BlockInfo, ConfigVar,
};
use wafer_run::{ConfigSource, StaticConfigSource, Wafer};

struct CounterBlock {
    name: &'static str,
    init_calls: Arc<AtomicUsize>,
    handle_calls: Arc<AtomicUsize>,
    declared: Vec<ConfigVar>,
}

#[async_trait]
impl Block for CounterBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(self.name, "0.1.0", "test/iface@v1", "test")
            .config_keys(self.declared.clone())
    }

    async fn lifecycle(
        &self,
        _ctx: &dyn wafer_block::context::Context,
        event: LifecycleEvent,
    ) -> Result<(), WaferError> {
        if event.event_type == LifecycleType::Init {
            self.init_calls.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    }

    async fn handle(
        &self,
        _ctx: &dyn wafer_block::context::Context,
        _msg: Message,
        _input: InputStream,
    ) -> OutputStream {
        self.handle_calls.fetch_add(1, Ordering::SeqCst);
        OutputStream::respond(Vec::new())
    }
}

fn config_var_prefix(block: &str) -> String {
    let mut out = String::new();
    for ch in block.chars() {
        match ch {
            '/' => out.push_str("__"),
            '-' => out.push('_'),
            c => out.extend(c.to_uppercase()),
        }
    }
    out.push_str("__");
    out
}

#[tokio::test]
async fn run_block_initializes_target_on_first_call() {
    let init_calls = Arc::new(AtomicUsize::new(0));
    let handle_calls = Arc::new(AtomicUsize::new(0));

    let block = Arc::new(CounterBlock {
        name: "test/foo",
        init_calls: init_calls.clone(),
        handle_calls: handle_calls.clone(),
        declared: vec![],
    });

    let cfg_src: Arc<dyn ConfigSource> = Arc::new(StaticConfigSource::default());
    let mut wafer = Wafer::new(cfg_src).expect("Wafer::new");
    wafer.register_block("test/foo", block).expect("register");
    // run_block resolves through all_blocks; populate it without going
    // through seal() so this test exercises lazy init in isolation.
    wafer.rebuild_all_blocks();
    let wafer = Arc::new(wafer);

    // First dispatch: triggers init then handle.
    let _out1 = wafer
        .run_block("test/foo", Message::new(""), InputStream::empty())
        .await;
    assert_eq!(init_calls.load(Ordering::SeqCst), 1);
    assert_eq!(handle_calls.load(Ordering::SeqCst), 1);

    // Second dispatch: cached init; handle still runs.
    let _out2 = wafer
        .run_block("test/foo", Message::new(""), InputStream::empty())
        .await;
    assert_eq!(init_calls.load(Ordering::SeqCst), 1, "init must not re-run");
    assert_eq!(handle_calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn run_block_skips_handle_if_init_fails() {
    use wafer_block::streams::output::TerminalNotResponse;

    let init_calls = Arc::new(AtomicUsize::new(0));
    let handle_calls = Arc::new(AtomicUsize::new(0));

    let key = format!("{}REQ", config_var_prefix("test/bad"));
    let block = Arc::new(CounterBlock {
        name: "test/bad",
        init_calls: init_calls.clone(),
        handle_calls: handle_calls.clone(),
        // ConfigVar::new defaults to optional=false (required).
        declared: vec![ConfigVar::new(&key, "doc", "")],
    });

    let cfg_src: Arc<dyn ConfigSource> = Arc::new(StaticConfigSource::default());
    let mut wafer = Wafer::new(cfg_src).expect("Wafer::new");
    wafer.register_block("test/bad", block).expect("register");
    wafer.rebuild_all_blocks();
    let wafer = Arc::new(wafer);

    let out = wafer
        .run_block("test/bad", Message::new(""), InputStream::empty())
        .await;
    // Config-load short-circuits because TEST_BAD__REQ is required+missing.
    // lifecycle(Init) never runs; handle must be skipped.
    assert_eq!(
        init_calls.load(Ordering::SeqCst),
        0,
        "lifecycle(Init) must not run when config load fails"
    );
    assert_eq!(
        handle_calls.load(Ordering::SeqCst),
        0,
        "handle must not run when init fails"
    );
    match out.collect_buffered().await {
        Err(TerminalNotResponse::Error(e)) => {
            assert!(
                e.message.contains("init"),
                "error message should reference init: {}",
                e.message
            );
        }
        other => panic!("expected Error terminal, got {other:?}"),
    }
}

/// A block reached only via `call_block` from another block's `handle`
/// must still run lifecycle(Init) lazily on first invocation. Before this
/// fix, `RuntimeContext::dispatch_call` invoked `block.handle(...)` without
/// consulting the init slot, so transitive callees skipped init.
#[tokio::test]
async fn call_block_initializes_callee_lazily() {
    struct CallerBlock;

    #[async_trait]
    impl Block for CallerBlock {
        fn info(&self) -> BlockInfo {
            BlockInfo::new("test/caller", "0.1.0", "test/iface@v1", "test")
        }

        async fn lifecycle(
            &self,
            _ctx: &dyn Context,
            _event: LifecycleEvent,
        ) -> Result<(), WaferError> {
            Ok(())
        }

        async fn handle(
            &self,
            ctx: &dyn Context,
            _msg: Message,
            _input: InputStream,
        ) -> OutputStream {
            // Invoke the callee via call_block; the runtime must ensure
            // lifecycle(Init) has run on the callee before its `handle` runs.
            ctx.call_block("test/callee", Message::new(""), InputStream::empty())
                .await
        }
    }

    let callee_init_calls = Arc::new(AtomicUsize::new(0));
    let callee_handle_calls = Arc::new(AtomicUsize::new(0));

    let caller = Arc::new(CallerBlock);
    let callee = Arc::new(CounterBlock {
        name: "test/callee",
        init_calls: callee_init_calls.clone(),
        handle_calls: callee_handle_calls.clone(),
        declared: vec![],
    });

    let cfg_src: Arc<dyn ConfigSource> = Arc::new(StaticConfigSource::default());
    let mut wafer = Wafer::new(cfg_src).expect("Wafer::new");
    wafer
        .register_block("test/caller", caller)
        .expect("register caller");
    wafer
        .register_block("test/callee", callee)
        .expect("register callee");
    wafer.rebuild_all_blocks();
    let wafer = Arc::new(wafer);

    let _out = wafer
        .run_block("test/caller", Message::new(""), InputStream::empty())
        .await;

    // Callee reached only via call_block — its init must have run.
    assert_eq!(
        callee_init_calls.load(Ordering::SeqCst),
        1,
        "callee init must run lazily on call_block"
    );
    assert_eq!(
        callee_handle_calls.load(Ordering::SeqCst),
        1,
        "callee handle must run after init"
    );

    // Second top-level dispatch: callee init must NOT re-run (slot caches Ok).
    let _out2 = wafer
        .run_block("test/caller", Message::new(""), InputStream::empty())
        .await;
    assert_eq!(
        callee_init_calls.load(Ordering::SeqCst),
        1,
        "callee init must be cached across call_block invocations"
    );
    assert_eq!(
        callee_handle_calls.load(Ordering::SeqCst),
        2,
        "callee handle re-runs on each call_block"
    );
}

/// When a callee is reached through an alias, its sub-context `node_id`
/// (the identity downstream WRAP checks attribute resource access to) must
/// be the *resolved* canonical block name, not the raw alias the caller
/// wrote. Before the fix, `dispatch_call` set `node_id` to the alias, so an
/// aliased callee making its own `call_block` was attributed to the alias —
/// observable here as the grandchild's `caller_id`.
#[tokio::test]
async fn aliased_callee_node_id_is_resolved_canonical_name() {
    use std::sync::Mutex;

    /// Calls `@callee-alias` and returns its output.
    struct ViaAliasCaller;

    #[async_trait]
    impl Block for ViaAliasCaller {
        fn info(&self) -> BlockInfo {
            BlockInfo::new("test/caller", "0.1.0", "test/iface@v1", "test")
        }
        async fn lifecycle(
            &self,
            _ctx: &dyn Context,
            _event: LifecycleEvent,
        ) -> Result<(), WaferError> {
            Ok(())
        }
        async fn handle(
            &self,
            ctx: &dyn Context,
            _msg: Message,
            _input: InputStream,
        ) -> OutputStream {
            ctx.call_block("@callee-alias", Message::new(""), InputStream::empty())
                .await
        }
    }

    /// Reached via the alias; calls a grandchild so we can observe the
    /// `caller_id` it propagates (which is this block's own `node_id`).
    struct MiddleCallee;

    #[async_trait]
    impl Block for MiddleCallee {
        fn info(&self) -> BlockInfo {
            BlockInfo::new("test/callee", "0.1.0", "test/iface@v1", "test")
        }
        async fn lifecycle(
            &self,
            _ctx: &dyn Context,
            _event: LifecycleEvent,
        ) -> Result<(), WaferError> {
            Ok(())
        }
        async fn handle(
            &self,
            ctx: &dyn Context,
            _msg: Message,
            _input: InputStream,
        ) -> OutputStream {
            ctx.call_block("test/grandchild", Message::new(""), InputStream::empty())
                .await
        }
    }

    /// Records the `caller_id` it observes — equals the middle callee's
    /// `node_id`.
    struct RecordingGrandchild {
        seen_caller: Arc<Mutex<Option<String>>>,
    }

    #[async_trait]
    impl Block for RecordingGrandchild {
        fn info(&self) -> BlockInfo {
            BlockInfo::new("test/grandchild", "0.1.0", "test/iface@v1", "test")
        }
        async fn lifecycle(
            &self,
            _ctx: &dyn Context,
            _event: LifecycleEvent,
        ) -> Result<(), WaferError> {
            Ok(())
        }
        async fn handle(
            &self,
            ctx: &dyn Context,
            _msg: Message,
            _input: InputStream,
        ) -> OutputStream {
            *self.seen_caller.lock().unwrap() = ctx.caller_id().map(str::to_string);
            OutputStream::respond(Vec::new())
        }
    }

    let seen_caller = Arc::new(Mutex::new(None));

    let cfg_src: Arc<dyn ConfigSource> = Arc::new(StaticConfigSource::default());
    let mut wafer = Wafer::new(cfg_src).expect("Wafer::new");
    wafer
        .register_block("test/caller", Arc::new(ViaAliasCaller))
        .expect("register caller");
    wafer
        .register_block("test/callee", Arc::new(MiddleCallee))
        .expect("register callee");
    wafer
        .register_block(
            "test/grandchild",
            Arc::new(RecordingGrandchild {
                seen_caller: seen_caller.clone(),
            }),
        )
        .expect("register grandchild");
    wafer
        .add_alias("@callee-alias", "test/callee")
        .expect("alias");
    wafer.rebuild_all_blocks();
    let wafer = Arc::new(wafer);

    let _out = wafer
        .run_block("test/caller", Message::new(""), InputStream::empty())
        .await;

    let observed = seen_caller.lock().unwrap().clone();
    assert_eq!(
        observed.as_deref(),
        Some("test/callee"),
        "callee reached via alias must attribute downstream calls to its \
         resolved canonical name, not the alias"
    );
}

/// Transitive init cycle (A.init → B.init → A) must surface as
/// `InitError::Cycle` (mapped to `FAILED_PRECONDITION`) on the top-level
/// output stream — without ever running `handle` on either block.
///
/// The shared `init_breadcrumbs` Arc inside `RuntimeContext` propagates
/// the init stack across `call_block` boundaries; this test verifies that
/// propagation end-to-end. Without it, B's init would not see A on the
/// stack and we'd recurse forever (or deadlock on A's slot lock).
#[tokio::test]
async fn transitive_init_cycle_surfaces_failed_precondition() {
    use wafer_block::{core_types::ErrorCode, streams::output::TerminalNotResponse};

    /// Calls into another block during `lifecycle(Init)`. Used to compose
    /// A→B→A and observe the cycle.
    struct CallDuringInitBlock {
        name: &'static str,
        callee: &'static str,
        init_calls: Arc<AtomicUsize>,
        handle_calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Block for CallDuringInitBlock {
        fn info(&self) -> BlockInfo {
            BlockInfo::new(self.name, "0.1.0", "test/iface@v1", "test")
        }

        async fn lifecycle(
            &self,
            ctx: &dyn Context,
            event: LifecycleEvent,
        ) -> Result<(), WaferError> {
            if event.event_type == LifecycleType::Init {
                self.init_calls.fetch_add(1, Ordering::SeqCst);
                // Trigger init on `callee` from inside our own init —
                // this is the move that closes the cycle when callee.init
                // ultimately tries to init us again.
                let out = ctx
                    .call_block(self.callee, Message::new(""), InputStream::empty())
                    .await;
                // The recursive call returns an error stream when the
                // cycle is detected; surface it as a lifecycle error so
                // the runtime's lazy-init pipeline records it.
                if let Err(TerminalNotResponse::Error(e)) = out.collect_buffered().await {
                    return Err(e);
                }
            }
            Ok(())
        }

        async fn handle(
            &self,
            _ctx: &dyn Context,
            _msg: Message,
            _input: InputStream,
        ) -> OutputStream {
            self.handle_calls.fetch_add(1, Ordering::SeqCst);
            OutputStream::respond(Vec::new())
        }
    }

    let a_init = Arc::new(AtomicUsize::new(0));
    let a_handle = Arc::new(AtomicUsize::new(0));
    let b_init = Arc::new(AtomicUsize::new(0));
    let b_handle = Arc::new(AtomicUsize::new(0));

    let a = Arc::new(CallDuringInitBlock {
        name: "test/a",
        callee: "test/b",
        init_calls: a_init.clone(),
        handle_calls: a_handle.clone(),
    });
    let b = Arc::new(CallDuringInitBlock {
        name: "test/b",
        callee: "test/a",
        init_calls: b_init.clone(),
        handle_calls: b_handle.clone(),
    });

    let cfg_src: Arc<dyn ConfigSource> = Arc::new(StaticConfigSource::default());
    let mut wafer = Wafer::new(cfg_src).expect("Wafer::new");
    wafer.register_block("test/a", a).expect("register a");
    wafer.register_block("test/b", b).expect("register b");
    wafer.rebuild_all_blocks();
    let wafer = Arc::new(wafer);

    let out = wafer
        .run_block("test/a", Message::new(""), InputStream::empty())
        .await;

    match out.collect_buffered().await {
        Err(TerminalNotResponse::Error(e)) => {
            assert_eq!(
                e.code,
                ErrorCode::FailedPrecondition,
                "cycle must surface as FAILED_PRECONDITION, got {:?}",
                e.code
            );
            assert!(
                e.message.contains("test/a"),
                "cycle error must name test/a; got: {}",
                e.message
            );
            assert!(
                e.message.contains("test/b"),
                "cycle error must name test/b; got: {}",
                e.message
            );
        }
        other => panic!("expected Error terminal for transitive init cycle, got {other:?}"),
    }

    // Neither `handle` should have run — init failed for both blocks.
    assert_eq!(
        a_handle.load(Ordering::SeqCst),
        0,
        "A.handle must not run when its init cycle-fails"
    );
    assert_eq!(
        b_handle.load(Ordering::SeqCst),
        0,
        "B.handle must not run when its init participates in a cycle"
    );
}
