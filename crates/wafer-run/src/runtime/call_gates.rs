//! The gates a `call_block` passes before its callee runs.
//!
//! [`admit_call`] is the one implementation of that admission sequence.
//! [`RuntimeContext`](crate::context::RuntimeContext) runs every
//! `call_block` / `call_block_with_attachments` through it, and a
//! [`Context`](crate::context::Context) an embedder implements itself — a
//! test harness standing in for the runtime — runs its calls through the
//! same function by implementing [`CallFrame`], instead of re-stating the
//! gates and drifting from them when one is added.
//!
//! What follows admission is not a gate and is not here: the runtime then
//! runs the callee's lazy `lifecycle(Init)` and the observability hooks
//! before handing the message to [`Block::handle`] on a sub-context built
//! from the [`AdmittedCall`].

use std::{
    borrow::Cow,
    collections::HashMap,
    sync::{atomic::AtomicBool, Arc},
};

use wafer_block::{
    core_types::{ErrorCode, Message, WaferError},
    types::InterfaceSpec,
    Block, BlockCapabilities,
};

use crate::{
    platform::Instant,
    runtime::validation::{check_action_interface, ActionCheck},
};

/// How many `call_block` frames may nest before a call is refused with
/// [`ErrorCode::ResourceExhausted`] — the depth every context the runtime
/// builds starts with ([`RuntimeContext::max_call_depth`](crate::context::RuntimeContext::max_call_depth)).
pub const DEFAULT_MAX_CALL_DEPTH: u32 = 16;

/// What [`admit_call`] reads about the frame making a call and the blocks it
/// can reach.
pub trait CallFrame {
    /// This frame's nesting depth: `0` at the top level, one more than its
    /// caller's for a frame a `call_block` produced.
    fn call_depth(&self) -> u32;

    /// The depth at which this frame may no longer call.
    fn max_call_depth(&self) -> u32;

    /// When this frame's work must end, if ever. A call made at or after it
    /// is refused with [`ErrorCode::DeadlineExceeded`] and
    /// [`Self::cancellation`] is set.
    fn deadline(&self) -> Option<Instant>;

    /// The frame's cancellation flag. Set: every call is refused with
    /// [`ErrorCode::Cancelled`].
    fn cancellation(&self) -> &AtomicBool;

    /// `name` resolved through the frame's alias map (one hop), or `name`
    /// itself when it is no alias.
    fn canonicalize<'a>(&'a self, name: &'a str) -> &'a str;

    /// The calling block's declared `call_block` allowlist
    /// ([`BlockInfo::call_allowlist`](wafer_block::BlockInfo::call_allowlist));
    /// `None` is unrestricted.
    fn caller_requires(&self) -> Option<&[String]>;

    /// The capabilities the block whose code runs on this frame enforces
    /// ([`Block::block_capabilities`]); `None` places no capability limit on
    /// which blocks it may call.
    fn caller_capabilities(&self) -> Option<BlockCapabilities>;

    /// The block registered under `name` — a registration name or an alias —
    /// or `None`.
    fn lookup(&self, name: &str) -> Option<Arc<dyn Block>>;

    /// The callee's declared interface and `call_block` allowlist. The
    /// default reads them from `block`'s `BlockInfo`; the runtime answers
    /// from the table it compiles at seal.
    fn callee_facts(&self, resolved: &str, block: &dyn Block) -> CalleeFacts<'_> {
        let _ = resolved;
        CalleeFacts::declared(block)
    }

    /// Every interface spec the action check knows, keyed by interface name.
    fn interface_specs(&self) -> &HashMap<String, InterfaceSpec>;

    /// Called when the callee declares an interface [`Self::interface_specs`]
    /// has no spec for. The call proceeds; the runtime logs a warning once
    /// per block.
    fn unknown_interface(&self, resolved: &str, interface: &str);
}

/// What the action gate and the callee's frame need about a callee.
pub struct CalleeFacts<'a> {
    /// The callee's declared interface (`BlockInfo::interface`).
    pub interface: Cow<'a, str>,
    /// The callee's own `call_block` allowlist, which becomes the
    /// [`CallFrame::caller_requires`] of the frame it runs on; `None` is
    /// unrestricted.
    pub requires: Option<Arc<Vec<String>>>,
}

impl CalleeFacts<'static> {
    /// The facts `block` declares in its `BlockInfo`.
    pub fn declared(block: &dyn Block) -> Self {
        let info = block.info();
        Self {
            requires: info.call_allowlist().map(Arc::new),
            interface: Cow::Owned(info.interface),
        }
    }
}

/// A call [`admit_call`] let through.
pub struct AdmittedCall<'a> {
    /// The callee's canonical name: the name it runs as, and the identity
    /// its own resource access is attributed to.
    pub resolved: &'a str,
    /// The callee.
    pub block: Arc<dyn Block>,
    /// The callee's `call_block` allowlist, for the frame it runs on.
    pub requires: Option<Arc<Vec<String>>>,
}

fn refuse(code: ErrorCode, message: impl Into<String>) -> WaferError {
    WaferError::new(code, message)
}

/// Run a call of `name` with `msg`, made from `frame`, through the gates the
/// runtime applies before any callee runs, in order:
///
/// 1. call depth — [`ErrorCode::ResourceExhausted`] at
///    [`CallFrame::max_call_depth`];
/// 2. deadline — [`ErrorCode::DeadlineExceeded`], setting the frame's
///    cancellation flag;
/// 3. cancellation — [`ErrorCode::Cancelled`];
/// 4. the caller's `requires` allowlist, matched against both the name
///    written and the name it resolves to — [`ErrorCode::PermissionDenied`];
/// 5. the caller's `call_block` capability, on the resolved name —
///    [`ErrorCode::PermissionDenied`];
/// 6. registration: the resolved name, else the name as written —
///    [`ErrorCode::Unimplemented`];
/// 7. the message's action (the `req.action` meta, else the message kind)
///    against the callee's declared interface, skipped for an interface with
///    no spec — [`ErrorCode::Unimplemented`].
///
/// WRAP resource access is not a gate here: the service a call reaches
/// authorizes the operation it decoded through
/// [`Context::check_resource_access`](crate::context::Context::check_resource_access).
pub fn admit_call<'a>(
    frame: &'a impl CallFrame,
    name: &'a str,
    msg: &Message,
) -> Result<AdmittedCall<'a>, WaferError> {
    // `call_depth` is this frame's depth, not a shared in-flight counter: a
    // frame at the maximum nesting depth cannot call further, while sibling
    // calls made from one frame never accumulate.
    let max = frame.max_call_depth();
    if frame.call_depth() >= max {
        return Err(refuse(
            ErrorCode::ResourceExhausted,
            format!("call_block depth exceeded maximum of {max} (calling '{name}')"),
        ));
    }

    // A passed deadline is its own code: the caller ran out of time, nobody
    // cancelled it.
    if let Some(deadline) = frame.deadline() {
        if Instant::now() >= deadline {
            frame
                .cancellation()
                .store(true, std::sync::atomic::Ordering::Relaxed);
            return Err(refuse(
                ErrorCode::DeadlineExceeded,
                format!("deadline exceeded before calling '{name}'"),
            ));
        }
    }
    if frame
        .cancellation()
        .load(std::sync::atomic::Ordering::Relaxed)
    {
        return Err(refuse(ErrorCode::Cancelled, "execution cancelled"));
    }

    // The canonical name is the identity every later decision uses: the
    // capability membership check, the lookup, and the callee's attribution.
    let resolved = frame.canonicalize(name);

    if let Some(requires) = frame.caller_requires() {
        if !requires.iter().any(|r| r == name || r == resolved) {
            return Err(refuse(
                ErrorCode::PermissionDenied,
                format!("block '{name}' not in requires list — call_block denied"),
            ));
        }
    }

    // `allows_call_block` is exact membership against the canonical names a
    // block declares, so it sees the resolved name — an alias would never
    // match.
    if let Some(caps) = frame.caller_capabilities() {
        if !caps.allows_call_block(resolved) {
            return Err(refuse(
                ErrorCode::PermissionDenied,
                format!("block capability denies call to '{name}'"),
            ));
        }
    }

    // The resolved name first, then the name as written — the same
    // canonicalize-then-fallback the flow runner and `Wafer::lookup_block`
    // use. `NotFound` is reserved for a service saying the thing a request
    // names does not exist; nothing to dispatch to is `Unimplemented`.
    let Some(block) = frame.lookup(resolved).or_else(|| frame.lookup(name)) else {
        return Err(refuse(
            ErrorCode::Unimplemented,
            format!("block '{name}' is not registered"),
        ));
    };

    let CalleeFacts {
        interface,
        requires,
    } = frame.callee_facts(resolved, &*block);

    // Two callers populate the action, in two places: the HTTP listener sets
    // the `req.action` meta (`kind` carries `"METHOD:/path"` for routing),
    // and SDK clients set `kind` to the service op (`"network.do"`) without
    // the meta. The meta wins when present.
    let action = match msg.action() {
        "" => msg.kind.as_str(),
        action => action,
    };
    match check_action_interface(resolved, &interface, action, frame.interface_specs()) {
        ActionCheck::Valid => {}
        ActionCheck::Invalid { message } => {
            return Err(refuse(ErrorCode::Unimplemented, message));
        }
        ActionCheck::UnknownInterface => frame.unknown_interface(resolved, &interface),
    }

    Ok(AdmittedCall {
        resolved,
        block,
        requires,
    })
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeSet, sync::atomic::Ordering, time::Duration};

    use parking_lot::Mutex;
    use wafer_block::{
        capabilities::Allowlist,
        context::Context,
        streams::{input::InputStream, output::OutputStream},
        types::{ActionSpec, BlockInfo},
    };
    use wafer_block_macro::wafer_async_trait;

    use super::*;

    struct Target {
        name: &'static str,
        interface: &'static str,
        requires: Vec<String>,
    }

    #[wafer_async_trait]
    impl Block for Target {
        fn info(&self) -> BlockInfo {
            BlockInfo::new(self.name, "0.0.1", self.interface, "call gate target")
                .requires(self.requires.clone())
        }

        async fn handle(
            &self,
            _ctx: &dyn Context,
            _msg: Message,
            _input: InputStream,
        ) -> OutputStream {
            OutputStream::respond(b"ok".to_vec())
        }
    }

    /// A frame of the shape an embedder's own `Context` has: every answer a
    /// plain field, the callee's facts read from its `BlockInfo`.
    struct Frame {
        depth: u32,
        deadline: Option<Instant>,
        cancelled: AtomicBool,
        aliases: HashMap<String, String>,
        requires: Option<Vec<String>>,
        capabilities: Option<BlockCapabilities>,
        blocks: HashMap<String, Arc<dyn Block>>,
        specs: HashMap<String, InterfaceSpec>,
        unknown: Mutex<Vec<(String, String)>>,
    }

    impl Frame {
        /// `acme/db` (interface `store@v1`: action `get`) registered and
        /// aliased as `db`; `acme/free` under an interface with no spec.
        fn new() -> Self {
            let mut blocks: HashMap<String, Arc<dyn Block>> = HashMap::new();
            blocks.insert(
                "acme/db".into(),
                Arc::new(Target {
                    name: "acme/db",
                    interface: "store@v1",
                    requires: vec!["acme/log".into()],
                }),
            );
            blocks.insert(
                "acme/free".into(),
                Arc::new(Target {
                    name: "acme/free",
                    interface: "custom@v1",
                    requires: Vec::new(),
                }),
            );
            let spec = InterfaceSpec {
                name: "store@v1".into(),
                description: "store".into(),
                actions: HashMap::from([(
                    "get".to_string(),
                    ActionSpec {
                        description: "get".into(),
                        message_schema: None,
                        response_schema: None,
                    },
                )]),
            };
            Self {
                depth: 0,
                deadline: None,
                cancelled: AtomicBool::new(false),
                aliases: HashMap::from([("db".to_string(), "acme/db".to_string())]),
                requires: None,
                capabilities: None,
                blocks,
                specs: HashMap::from([(spec.name.clone(), spec)]),
                unknown: Mutex::new(Vec::new()),
            }
        }
    }

    impl CallFrame for Frame {
        fn call_depth(&self) -> u32 {
            self.depth
        }
        fn max_call_depth(&self) -> u32 {
            DEFAULT_MAX_CALL_DEPTH
        }
        fn deadline(&self) -> Option<Instant> {
            self.deadline
        }
        fn cancellation(&self) -> &AtomicBool {
            &self.cancelled
        }
        fn canonicalize<'a>(&'a self, name: &'a str) -> &'a str {
            self.aliases.get(name).map_or(name, String::as_str)
        }
        fn caller_requires(&self) -> Option<&[String]> {
            self.requires.as_deref()
        }
        fn caller_capabilities(&self) -> Option<BlockCapabilities> {
            self.capabilities.clone()
        }
        fn lookup(&self, name: &str) -> Option<Arc<dyn Block>> {
            self.blocks.get(name).cloned()
        }
        fn interface_specs(&self) -> &HashMap<String, InterfaceSpec> {
            &self.specs
        }
        fn unknown_interface(&self, resolved: &str, interface: &str) {
            self.unknown
                .lock()
                .push((resolved.to_string(), interface.to_string()));
        }
    }

    fn get() -> Message {
        Message::new("get")
    }

    fn refusal(frame: &Frame, name: &str, msg: &Message) -> (ErrorCode, String) {
        match admit_call(frame, name, msg) {
            Ok(admitted) => panic!("expected a refusal, admitted {}", admitted.resolved),
            Err(e) => (e.code, e.message),
        }
    }

    #[test]
    fn an_alias_is_admitted_as_its_target_with_the_targets_allowlist() {
        let frame = Frame::new();
        let admitted = admit_call(&frame, "db", &get()).expect("admitted");
        assert_eq!(admitted.resolved, "acme/db");
        assert_eq!(admitted.block.info().name, "acme/db");
        assert_eq!(
            admitted.requires.as_deref().map(Vec::as_slice),
            Some(["acme/log".to_string()].as_slice())
        );
    }

    #[test]
    fn a_frame_at_the_maximum_depth_cannot_call_and_one_below_it_can() {
        let mut frame = Frame::new();
        frame.depth = DEFAULT_MAX_CALL_DEPTH - 1;
        admit_call(&frame, "db", &get()).expect("one below the ceiling calls");
        frame.depth = DEFAULT_MAX_CALL_DEPTH;
        assert_eq!(
            refusal(&frame, "db", &get()),
            (
                ErrorCode::ResourceExhausted,
                "call_block depth exceeded maximum of 16 (calling 'db')".to_string()
            )
        );
    }

    #[test]
    fn depth_is_checked_before_cancellation() {
        let mut frame = Frame::new();
        frame.depth = DEFAULT_MAX_CALL_DEPTH;
        frame.cancelled.store(true, Ordering::Relaxed);
        assert_eq!(
            refusal(&frame, "db", &get()).0,
            ErrorCode::ResourceExhausted
        );
    }

    #[test]
    fn a_passed_deadline_refuses_and_cancels_the_frame() {
        let mut frame = Frame::new();
        frame.deadline = Some(Instant::now() - Duration::from_millis(1));
        assert_eq!(
            refusal(&frame, "db", &get()),
            (
                ErrorCode::DeadlineExceeded,
                "deadline exceeded before calling 'db'".to_string()
            )
        );
        assert!(frame.cancelled.load(Ordering::Relaxed));
    }

    #[test]
    fn a_cancelled_frame_cannot_call() {
        let frame = Frame::new();
        frame.cancelled.store(true, Ordering::Relaxed);
        assert_eq!(
            refusal(&frame, "db", &get()),
            (ErrorCode::Cancelled, "execution cancelled".to_string())
        );
    }

    #[test]
    fn requires_matches_the_name_written_or_the_name_it_resolves_to() {
        let mut frame = Frame::new();
        frame.requires = Some(vec!["db".into()]);
        admit_call(&frame, "db", &get()).expect("the alias as written");
        frame.requires = Some(vec!["acme/db".into()]);
        admit_call(&frame, "db", &get()).expect("the name it resolves to");
        frame.requires = Some(vec!["acme/other".into()]);
        assert_eq!(
            refusal(&frame, "db", &get()),
            (
                ErrorCode::PermissionDenied,
                "block 'db' not in requires list — call_block denied".to_string()
            )
        );
    }

    #[test]
    fn the_capability_gate_sees_the_resolved_name() {
        let mut frame = Frame::new();
        let mut caps = BlockCapabilities::none();
        caps.callable_blocks = Allowlist::Only(BTreeSet::from(["acme/db".to_string()]));
        frame.capabilities = Some(caps);
        admit_call(&frame, "db", &get()).expect("the alias of an allowed block");
        frame.capabilities = Some(BlockCapabilities::none());
        assert_eq!(
            refusal(&frame, "db", &get()),
            (
                ErrorCode::PermissionDenied,
                "block capability denies call to 'db'".to_string()
            )
        );
    }

    #[test]
    fn a_name_whose_alias_target_is_missing_falls_back_to_the_name_written() {
        let mut frame = Frame::new();
        frame
            .aliases
            .insert("acme/free".to_string(), "acme/gone".to_string());
        let admitted = admit_call(&frame, "acme/free", &get()).expect("found as written");
        assert_eq!(admitted.block.info().name, "acme/free");
        assert_eq!(admitted.resolved, "acme/gone");
    }

    #[test]
    fn an_unregistered_block_is_unimplemented() {
        assert_eq!(
            refusal(&Frame::new(), "acme/nope", &get()),
            (
                ErrorCode::Unimplemented,
                "block 'acme/nope' is not registered".to_string()
            )
        );
    }

    #[test]
    fn the_action_is_the_meta_else_the_kind_checked_against_the_interface() {
        let frame = Frame::new();
        assert_eq!(
            refusal(&frame, "db", &Message::new("put")),
            (
                ErrorCode::Unimplemented,
                "block 'acme/db' with interface 'store@v1' does not expose action 'put'"
                    .to_string()
            )
        );
        let mut meta_get = Message::new("put");
        meta_get.set_meta(wafer_block::meta::META_REQ_ACTION, "get");
        admit_call(&frame, "db", &meta_get).expect("the meta action wins over the kind");
    }

    #[test]
    fn an_interface_without_a_spec_is_reported_and_called() {
        let frame = Frame::new();
        admit_call(&frame, "acme/free", &Message::new("anything")).expect("called");
        assert_eq!(
            *frame.unknown.lock(),
            vec![("acme/free".to_string(), "custom@v1".to_string())]
        );
    }
}
