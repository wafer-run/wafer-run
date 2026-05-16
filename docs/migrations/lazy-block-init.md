# Migrating to lazy block init

This release switches `wafer-run` from eager `lifecycle(Init)` at boot to
lazy per-block init on first dispatch. Embedders (solobase, gizza-ai, etc.)
need to update their construction path.

## Before

```rust
use std::collections::HashMap;
use wafer_run::Wafer;

// Pre-load all block configs into a single HashMap.
let mut config = HashMap::new();
config.insert("SUPPERS_AI__AUTH__JWT_SECRET".to_string(), secret);
// ... many more keys ...

let mut wafer = Wafer::new()?;  // old signature: no args
wafer.register_block("suppers-ai/auth", Arc::new(AuthBlock::new()))?;
wafer.add_block_config("suppers-ai/auth", auth_json);
// ...
wafer.start_without_bind().await?;
// Init was dispatched eagerly to every block; if any failed, this errored.
```

## After

```rust
use std::sync::Arc;
use wafer_run::{Wafer, StaticConfigSource};

// 1. Implement ConfigSource for your environment (env vars, D1, etc.) —
//    or use StaticConfigSource for tests.
let cfg_source: Arc<dyn wafer_run::ConfigSource> =
    Arc::new(MyEnvConfigSource::new());

// 2. Construct the runtime with the source.
let mut wafer = Wafer::new(cfg_source)?;
wafer.register_block("suppers-ai/auth", Arc::new(AuthBlock::new()))?;
// add_block_config still works for composite/uses config, but it
// no longer feeds the block's lifecycle(Init) payload — init config
// comes from the ConfigSource on first dispatch.
wafer.add_block_config("suppers-ai/auth", auth_composite_json);

// 3. Finalize: composite/uses expansion + capability resolution
//    + snapshot finalization. Replaces start_without_bind() and resolve().
wafer.seal().await?;

// 4. (Optional, for /_health) verify configs without dispatching anything.
let report = wafer.validate_all_block_configs().await;
if !report.broken.is_empty() {
    // log and decide
}

// 5. First dispatch triggers Init on the target block lazily.
let out = wafer
    .run_block("suppers-ai/auth", msg, input)
    .await;
// Init errors surface as OutputStream::error events on first dispatch.
```

## Implementing ConfigSource

```rust
#[async_trait::async_trait]
impl wafer_run::ConfigSource for MyEnvConfigSource {
    async fn load_for_block(
        &self,
        block: &str,
        declared_keys: &[wafer_block::ConfigVar],
    ) -> Result<wafer_run::EnvBlockConfig, wafer_run::ConfigError> {
        let mut out = std::collections::HashMap::new();
        for var in declared_keys {
            if let Ok(v) = std::env::var(&var.key) {
                out.insert(var.key.clone(), v);
            } else if !var.default.is_empty() {
                out.insert(var.key.clone(), var.default.clone());
            } else if !var.optional {
                return Err(wafer_run::ConfigError::MissingRequired {
                    block: block.to_string(),
                    key: var.key.clone(),
                });
            }
        }
        Ok(wafer_run::EnvBlockConfig::new(out))
    }
}
```

## What got faster

- Cold-start CPU. The boot path no longer runs every block's `lifecycle(Init)`.
- D1 query amplification. Cold start reads zero rows from `variables` /
  `block_settings` until the first block actually dispatches. Each block
  loads only its own declared keys.

## What you lose

- Boot-time "broken config" failures. A missing required key no longer
  refuses the deploy; it surfaces as a 5xx on first request to the
  affected block. Mitigate with `/_health` calling
  `validate_all_block_configs()`.
- Eager `lifecycle(Init)` on every block. Native blocks that did
  internal pre-work in Init now defer that to their first message.

## Symbol-level checklist

- [ ] Replace `Wafer::new()` calls with `Wafer::new(your_config_source)`.
- [ ] Replace `Wafer::resolve()` calls with `Wafer::seal()`.
- [ ] Replace `Wafer::start_without_bind()` calls with `Wafer::seal()`.
- [ ] Stop building giant pre-loaded `HashMap<String,String>` of all configs.
- [ ] Implement `ConfigSource` (or use `StaticConfigSource` for tests).
- [ ] If you have a `/_health` route, add a call to `validate_all_block_configs()`.
