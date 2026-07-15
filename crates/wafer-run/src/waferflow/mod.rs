//! WaferFlow execution: seal-time compilation ([`plan`]) plus the runtime
//! walker ([`executor`]). Flows are run through [`crate::Wafer::run`], which
//! feeds the compiled plan to the executor.

/// Executor that walks a compiled flow plan and dispatches to blocks.
pub(crate) mod executor;
/// Seal-time flow compilation (PERF-03): expressions parsed, configs
/// flattened, blocks resolved, jump targets indexed — once per flow.
pub(crate) mod plan;

pub(crate) use executor::execute as execute_waferflow;
