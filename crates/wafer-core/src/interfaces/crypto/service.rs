use std::{collections::HashMap, time::Duration};

use thiserror::Error;

/// Errors returned by [`CryptoService`] operations.
#[derive(Error, Debug)]
pub enum CryptoError {
    /// Failure while computing a password hash.
    #[error("hash error: {0}")]
    HashError(String),
    /// `compare_hash` rejected the password.
    #[error("password mismatch")]
    PasswordMismatch,
    /// `compare_hash` could not check the password: the stored hash is
    /// malformed, names an unsupported scheme, or carries cost parameters
    /// outside the accepted range. A fault in the stored credential, not a
    /// wrong password.
    #[error("malformed password hash: {0}")]
    MalformedHash(String),
    /// Failure while issuing / signing a token.
    #[error("sign error: {0}")]
    SignError(String),
    /// Failure while verifying / decoding a token.
    #[error("verify error: {0}")]
    VerifyError(String),
    /// Catch-all variant carrying an arbitrary backend message.
    #[error("{0}")]
    Other(String),
}

/// Service provides cryptographic operations.
///
/// Tokens are always signed and verified under a key derived for the
/// calling block, so one block cannot mint or accept another's tokens. The
/// trait deliberately has no master-key `sign`/`verify`: an implementation
/// must provide [`sign_for`](Self::sign_for) and
/// [`verify_for`](Self::verify_for) itself.
///
/// ```compile_fail,E0046
/// use std::{collections::HashMap, time::Duration};
/// use wafer_core::interfaces::crypto::service::{CryptoError, CryptoService};
///
/// struct NoSignFor;
///
/// impl CryptoService for NoSignFor {
///     fn hash(&self, _: &str) -> Result<String, CryptoError> { unimplemented!() }
///     fn compare_hash(&self, _: &str, _: &str) -> Result<(), CryptoError> { unimplemented!() }
///     // `sign_for` omitted: there is no default to fall back on.
///     fn verify_for(
///         &self,
///         _: &str,
///         _: &str,
///     ) -> Result<HashMap<String, serde_json::Value>, CryptoError> {
///         unimplemented!()
///     }
///     fn random_bytes(&self, _: usize) -> Result<Vec<u8>, CryptoError> { unimplemented!() }
/// }
/// ```
pub trait CryptoService: wafer_block::MaybeSend + wafer_block::MaybeSync {
    /// Hash produces a one-way hash of a password.
    fn hash(&self, password: &str) -> Result<String, CryptoError>;

    /// CompareHash checks a password against a stored hash:
    /// [`CryptoError::PasswordMismatch`] for a wrong password,
    /// [`CryptoError::MalformedHash`] when the stored hash cannot be checked.
    fn compare_hash(&self, password: &str, hash: &str) -> Result<(), CryptoError>;

    /// Create a signed token from claims with the given expiry, using the
    /// key derived for `block_id`.
    fn sign_for(
        &self,
        block_id: &str,
        claims: HashMap<String, serde_json::Value>,
        expiry: Duration,
    ) -> Result<String, CryptoError>;

    /// Validate a token under the key derived for `block_id` and return its
    /// claims.
    fn verify_for(
        &self,
        block_id: &str,
        token: &str,
    ) -> Result<HashMap<String, serde_json::Value>, CryptoError>;

    /// RandomBytes generates n cryptographically-secure random bytes.
    fn random_bytes(&self, n: usize) -> Result<Vec<u8>, CryptoError>;
}
