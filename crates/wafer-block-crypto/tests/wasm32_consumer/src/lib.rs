//! Compile fixture for `wafer-block-crypto` on `wasm32-unknown-unknown`.
//! See this crate's `Cargo.toml` for why it exists and how it is built.
//!
//! Uses the crate the way a Worker or browser embedder does: the password
//! (plain and peppered), JWT and constant-time primitives directly, and
//! `Argon2JwtCryptoService` behind `dyn CryptoService`. Naming them here makes
//! the build generate code for them on wasm32, not only typecheck the library.
//!
//! Nothing in here ever runs: `cargo build` is the whole test.

#[cfg(not(target_arch = "wasm32"))]
compile_error!(
    "this fixture only exercises wafer-block-crypto on wasm32; \
     build it with --target wasm32-unknown-unknown (scripts/check.sh wasm does)"
);

use std::{collections::BTreeMap, time::Duration};

use wafer_block_crypto::{
    primitives::{self, Argon2Cost},
    service::{Argon2JwtCryptoService, CryptoError, CryptoService},
};

/// The service as an embedder registers it: a trait object.
pub fn crypto_service(jwt_secret: String) -> Result<Box<dyn CryptoService>, CryptoError> {
    Ok(Box::new(Argon2JwtCryptoService::new(jwt_secret)?))
}

/// Hash, verify, sign and compare through the primitives, as the embedders'
/// own crypto adapters do.
pub fn primitives_round_trip(password: &str, secret: &[u8]) -> Result<bool, CryptoError> {
    let hash = primitives::hash_password(password, Argon2Cost::Constrained)?;
    let peppers = primitives::PasswordPeppers::new(
        Some(primitives::PepperKey::new(secret)?),
        Vec::new(),
        false,
    )?;
    primitives::verify_password_any_scheme(password, &hash, &peppers)?;
    let peppered = primitives::hash_password_peppered(
        password,
        Argon2Cost::Constrained,
        peppers.current().expect("configured above"),
    )?;
    primitives::verify_password_any_scheme(password, &peppered, &peppers)?;
    let token = primitives::jwt_sign(BTreeMap::new(), Duration::from_secs(60), secret)?;
    let nonce = primitives::random_bytes(16)?;
    Ok(primitives::constant_time_eq(token.as_bytes(), &nonce))
}
