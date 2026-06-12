//! Config-expansion passes run during [`Wafer::seal`]: composite block
//! configs, declarative flow `config_map` / `config_defaults`, and `"uses"`
//! contributions between block configs.

use std::collections::HashMap;

use super::Wafer;

impl Wafer {
    /// Gather `"uses"` contributions from all block configs and deep-merge them
    /// into the target infrastructure block configs.
    pub(crate) fn expand_composite_configs(&mut self) {
        let keys: Vec<String> = self
            .registration
            .block_configs
            .keys()
            .filter(|k| self.registration.config_expanders.contains_key(k.as_str()))
            .cloned()
            .collect();

        for key in keys {
            if let Some(config) = self.registration.block_configs.remove(&key) {
                if let Some(expander) = self.registration.config_expanders.get(&key) {
                    for (name, val) in expander(config) {
                        let entry = self
                            .registration
                            .block_configs
                            .entry(name)
                            .or_insert_with(|| serde_json::Value::Object(Default::default()));
                        deep_merge(entry, &val);
                    }
                }
            }
        }
    }

    /// Expand declarative `config_map` and `config_defaults` from WaferFlow definitions.
    pub(crate) fn expand_declarative_flow_configs(&mut self) {
        #[expect(
            clippy::type_complexity,
            reason = "tuple mirrors the accumulated shape of the eligible iterator; no useful alias"
        )]
        let eligible: Vec<(
            String,
            HashMap<String, wafer_flow::ConfigMapEntry>,
            HashMap<String, serde_json::Value>,
        )> = self
            .flows
            .values()
            .filter(|f| f.config_map.as_ref().is_some_and(|m| !m.is_empty()))
            .filter(|f| self.registration.block_configs.contains_key(&f.id))
            .map(|f| {
                (
                    f.id.clone(),
                    f.config_map.clone().unwrap_or_default(),
                    f.config_defaults.clone().unwrap_or_default(),
                )
            })
            .collect();

        for (flow_id, config_map, config_defaults) in eligible {
            let Some(flow_config) = self.registration.block_configs.remove(&flow_id) else {
                continue;
            };

            // 1. Apply config_defaults to target blocks
            for (target, defaults) in &config_defaults {
                let entry = self
                    .registration
                    .block_configs
                    .entry(target.clone())
                    .or_insert_with(|| serde_json::Value::Object(Default::default()));
                deep_merge(entry, defaults);
            }

            // 2. Route config_map keys to target blocks
            if let Some(obj) = flow_config.as_object() {
                for (user_key, mapping) in &config_map {
                    if let Some(value) = obj.get(user_key) {
                        let entry = self
                            .registration
                            .block_configs
                            .entry(mapping.target.clone())
                            .or_insert_with(|| serde_json::Value::Object(Default::default()));
                        let contribution =
                            serde_json::json!({ mapping.key.clone(): value.clone() });
                        deep_merge(entry, &contribution);
                    }
                }
            }
        }
    }

    /// Merge `"uses"` contributions across block configs into their target
    /// blocks' configs, then strip the `"uses"` keys.
    pub(crate) fn gather_uses_configs(&mut self) {
        let mut contributions: Vec<(String, serde_json::Value)> = Vec::new();

        for config in self.registration.block_configs.values() {
            if let Some(uses) = config.get("uses").and_then(|v| v.as_object()) {
                for (target, contrib) in uses {
                    contributions.push((target.clone(), contrib.clone()));
                }
            }
        }

        for (target, contrib) in contributions {
            let resolved_target = self.canonicalize(&target).to_string();
            let entry = self
                .registration
                .block_configs
                .entry(resolved_target)
                .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
            deep_merge(entry, &contrib);
        }

        for config in self.registration.block_configs.values_mut() {
            if let Some(obj) = config.as_object_mut() {
                obj.remove("uses");
            }
        }
    }
}

/// Deep-merge `src` into `dst`. For objects, keys are combined recursively.
/// For non-object values, `dst`'s existing value wins (contributors cannot
/// override the target block's own scalar values).
pub(crate) fn deep_merge(dst: &mut serde_json::Value, src: &serde_json::Value) {
    if let (serde_json::Value::Object(dst_map), serde_json::Value::Object(src_map)) = (dst, src) {
        for (key, src_val) in src_map {
            if let Some(dst_val) = dst_map.get_mut(key) {
                deep_merge(dst_val, src_val);
            } else {
                dst_map.insert(key.clone(), src_val.clone());
            }
        }
    }
}
