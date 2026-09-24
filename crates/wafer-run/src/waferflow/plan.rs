//! Seal-time compiled flow plan (PERF-03).
//!
//! [`compile_flow`] turns a parsed [`WaferFlow`] into a [`CompiledFlow`]
//! whose per-step data is fully precomputed: alias-resolved block handles
//! and init slots, parsed `step.config` maps, compiled `input` templates,
//! compiled `each` paths, compiled `when` conditions with jump targets
//! resolved to step indices, the flow-level timeout, and the
//! "uses accumulator" flag. The executor then does zero parsing and zero
//! linear searches per step.
//!
//! Every flow reaching compilation passed [`wafer_flow::validate`] in
//! [`Wafer::add_flow`], so its jump targets name top-level steps and its
//! expressions parse. A block missing at seal time compiles to a form that
//! reproduces the runtime "block not found" error if — and only if — that
//! step actually runs. Nothing new fails at seal time.

use std::{collections::HashMap, sync::Arc, time::Duration};

use wafer_block::{config::parse_config_map, Block};
use wafer_flow::{CompiledCondition, CompiledPath, CompiledTemplate, OnError, Step, WaferFlow};

use crate::runtime::{slot::BlockSlot, Wafer};

/// A flow with every per-step decision precomputed at seal time.
pub(crate) struct CompiledFlow {
    /// Flow id (error messages + observability).
    pub(crate) id: String,
    /// The flow's `config` timeout.
    pub(crate) timeout: Option<Duration>,
    /// The flow's `config.max_steps` (1000 when unset): the shared step
    /// budget for one execution.
    pub(crate) max_steps: usize,
    /// The flow's `config.on_error`.
    pub(crate) on_error: OnError,
    /// True if any step (recursively) reads from or writes to the
    /// accumulator, in which case the flow input must be parsed and stored
    /// under `$.input` (was recomputed per run).
    pub(crate) uses_accumulator: bool,
    /// Compiled top-level steps, in definition order.
    pub(crate) steps: Vec<CompiledStep>,
}

/// One step with its dispatch target, config, and expressions precompiled.
pub(crate) struct CompiledStep {
    /// Step id (accumulator key, error messages, observability node path).
    pub(crate) id: String,
    /// Block name exactly as the flow author wrote it (observability +
    /// error messages), possibly an alias.
    pub(crate) block_label: String,
    /// Seal-time resolved dispatch target. `None` preserves the runtime
    /// "block '…' not found in step '…'" error for flows compiled while the
    /// referenced block is unregistered (e.g. a flow added after `seal()`).
    pub(crate) target: Option<StepTarget>,
    /// Compiled `input` template (pipeline mode when present).
    pub(crate) input: Option<CompiledTemplate>,
    /// Parsed `step.config` (empty map when the step declares none).
    pub(crate) config: Arc<HashMap<String, String>>,
    /// Compiled `each` fan-out expression.
    pub(crate) each: Option<CompiledEach>,
    /// Compiled parallel branches.
    pub(crate) parallel: Option<Vec<CompiledBranch>>,
    /// Compiled `next` routing entries.
    pub(crate) next: Option<Vec<CompiledNextEntry>>,
}

/// Alias-resolved dispatch target: canonical name, block handle, init slot.
pub(crate) struct StepTarget {
    /// Canonical (alias-resolved) block name — the context `node_id` (WRAP
    /// attribution) and init identity.
    pub(crate) name: String,
    /// The resolved block instance.
    pub(crate) block: Arc<dyn Block>,
    /// The block's once-success init slot (was a per-invocation map lookup).
    pub(crate) slot: Arc<BlockSlot>,
}

/// An `each` expression compiled once; the raw string is kept because both
/// runtime error messages quote it.
pub(crate) struct CompiledEach {
    /// The expression as written.
    pub(crate) raw: String,
    /// The compiled path.
    pub(crate) path: CompiledPath,
}

/// One parallel branch: its compiled steps in order.
pub(crate) struct CompiledBranch {
    /// Compiled branch steps.
    pub(crate) steps: Vec<CompiledStep>,
}

/// One `next` routing entry with its condition compiled and its jump target
/// pre-resolved.
pub(crate) struct CompiledNextEntry {
    /// `when` condition — raw string (for the COR-03 error message) plus
    /// compiled form. `None` is the unconditional default entry.
    pub(crate) when: Option<(String, CompiledCondition)>,
    /// Where the entry routes when taken.
    pub(crate) target: NextTarget,
}

/// Pre-resolved routing target of a [`CompiledNextEntry`].
pub(crate) enum NextTarget {
    /// Jump to the top-level step at this index (was a linear id search per
    /// jump).
    Step(usize),
    /// `step` names an id that is not a top-level step. `validate` refuses
    /// such a flow at `add_flow`; if one is compiled regardless, taking the
    /// entry fails with `NotFound` rather than jumping anywhere.
    MissingStep(String),
    /// Transfer control to another flow.
    Flow(String),
    /// Neither `step` nor `flow`. `validate` refuses such a flow at
    /// `add_flow`; if one is compiled regardless, taking the entry ends
    /// routing and falls through to sequential advance.
    None,
}

/// The step budget of a flow whose config sets no `max_steps`.
const DEFAULT_MAX_STEPS: usize = 1000;

/// Compile `flow` against the runtime's current registrations. Called once
/// per flow at `seal()`; also used as the ad-hoc fallback for flows added
/// after sealing.
pub(crate) fn compile_flow(wafer: &Wafer, flow: &WaferFlow) -> CompiledFlow {
    let config = flow.config.clone().unwrap_or_default();
    // A `max_steps` beyond `usize` (32-bit targets) cannot be reached.
    let max_steps = config.max_steps.map_or(DEFAULT_MAX_STEPS, |n| {
        usize::try_from(n.get()).unwrap_or(usize::MAX)
    });

    // Jump-target index over TOP-LEVEL steps only, matching the executor's
    // former `steps.iter().position(...)` search (branch-local ids were
    // never valid jump targets). First occurrence wins, as `position` did.
    let mut index: HashMap<&str, usize> = HashMap::with_capacity(flow.steps.len());
    for (i, step) in flow.steps.iter().enumerate() {
        index.entry(step.id.as_str()).or_insert(i);
    }

    CompiledFlow {
        id: flow.id.clone(),
        timeout: config.timeout(),
        max_steps,
        on_error: config.on_error(),
        uses_accumulator: steps_use_accumulator(&flow.steps),
        steps: flow
            .steps
            .iter()
            .map(|s| compile_step(wafer, s, &index))
            .collect(),
    }
}

fn compile_step(wafer: &Wafer, step: &Step, index: &HashMap<&str, usize>) -> CompiledStep {
    let target = wafer.lookup_block(&step.block).map(|(resolved, block)| {
        // Every registered block has a paired slot (`register_block_inner` /
        // `register_remote_block`); a missing entry is a runtime invariant
        // violation, matching the dispatch-time expectation this replaces.
        let slot = wafer
            .registration
            .slots
            .get(resolved)
            .cloned()
            .expect("slot must exist for any registered block");
        StepTarget {
            name: resolved.to_string(),
            block,
            slot,
        }
    });

    let next = step.next.as_ref().map(|entries| {
        entries
            .iter()
            .map(|e| CompiledNextEntry {
                when: e
                    .when
                    .as_ref()
                    .map(|w| (w.clone(), CompiledCondition::compile(w))),
                // `validate` refuses an entry naming both `step` and
                // `flow`; `step` is read first.
                target: match (&e.step, &e.flow) {
                    (Some(s), _) => index.get(s.as_str()).map_or_else(
                        || NextTarget::MissingStep(s.clone()),
                        |i| NextTarget::Step(*i),
                    ),
                    (None, Some(f)) => NextTarget::Flow(f.clone()),
                    (None, None) => NextTarget::None,
                },
            })
            .collect()
    });

    CompiledStep {
        id: step.id.clone(),
        block_label: step.block.clone(),
        target,
        input: step.input.as_ref().map(CompiledTemplate::compile),
        config: Arc::new(
            step.config
                .as_ref()
                .map(parse_config_map)
                .unwrap_or_default(),
        ),
        each: step.each.as_ref().map(|raw| CompiledEach {
            raw: raw.clone(),
            path: CompiledPath::compile(raw),
        }),
        parallel: step.parallel.as_ref().map(|branches| {
            branches
                .iter()
                .map(|b| CompiledBranch {
                    steps: b
                        .steps
                        .iter()
                        .map(|s| compile_step(wafer, s, index))
                        .collect(),
                })
                .collect()
        }),
        next,
    }
}

/// True if any step (recursively through parallel branches) reads from or
/// writes to the accumulator.
fn steps_use_accumulator(steps: &[Step]) -> bool {
    steps.iter().any(|s| {
        s.input.is_some()
            || s.each.is_some()
            || s.parallel
                .as_ref()
                .is_some_and(|branches| branches.iter().any(|b| steps_use_accumulator(&b.steps)))
    })
}
