//! The registration core: the block registry (names → instances, aliases,
//! init slots, registrars/expanders, interface specs, block configs) plus the
//! WRAP grant/capability state collected during registration. These fields are
//! welded together — registration both inserts blocks and collects/validates
//! their grants — so they are grouped as one cohesive sub-struct rather than
//! split. See the god-struct decomposition spec.

use std::{collections::HashMap, sync::Arc};

use crate::{
    block::Block,
    error::RuntimeError,
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

impl RegistrationCore {
    /// Register an alias mapping (single-hop; chained aliases rejected so
    /// lookup stays O(1)). See [`crate::error::AliasError`] for rejection
    /// reasons.
    pub(crate) fn add_alias(
        &mut self,
        alias: String,
        target: String,
    ) -> Result<(), crate::error::AliasError> {
        if alias == target {
            return Err(crate::error::AliasError::Cycle { alias });
        }
        if self.aliases.contains_key(&target) {
            return Err(crate::error::AliasError::TargetIsAlias { alias, target });
        }
        if self.aliases.values().any(|t| t == &alias) {
            return Err(crate::error::AliasError::AliasIsExistingTarget { alias });
        }
        Arc::make_mut(&mut self.aliases).insert(alias, target);
        Ok(())
    }

    /// Resolve `name` through the alias map, single-hop. Returns the alias
    /// target if `name` is an alias, else `name` itself.
    pub(crate) fn canonicalize<'a>(&'a self, name: &'a str) -> &'a str {
        self.aliases.get(name).map_or(name, |s| s.as_str())
    }

    /// Resolve a dispatch target through the alias map, returning the
    /// canonical name and the block.
    ///
    /// Tries the alias-resolved name first, then the original. Under the
    /// current invariant these always agree: `rebuild_all_blocks` is the sole
    /// writer of `all_blocks` and inserts an alias key only alongside its
    /// registered target, so a hit on the original name implies a hit on the
    /// resolved one. The `or_else` is therefore a cheap defensive fallback
    /// (and preserves the pre-refactor lookup) rather than a reachable path.
    pub(crate) fn lookup_with_alias<'a>(
        &'a self,
        name: &'a str,
    ) -> Option<(&'a str, Arc<dyn Block>)> {
        let resolved = self.canonicalize(name);
        self.all_blocks
            .get(resolved)
            .or_else(|| self.all_blocks.get(name))
            .map(|b| (resolved, b.clone()))
    }

    /// Rebuild the `all_blocks` map from registered blocks + aliases. Call
    /// after `resolve()` completes.
    pub(crate) fn rebuild_all_blocks(&mut self) {
        let mut map = HashMap::new();
        for (name, block) in &self.blocks {
            map.insert(name.clone(), block.clone());
        }
        // Alias names point to the same Arc<dyn Block> as their target.
        for (alias, target) in self.aliases.as_ref() {
            if let Some(block) = self.blocks.get(target) {
                map.insert(alias.clone(), block.clone());
            }
        }
        self.all_blocks = Arc::new(map);
    }

    /// Set the admin block ID, then re-scan every registered block's typed
    /// WRAP grants so admin-declared grants registered before this call are
    /// collected. External grants (via [`add_wrap_grants`](Self::add_wrap_grants))
    /// are preserved.
    pub(crate) fn set_admin_block(&mut self, block_id: String) {
        self.wrap.admin_block = Arc::new(block_id);
        self.rebuild_wrap_grants();
    }

    /// Rebuild `self.wrap.grants` from scratch: walk every registered block's
    /// declared grants (filtered through the per-block validator) and append
    /// the externally-supplied grants. Called by
    /// [`set_admin_block`](Self::set_admin_block).
    fn rebuild_wrap_grants(&mut self) {
        self.wrap.validation_errors.clear(); // full re-walk; old errors are stale
        let admin_block: String = (*self.wrap.admin_block).clone();
        let mut merged: Vec<wafer_block::types::ResourceGrant> = Vec::new();
        // Walk blocks in deterministic order for snapshot stability.
        let mut names: Vec<&String> = self.blocks.keys().collect();
        names.sort();
        for name in names {
            let block = &self.blocks[name];
            let info = block.info();
            let outcome = crate::runtime::lifecycle::validate_and_collect_grants_for_block(
                &info,
                &admin_block,
            );
            merged.extend(outcome.accepted);
            self.wrap.validation_errors.extend(outcome.rejected);
        }
        merged.extend(self.wrap.grants_external.iter().cloned());
        self.wrap.grants = Arc::new(merged);
    }

    /// Add extra WRAP grants (e.g. loaded from a database). Appended to the
    /// existing grants and tracked separately so a later
    /// [`set_admin_block`](Self::set_admin_block) rescan does not drop them.
    pub(crate) fn add_wrap_grants(&mut self, grants: Vec<wafer_block::types::ResourceGrant>) {
        self.wrap.grants_external.extend(grants.iter().cloned());
        let mut all = (*self.wrap.grants).clone();
        all.extend(grants);
        self.wrap.grants = Arc::new(all);
    }

    /// Shared validation + insertion logic for code-registered blocks. Used by
    /// the [`BlockRegistry`](wafer_block::registry::BlockRegistry) trait impl
    /// (via `Wafer`) and `Wafer::load_inventory_blocks`. `asset_loader` is
    /// forwarded to any `WasmiBlock` so `set_asset_loader` and `register_block`
    /// can be called in any order (the block's only `WasmState` need).
    #[cfg_attr(not(feature = "wasmi"), allow(unused_variables))]
    pub(crate) fn register_block_inner(
        &mut self,
        name: &str,
        block: Arc<dyn Block>,
        asset_loader: &Arc<dyn crate::asset_loader::LoadAssetCallback>,
    ) -> Result<(), RuntimeError> {
        if self.blocks.contains_key(name) {
            return Err(RuntimeError::DuplicateBlock {
                name: name.to_string(),
            });
        }

        // Validate block name format.
        crate::runtime::validate_block_name(name)?;

        // Reject declared config keys under platform-reserved prefixes
        // (e.g. SOLOBASE_SHARED__): those keys are platform-owned, not
        // block-owned. Fails boot loudly rather than silently accepting a
        // key the block can never legitimately write.
        let info = block.info();
        info.validate().map_err(RuntimeError::ReservedConfigKey)?;

        // Validate that all config_keys use the block's own prefix.
        // Block "suppers-ai/auth" may only declare keys starting with "SUPPERS_AI__AUTH__".
        if !info.config_keys.is_empty() {
            let expected_prefix = crate::runtime::block_name_to_var_prefix(name);
            for var in &info.config_keys {
                if !var.key.starts_with(&expected_prefix) {
                    return Err(RuntimeError::ConfigVarPrefix {
                        name: name.to_string(),
                        var: var.key.clone(),
                        prefix: expected_prefix,
                    });
                }
            }
        }

        // Validate this block's WRAP grants and append the accepted ones.
        // Typed grants declared before `set_admin_block` are deferred — that
        // rescan re-collects them, so registration order doesn't matter.
        let admin_block: String = (*self.wrap.admin_block).clone();
        let outcome =
            crate::runtime::lifecycle::validate_and_collect_grants_for_block(&info, &admin_block);
        if !outcome.accepted.is_empty() {
            let mut all = (*self.wrap.grants).clone();
            all.extend(outcome.accepted);
            self.wrap.grants = Arc::new(all);
        }
        self.wrap.validation_errors.extend(outcome.rejected);

        // Propagate the current asset loader to the block before inserting.
        // Only WasmiBlock instances override `as_any()`, so native blocks are
        // skipped without any unsafe code.
        #[cfg(feature = "wasmi")]
        if let Some(wasmi_block) = block
            .as_any()
            .and_then(|any| any.downcast_ref::<crate::wasm::WasmiBlock>())
        {
            wasmi_block.set_asset_loader(asset_loader.clone());
        }

        self.blocks.insert(name.to_string(), block);
        // Pair every registration with a fresh init slot so `Wafer::init_block`
        // can lazily run lifecycle(Init) once per block. Mutate through
        // `Arc::make_mut` so live `RuntimeContext` clones sharing the previous
        // Arc keep their snapshot.
        Arc::make_mut(&mut self.slots).insert(
            name.to_string(),
            Arc::new(crate::runtime::slot::BlockSlot::new()),
        );
        Ok(())
    }

    /// Insert a block downloaded by `seal()`'s remote-resolution path while
    /// running the same WRAP grant validation + slot allocation that
    /// [`register_block_inner`](Self::register_block_inner) performs for
    /// code-registered blocks.
    ///
    /// Block-name and config-key-prefix validation are intentionally skipped:
    /// remote blocks come in under names the user already declared, and
    /// re-validating would reject blocks already accepted by their config.
    /// Duplicate-registration is not checked because every remote-path call
    /// site filters `blocks.contains_key(name)` before invoking this helper.
    pub(crate) fn register_remote_block(
        &mut self,
        name: &str,
        block: Arc<dyn Block>,
    ) -> Result<(), RuntimeError> {
        let info = block.info();
        // Reserved-prefix declaration is a platform-ownership invariant, not a
        // naming-convention nicety, so it applies to remote blocks too (unlike
        // the block-name / config-prefix checks this helper skips).
        info.validate().map_err(RuntimeError::ReservedConfigKey)?;
        let admin_block: String = (*self.wrap.admin_block).clone();
        let outcome =
            crate::runtime::lifecycle::validate_and_collect_grants_for_block(&info, &admin_block);
        if !outcome.accepted.is_empty() {
            let mut all = (*self.wrap.grants).clone();
            all.extend(outcome.accepted);
            self.wrap.grants = Arc::new(all);
        }
        self.wrap.validation_errors.extend(outcome.rejected);

        self.blocks.insert(name.to_string(), block);
        Arc::make_mut(&mut self.slots).insert(
            name.to_string(),
            Arc::new(crate::runtime::slot::BlockSlot::new()),
        );
        Ok(())
    }

    /// Register an interface specification, overwriting any existing spec with
    /// the same name.
    pub(crate) fn register_interface(&mut self, spec: wafer_block::InterfaceSpec) {
        self.interface_specs.insert(spec.name.clone(), spec);
    }

    /// Store a block's JSON config (from `add_block_config`), keyed by name.
    pub(crate) fn add_block_config(&mut self, name: &str, config: serde_json::Value) {
        self.block_configs.insert(name.to_string(), config);
    }
}
