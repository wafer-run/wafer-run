use crate::runtime::Wafer;
use crate::services::Services;
use super::blocks::BlockRegistry;
use super::config::WaferConfig;
use super::providers::ProviderRegistry;

/// Bootstrap creates a WAFER runtime and platform services from a config.
pub fn bootstrap(
    cfg: &WaferConfig,
    provider_registry: &ProviderRegistry,
) -> Result<(Wafer, Services), String> {
    let svc = provider_registry.create_services(&cfg.services)?;
    let mut w = Wafer::new();
    w.register_platform_services(Services {
        database: svc.database.clone(),
        storage: svc.storage.clone(),
        logger: svc.logger.clone(),
        crypto: svc.crypto.clone(),
        config: svc.config.clone(),
        network: svc.network.clone(),
    });
    Ok((w, svc))
}

/// BootstrapFull creates a WAFER runtime, platform services, and auto-registers blocks.
pub fn bootstrap_full(
    cfg: &WaferConfig,
    provider_registry: &ProviderRegistry,
    block_registry: &BlockRegistry,
) -> Result<(Wafer, Services, Vec<String>), String> {
    let svc = provider_registry.create_services(&cfg.services)?;

    let mut w = Wafer::new();
    w.register_platform_services(Services {
        database: svc.database.clone(),
        storage: svc.storage.clone(),
        logger: svc.logger.clone(),
        crypto: svc.crypto.clone(),
        config: svc.config.clone(),
        network: svc.network.clone(),
    });

    // Auto-register blocks from config
    let (registered, unresolved) = block_registry.create_blocks(&cfg.blocks)?;
    for (name, block) in registered {
        w.register_block(name, block);
    }

    Ok((w, svc, unresolved))
}
