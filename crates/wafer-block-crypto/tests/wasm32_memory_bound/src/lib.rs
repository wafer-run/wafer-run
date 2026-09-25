//! Runtime fixture for the argon2 memory bound on wasm32. See this crate's
//! `Cargo.toml`; `measure.mjs` drives it.

#[cfg(not(target_arch = "wasm32"))]
compile_error!(
    "this fixture only measures wafer-block-crypto on wasm32; \
     build it with --target wasm32-unknown-unknown (scripts/check.sh wasm does)"
);

use wafer_block_crypto::{primitives, service::CryptoError};

/// Verify a password against a stored argon2id hash at costs `m`/`t`/`p`,
/// with a fixed 16-byte salt and 32-byte output. The output is not the
/// password's, so a derivation runs in full and reports a mismatch.
///
/// Returns 0 for a mismatch, 1 for a match, 2 for a refused hash.
#[no_mangle]
pub extern "C" fn verify_at(m: u32, t: u32, p: u32) -> u32 {
    let hash = format!(
        "$argon2id$v=19$m={m},t={t},p={p}\
         $AAECAwQFBgcICQoLDA0ODw$AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8"
    );
    match primitives::verify_password("pw", &hash) {
        Err(CryptoError::PasswordMismatch) => 0,
        Ok(()) => 1,
        Err(_) => 2,
    }
}

/// Allocate `bytes` and never free them: the rest of an application
/// allocating between two logins. A live allocation above a freed
/// derivation buffer stops the allocator from growing that buffer in place,
/// which is what makes a slightly larger request take fresh memory.
#[no_mangle]
pub extern "C" fn pin_allocation(bytes: u32) {
    std::mem::forget(vec![1u8; bytes as usize]);
}
