//! The registration core: the block registry (names → instances, aliases,
//! init slots, registrars/expanders, interface specs, block configs) plus the
//! WRAP grant/capability state collected during registration. These fields are
//! welded together — registration both inserts blocks and collects/validates
//! their grants — so they are grouped as one cohesive sub-struct rather than
//! split. See the god-struct decomposition spec.

use std::{collections::HashMap, sync::Arc};

use crate::{
    block::Block,
    platform::{ConfigExpanderFn, RegistrarFn},
};

/// WRAP (resource-access) state, nested inside [`RegistrationCore`] because it
/// is collected and rebuilt during block registration.
pub(crate) struct WrapState {
    /// Merged grant list (code-declared + external). Cloned into every
    /// [`RuntimeContext`](crate::context::RuntimeContext).
    pub(crate) grants: Arc<Vec<wafer_block::types::ResourceGrant>>,
    /// Extra grants supplied via `Wafer::add_wrap_grants` (e.g. loaded from a
    /// database). Kept separate so `set_admin_block` can rebuild the
    /// code-declared portion without losing these.
    pub(crate) grants_external: Vec<wafer_block::types::ResourceGrant>,
    /// The block ID granted admin privileges (exact match).
    pub(crate) admin_block: Arc<String>,
    /// Effective capabilities per block after declared ∩ config ∩ host
    /// intersection. Computed at `resolve()` time.
    pub(crate) effective_capabilities: Arc<HashMap<String, wafer_block::BlockCapabilities>>,
    /// Accumulator for grant-validation failures; drained + checked by
    /// `Wafer::start()`, which fails boot with `RuntimeError::GrantsRejected`
    /// if non-empty.
    pub(crate) validation_errors: Vec<crate::error::GrantValidationError>,
}

impl WrapState {
    fn new() -> Self {
        Self {
            grants: Arc::new(Vec::new()),
            grants_external: Vec::new(),
            admin_block: Arc::new(String::new()),
            effective_capabilities: Arc::new(HashMap::new()),
            validation_errors: Vec::new(),
        }
    }
}

/// Block-registration state grouped out of the `Wafer` god-struct: the
/// registry maps + the nested [`WrapState`].
pub(crate) struct RegistrationCore {
    /// Registered blocks (name → instance). Grows during registration.
    pub(crate) blocks: HashMap<String, Arc<dyn Block>>,
    /// All registered blocks + aliases, shared with contexts.
    pub(crate) all_blocks: Arc<HashMap<String, Arc<dyn Block>>>,
    /// Alias mappings (e.g. `wafer-run/database` → `wafer-run/sqlite`).
    pub(crate) aliases: Arc<HashMap<String, String>>,
    /// Per-block init slots for lazy-once-success caching.
    pub(crate) slots: Arc<HashMap<String, Arc<crate::runtime::slot::BlockSlot>>>,
    /// Named registrars: functions that register blocks/flows by name.
    pub(crate) registrars: HashMap<String, RegistrarFn>,
    /// Config expanders: split a composite config into per-block configs.
    pub(crate) config_expanders: HashMap<String, ConfigExpanderFn>,
    /// Registered interface specifications.
    pub(crate) interface_specs: HashMap<String, wafer_block::InterfaceSpec>,
    /// Block configurations loaded from blocks.json (name → config JSON).
    pub(crate) block_configs: HashMap<String, serde_json::Value>,
    /// WRAP grant/capability state.
    pub(crate) wrap: WrapState,
}

impl RegistrationCore {
    /// Empty registry seeded with the built-in interface specs (matches the
    /// previous `Wafer::empty()` initialization).
    pub(crate) fn new() -> Self {
        Self {
            blocks: HashMap::new(),
            all_blocks: Arc::new(HashMap::new()),
            aliases: Arc::new(HashMap::new()),
            slots: Arc::new(HashMap::new()),
            registrars: HashMap::new(),
            config_expanders: HashMap::new(),
            interface_specs: wafer_block::interfaces::all()
                .into_iter()
                .map(|s| (s.name.clone(), s))
                .collect(),
            block_configs: HashMap::new(),
            wrap: WrapState::new(),
        }
    }
}
