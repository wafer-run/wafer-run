//! Pure cryptographic primitives shared by every WAFER crypto consumer.
//!
//! Single source of truth for the HS256 JWT stack used across the WAFER
//! ecosystem: base64url encoding, HMAC-SHA256, JWT sign/verify, HKDF
//! per-block key derivation, argon2id password hashing, constant-time
//! comparison, and CSPRNG byte generation.
//!
//! Everything here is pure Rust and wasm32-compatible (`hmac`, `sha2`,
//! `hkdf`, `base64ct`, `argon2`, `subtle`). Randomness goes through the OS
//! RNG (`getrandom` under the hood); on `wasm32-unknown-unknown` the final
//! binary must enable `getrandom`'s `js` feature.
//!
//! # Policy decisions
//!
//! These functions back the native [`Argon2JwtCryptoService`] and are the
//! intended replacement for the per-repo copies that previously lived in the
//! consuming application's native, Cloudflare, and browser crates — each of
//! which had drifted subtly. The unified policy:
//!
//! - **HMAC argument order is `(key, data)`**, matching the textbook
//!   HMAC(K, m) notation. (One historical copy used `(data, key)`.)
//! - **`exp` verification is explicit**: [`jwt_verify`] takes a
//!   [`JwtExpPolicy`] so every call site documents whether tokens without
//!   an `exp` claim are acceptable. Use [`JwtExpPolicy::Required`] unless
//!   there is a documented reason not to — [`jwt_sign`] always stamps
//!   `exp`, so an exp-less token was not produced by this stack and
//!   accepting one creates a forever-valid credential.
//! - **Per-block key derivation** is HKDF-SHA256 (no salt) with info
//!   string `wafer-jwt|{block_id}` and a 32-byte output, lowercase
//!   hex-encoded. Every consumer MUST match this exactly or cross-component
//!   token verification breaks; [`derive_block_key`] carries a pinned
//!   known-answer test guarding the format.
//! - **JWT secrets should be at least [`MIN_JWT_SECRET_LEN`] bytes.** The
//!   primitives themselves accept any key length (HMAC does); enforcing
//!   the minimum is a construction-time policy for service wrappers.
//!
//! [`Argon2JwtCryptoService`]: crate::service::Argon2JwtCryptoService

use std::{collections::HashMap, time::Duration};

use wafer_core::interfaces::crypto::service::CryptoError;

/// Minimum recommended JWT secret length in bytes. HS256 derives from
/// HMAC-SHA256, for which RFC 2104 §3 recommends a key at least as long as
/// the hash output (32 bytes for SHA-256). Shorter keys are accepted by
/// HMAC itself but offer less than the algorithm's nominal 256-bit security
/// and are typically rejected by mature JWT libraries.
pub const MIN_JWT_SECRET_LEN: usize = 32;

/// Standard JWT header for HS256, base64url-encoded (no padding).
/// `{"alg":"HS256","typ":"JWT"}`
const JWT_HEADER_B64: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9";

// ---------------------------------------------------------------------------
// Encoding + comparison
// ---------------------------------------------------------------------------

/// Encode bytes as base64url without padding (as required by JWT RFC 7515).
pub fn b64url_encode(data: &[u8]) -> String {
    use base64ct::{Base64UrlUnpadded, Encoding};
    Base64UrlUnpadded::encode_string(data)
}

/// Decode a base64url (no-padding) string into bytes.
///
/// Errors with [`CryptoError::VerifyError`] — this decoder's primary use is
/// unpacking untrusted JWT segments, where a malformed segment is a
/// verification failure.
pub fn b64url_decode(s: &str) -> Result<Vec<u8>, CryptoError> {
    use base64ct::{Base64UrlUnpadded, Encoding};
    Base64UrlUnpadded::decode_vec(s)
        .map_err(|e| CryptoError::VerifyError(format!("base64 decode: {e}")))
}

/// Compute HMAC-SHA256 over `data` using `key`.
///
/// Argument order is `(key, data)` — HMAC(K, m). Infallible: HMAC accepts
/// keys of any length (RFC 2104 hashes long keys, pads short ones).
pub fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

/// Constant-time equality comparison for secret material (signatures, MACs,
/// shared tokens). Returns `true` when both slices have equal length and
/// content. The length comparison itself is not constant-time — lengths are
/// not considered secret.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    use subtle::ConstantTimeEq;
    a.ct_eq(b).into()
}

/// Generate `n` cryptographically-secure random bytes from the OS RNG.
pub fn random_bytes(n: usize) -> Result<Vec<u8>, CryptoError> {
    use argon2::password_hash::rand_core::{OsRng, RngCore};
    let mut buf = vec![0u8; n];
    OsRng
        .try_fill_bytes(&mut buf)
        .map_err(|e| CryptoError::Other(format!("rng error: {e}")))?;
    Ok(buf)
}

// ---------------------------------------------------------------------------
// HS256 JWT
// ---------------------------------------------------------------------------

/// How [`jwt_verify`] treats the `exp` claim.
///
/// [`jwt_sign`] always stamps `exp`, so tokens produced by this stack
/// always carry one. Prefer [`JwtExpPolicy::Required`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JwtExpPolicy {
    /// Reject tokens whose `exp` claim is missing or non-numeric.
    ///
    /// This is the recommended policy: an exp-less token never expires,
    /// and nothing in the WAFER stack mints such tokens.
    Required,
    /// Accept tokens without an `exp` claim (a present `exp` is still
    /// validated). Only for verifying externally-minted tokens that
    /// legitimately never expire.
    AllowMissing,
}

/// Sign a claims map as a compact HS256 JWT, stamping `iat` (now) and
/// `exp` (now + `expiry`) into the claims before signing.
///
/// Takes `claims` by value to avoid cloning the caller's map just to add
/// the two timestamp claims; callers typically build a fresh map per token.
/// Caller-supplied `iat`/`exp` entries are overwritten.
///
/// Errors when `expiry` overflows the representable range (i64
/// milliseconds) — a misconfigured expiry must not silently produce a
/// token with a different lifetime than the caller asked for.
pub fn jwt_sign(
    mut claims: HashMap<String, serde_json::Value>,
    expiry: Duration,
    secret: &[u8],
) -> Result<String, CryptoError> {
    let now = chrono::Utc::now();
    let chrono_expiry = chrono::Duration::from_std(expiry)
        .map_err(|e| CryptoError::SignError(format!("expiry out of range: {e}")))?;
    let exp = now + chrono_expiry;

    claims.insert("iat".to_string(), serde_json::json!(now.timestamp()));
    claims.insert("exp".to_string(), serde_json::json!(exp.timestamp()));

    jwt_encode_unstamped(&claims, secret)
}

/// Encode and sign claims exactly as given — no `iat`/`exp` stamping.
///
/// Private on purpose: production tokens must carry `exp` (see
/// [`JwtExpPolicy`]). Used by tests to craft tokens with arbitrary claims.
fn jwt_encode_unstamped(
    claims: &HashMap<String, serde_json::Value>,
    secret: &[u8],
) -> Result<String, CryptoError> {
    let payload_json =
        serde_json::to_string(claims).map_err(|e| CryptoError::SignError(e.to_string()))?;
    let payload_b64 = b64url_encode(payload_json.as_bytes());

    let signing_input = format!("{JWT_HEADER_B64}.{payload_b64}");
    let sig = hmac_sha256(secret, signing_input.as_bytes());
    let sig_b64 = b64url_encode(&sig);

    Ok(format!("{signing_input}.{sig_b64}"))
}

/// Verify a compact HS256 JWT and return its claims.
///
/// Validates:
/// - Three-part compact structure
/// - Header is the canonical HS256/JWT header, or decodes to `alg: "HS256"`
/// - Signature is correct (constant-time comparison via HMAC verify)
/// - `exp` claim per `exp_policy`; when present (and numeric) it must not
///   have passed. A non-numeric `exp` counts as missing.
pub fn jwt_verify(
    token: &str,
    secret: &[u8],
    exp_policy: JwtExpPolicy,
) -> Result<HashMap<String, serde_json::Value>, CryptoError> {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    let parts: Vec<&str> = token.split('.').collect();
    let [header_b64, payload_b64, sig_b64] = parts.as_slice() else {
        return Err(CryptoError::VerifyError(
            "invalid JWT structure".to_string(),
        ));
    };

    // Verify header (we only support HS256 tokens produced by this stack).
    if *header_b64 != JWT_HEADER_B64 {
        // Fall back: decode and check the alg field to allow minor JSON
        // formatting variants (key order, whitespace).
        let header_bytes = b64url_decode(header_b64)?;
        let header: serde_json::Value = serde_json::from_slice(&header_bytes)
            .map_err(|e| CryptoError::VerifyError(format!("header decode: {e}")))?;
        // A missing or non-string `alg` field is a malformed JWT — reject
        // explicitly rather than collapsing both cases into a generic
        // "unsupported algorithm" message.
        let alg = header
            .get("alg")
            .ok_or_else(|| CryptoError::VerifyError("missing `alg` field in JWT header".into()))?
            .as_str()
            .ok_or_else(|| {
                CryptoError::VerifyError("`alg` field in JWT header is not a string".into())
            })?;
        if alg != "HS256" {
            return Err(CryptoError::VerifyError(format!(
                "unsupported algorithm: {alg}"
            )));
        }
    }

    // Verify signature (constant-time via hmac::Mac::verify_slice).
    let signing_input = format!("{header_b64}.{payload_b64}");
    let sig_bytes = b64url_decode(sig_b64)?;
    let mut mac =
        <Hmac<Sha256> as Mac>::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(signing_input.as_bytes());
    mac.verify_slice(&sig_bytes)
        .map_err(|_| CryptoError::VerifyError("signature mismatch".to_string()))?;

    // Decode payload.
    let payload_bytes = b64url_decode(payload_b64)?;
    let claims: HashMap<String, serde_json::Value> = serde_json::from_slice(&payload_bytes)
        .map_err(|e| CryptoError::VerifyError(format!("payload decode: {e}")))?;

    // Validate expiry per policy.
    let now = chrono::Utc::now().timestamp();
    match (claims.get("exp").and_then(|v| v.as_i64()), exp_policy) {
        (Some(exp), _) if now > exp => {
            return Err(CryptoError::VerifyError("token expired".to_string()));
        }
        (Some(_), _) | (None, JwtExpPolicy::AllowMissing) => {}
        (None, JwtExpPolicy::Required) => {
            return Err(CryptoError::VerifyError(
                "token missing exp claim".to_string(),
            ));
        }
    }

    Ok(claims)
}

// ---------------------------------------------------------------------------
// Per-block key derivation
// ---------------------------------------------------------------------------

/// Derive a per-block JWT signing key from the master secret using
/// HKDF-SHA256 (no salt), info string `wafer-jwt|{block_id}`, 32-byte
/// output, lowercase hex-encoded (64 chars — always ≥ [`MIN_JWT_SECRET_LEN`]).
///
/// This derivation is a cross-component contract: tokens signed with a
/// key derived here by one component (e.g. the native runtime) must verify
/// against the key derived by another (e.g. a browser/Workers deployment).
/// Do not change the info-string format or output encoding — the pinned
/// known-answer test below exists to catch exactly that.
pub fn derive_block_key(master_secret: &[u8], block_id: &str) -> String {
    use hkdf::Hkdf;
    use sha2::Sha256;

    let hk = Hkdf::<Sha256>::new(None, master_secret);
    let info = format!("wafer-jwt|{block_id}");
    let mut okm = [0u8; 32];
    // `Hkdf::expand` only fails when the requested output exceeds
    // `255 * HashLen` bytes (8160 for SHA-256). Our `okm` is fixed at
    // 32 bytes, so this branch is unreachable. The `expect` documents
    // the invariant rather than handling an actual failure mode.
    hk.expand(info.as_bytes(), &mut okm)
        .expect("32-byte HKDF-SHA256 output is well within the 8160-byte max");
    okm.iter().map(|b| format!("{b:02x}")).collect()
}

// ---------------------------------------------------------------------------
// Password hashing (argon2id)
// ---------------------------------------------------------------------------

/// Argon2id cost preset for [`hash_password`].
///
/// Cost parameters are baked into the produced PHC string, so
/// [`verify_password`] handles hashes of either preset (and any other
/// argon2 parameters) transparently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Argon2Cost {
    /// `argon2` crate defaults (currently 19 MiB memory, 2 iterations,
    /// 1 lane) — for native deployments.
    Default,
    /// Low-cost parameters (4 MiB memory, 2 iterations, 1 lane) for
    /// CPU/memory-constrained environments such as Cloudflare Workers,
    /// where the default memory cost exceeds runtime limits.
    Constrained,
}

/// Hash a password with argon2id at the given cost, producing a PHC-format
/// string (`$argon2id$...`) with a random 16-byte salt.
pub fn hash_password(password: &str, cost: Argon2Cost) -> Result<String, CryptoError> {
    use argon2::{
        password_hash::{rand_core::OsRng, SaltString},
        Argon2, PasswordHasher,
    };
    let argon2 = match cost {
        Argon2Cost::Default => Argon2::default(),
        Argon2Cost::Constrained => {
            let params = argon2::Params::new(4096, 2, 1, None)
                .map_err(|e| CryptoError::HashError(format!("argon2 params: {e}")))?;
            Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params)
        }
    };
    let salt = SaltString::generate(&mut OsRng);
    argon2
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| CryptoError::HashError(e.to_string()))
}

/// Verify a password against a PHC-format argon2 hash (any cost — the
/// parameters are read from the hash string itself).
///
/// Returns [`CryptoError::PasswordMismatch`] when the password is wrong and
/// [`CryptoError::HashError`] when the hash string is malformed.
pub fn verify_password(password: &str, hash: &str) -> Result<(), CryptoError> {
    use argon2::{password_hash::PasswordHash, Argon2, PasswordVerifier};
    let parsed = PasswordHash::new(hash).map_err(|e| CryptoError::HashError(e.to_string()))?;
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .map_err(|_| CryptoError::PasswordMismatch)
}

// ---------------------------------------------------------------------------
// Password hashing (PBKDF2-HMAC-SHA256)
// ---------------------------------------------------------------------------

/// PHC-style scheme identifier this crate writes and recognises for
/// PBKDF2-HMAC-SHA256 hashes.
const PBKDF2_SHA256_ID: &str = "pbkdf2-sha256";

/// Salt length in bytes for [`pbkdf2_hash`]. 128 bits, per NIST SP 800-132
/// §5.1 (which sets 128 bits as the recommendation and 128 bits as the
/// practical floor for password storage).
const PBKDF2_SALT_LEN: usize = 16;

/// Derived-key length in bytes for [`pbkdf2_hash`] — the full SHA-256 output.
const PBKDF2_DK_LEN: usize = 32;

/// Recommended PBKDF2-HMAC-SHA256 iteration count (OWASP Password Storage
/// Cheat Sheet, 2023).
///
/// This is the value to pass unless you have measured a reason not to.
/// It runs in roughly a second of single-threaded wasm, which is acceptable
/// for a login or password change — the only operations that hash a
/// password — and is not acceptable per request.
pub const PBKDF2_SHA256_RECOMMENDED_ITERATIONS: u32 = 600_000;

/// Lowest iteration count [`pbkdf2_hash`] will *write*. NIST SP 800-132 §5.2
/// gives 10,000 as the floor for password storage.
///
/// [`pbkdf2_verify`] does **not** enforce this: the cost of a stored hash is
/// a fact about when it was written, and refusing to check an existing
/// credential because its cost is now considered low locks the user out of
/// their own account instead of protecting them. Raise the floor for new
/// hashes; re-hash on next successful login if you want old ones upgraded.
pub const PBKDF2_SHA256_MIN_ITERATIONS: u32 = 10_000;

/// Hash a password with PBKDF2-HMAC-SHA256 at `iterations`, producing a
/// PHC-style string with a fresh random 16-byte salt.
///
/// # Format
///
/// `$pbkdf2-sha256$i={iterations}${salt}${derived_key}`, where both fields
/// are **standard** base64 (RFC 4648 §4, `+/` alphabet) **with** padding —
/// not the URL-safe unpadded encoding [`b64url_encode`] uses for JWTs. The
/// derived key is 32 bytes.
///
/// This layout is a **persisted credential format**: hashes written by this
/// function are stored and read back months later, by this code and by other
/// implementations of the same scheme. It is pinned by a known-answer test.
/// Do not change the alphabet, the padding, the field order, the `i=` prefix
/// or the lengths without a migration — every stored credential decodes
/// through it.
///
/// Errors when `iterations` is below [`PBKDF2_SHA256_MIN_ITERATIONS`].
pub fn pbkdf2_hash(password: &str, iterations: u32) -> Result<String, CryptoError> {
    use base64ct::{Base64, Encoding};

    if iterations < PBKDF2_SHA256_MIN_ITERATIONS {
        return Err(CryptoError::HashError(format!(
            "PBKDF2 iterations must be at least {PBKDF2_SHA256_MIN_ITERATIONS} \
             (NIST SP 800-132 §5.2); got {iterations}"
        )));
    }

    let salt = random_bytes(PBKDF2_SALT_LEN)?;
    let derived = pbkdf2_derive(password, &salt, iterations, PBKDF2_DK_LEN)
        .map_err(CryptoError::HashError)?;

    Ok(format!(
        "${PBKDF2_SHA256_ID}$i={iterations}${}${}",
        Base64::encode_string(&salt),
        Base64::encode_string(&derived)
    ))
}

/// Verify a password against a hash produced by [`pbkdf2_hash`].
///
/// The iteration count, salt and derived-key length all come from the stored
/// string, so a hash written under an older (or newer) cost still verifies.
/// The comparison is constant-time.
///
/// Returns [`CryptoError::PasswordMismatch`] when the password is wrong and
/// [`CryptoError::VerifyError`] when the string is not a well-formed
/// `pbkdf2-sha256` hash — including when it is some *other* scheme's hash,
/// which this function cannot check. Use [`verify_password_any_scheme`] when
/// the stored hash may be of either scheme this crate supports.
pub fn pbkdf2_verify(password: &str, hash: &str) -> Result<(), CryptoError> {
    use base64ct::{Base64, Encoding};

    // `$pbkdf2-sha256$i=N$salt$dk` splits into a leading empty field plus
    // four populated ones.
    let parts: Vec<&str> = hash.split('$').collect();
    if parts.len() != 5 || !parts[0].is_empty() || parts[1] != PBKDF2_SHA256_ID {
        return Err(CryptoError::VerifyError(format!(
            "not a {PBKDF2_SHA256_ID} hash"
        )));
    }

    let iterations: u32 = parts[2]
        .strip_prefix("i=")
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| CryptoError::VerifyError("invalid iteration count".to_string()))?;

    let salt = Base64::decode_vec(parts[3])
        .map_err(|e| CryptoError::VerifyError(format!("invalid salt: {e}")))?;
    if salt.is_empty() {
        return Err(CryptoError::VerifyError("empty salt".to_string()));
    }
    let expected = Base64::decode_vec(parts[4])
        .map_err(|e| CryptoError::VerifyError(format!("invalid hash: {e}")))?;

    // The derived-key length is fixed rather than taken from the stored
    // string. PBKDF2 with a shorter `dkLen` returns a PREFIX of the longer
    // output, so deriving `expected.len()` bytes would let a truncated
    // stored hash verify against the same password — an 8-byte "hash" would
    // be checked at 64 bits. Nothing this crate writes is anything but
    // `PBKDF2_DK_LEN`, so anything else is malformed.
    if expected.len() != PBKDF2_DK_LEN {
        return Err(CryptoError::VerifyError(format!(
            "derived key must be {PBKDF2_DK_LEN} bytes, got {}",
            expected.len()
        )));
    }

    let computed = pbkdf2_derive(password, &salt, iterations, PBKDF2_DK_LEN)
        .map_err(CryptoError::VerifyError)?;

    if constant_time_eq(&computed, &expected) {
        Ok(())
    } else {
        Err(CryptoError::PasswordMismatch)
    }
}

/// The derivation itself, shared by hash and verify so the two cannot use
/// different parameters. `Err` carries a message for the caller to wrap in
/// whichever `CryptoError` variant fits its direction.
fn pbkdf2_derive(
    password: &str,
    salt: &[u8],
    iterations: u32,
    dk_len: usize,
) -> Result<Vec<u8>, String> {
    use hmac::Hmac;
    use sha2::Sha256;

    // The `pbkdf2` crate treats a zero round count as a valid no-op
    // derivation rather than an error, which would make the "hash" the
    // first HMAC block of the password. Refuse it here so neither direction
    // can be talked into it by a malformed stored string.
    if iterations == 0 {
        return Err("PBKDF2 iteration count must be non-zero".to_string());
    }
    if dk_len == 0 {
        return Err("PBKDF2 derived key must be non-empty".to_string());
    }

    let mut out = vec![0u8; dk_len];
    pbkdf2::pbkdf2::<Hmac<Sha256>>(password.as_bytes(), salt, iterations, &mut out)
        .map_err(|e| format!("pbkdf2: {e}"))?;
    Ok(out)
}

// ---------------------------------------------------------------------------
// Password scheme selection
// ---------------------------------------------------------------------------

/// Which algorithm a service uses when it **writes** a new password hash.
///
/// Verification never consults this — see [`verify_password_any_scheme`].
/// The choice is a deployment property, not a security level: both schemes
/// here are accepted password-storage algorithms, and the reason to pick one
/// is the runtime it has to run in. Argon2id is the better function and the
/// default; PBKDF2 exists because argon2id's memory cost is unaffordable in
/// single-threaded wasm, where the default parameters take minutes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasswordScheme {
    /// argon2id at the given cost preset. The default.
    Argon2(Argon2Cost),
    /// PBKDF2-HMAC-SHA256 at `iterations`. Pass
    /// [`PBKDF2_SHA256_RECOMMENDED_ITERATIONS`] unless you have measured a
    /// reason not to; below [`PBKDF2_SHA256_MIN_ITERATIONS`] hashing fails.
    Pbkdf2Sha256 {
        /// PBKDF2 iteration count baked into each hash this scheme writes.
        iterations: u32,
    },
}

impl Default for PasswordScheme {
    fn default() -> Self {
        Self::Argon2(Argon2Cost::Default)
    }
}

/// Hash `password` under `scheme`, producing that scheme's PHC-style string.
pub fn hash_password_with(password: &str, scheme: PasswordScheme) -> Result<String, CryptoError> {
    match scheme {
        PasswordScheme::Argon2(cost) => hash_password(password, cost),
        PasswordScheme::Pbkdf2Sha256 { iterations } => pbkdf2_hash(password, iterations),
    }
}

/// Verify `password` against a stored hash of **either** supported scheme,
/// dispatching on the scheme identifier the hash string carries.
///
/// Verification is driven by the stored hash and not by any configured
/// scheme on purpose. A stored hash is a fact from whenever it was written;
/// a configured scheme is a fact about now. Checking the credential against
/// the current setting instead of against itself would mean that changing the
/// setting — or running the same database on two targets that hash
/// differently, which is exactly why [`PasswordScheme`] exists — invalidates
/// every credential already stored. Both schemes are accepted algorithms, so
/// nothing is weakened by recognising both.
///
/// A string that names no scheme this crate knows is a
/// [`CryptoError`], never an accept.
pub fn verify_password_any_scheme(password: &str, hash: &str) -> Result<(), CryptoError> {
    // The scheme identifier is the first field of a PHC string
    // (`$<id>$<params>$<salt>$<hash>`), so it is what follows the leading
    // `$`. Dispatching on it explicitly — rather than handing an unknown
    // string to one verifier and trusting it to refuse — is what makes
    // "never an accept" checkable, and it keeps an unrecognised scheme
    // distinguishable from a wrong password: `verify_password` maps every
    // argon2 parse failure to `PasswordMismatch`, so a stored hash written
    // by some third scheme would otherwise be reported forever as the user
    // typing the wrong password.
    match hash
        .strip_prefix('$')
        .and_then(|rest| rest.split('$').next())
    {
        Some(PBKDF2_SHA256_ID) => pbkdf2_verify(password, hash),
        Some(id) if id.starts_with("argon2") => verify_password(password, hash),
        _ => Err(CryptoError::VerifyError(
            "unrecognised password hash scheme".to_string(),
        )),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &[u8] = b"test-secret-padded-to-32-bytes-or-more-for-validation";

    fn claims_with_sub(sub: &str) -> HashMap<String, serde_json::Value> {
        let mut m = HashMap::new();
        m.insert("sub".to_string(), serde_json::json!(sub));
        m
    }

    // -- base64url --

    #[test]
    fn b64url_roundtrip() {
        let data = b"hello \xff\x00 world";
        let encoded = b64url_encode(data);
        assert!(!encoded.contains('='), "must be unpadded");
        assert_eq!(b64url_decode(&encoded).unwrap(), data);
    }

    #[test]
    fn b64url_decode_rejects_invalid_input() {
        assert!(b64url_decode("not!!valid@@base64").is_err());
        // Standard-alphabet padding is invalid in the unpadded url-safe form.
        assert!(b64url_decode("aGVsbG8=").is_err());
    }

    // -- HMAC --

    #[test]
    fn hmac_sha256_matches_rfc4231_test_case_2() {
        // RFC 4231 §4.3 test case 2: key "Jefe", data "what do ya want
        // for nothing?".
        let mac = hmac_sha256(b"Jefe", b"what do ya want for nothing?");
        let hex: String = mac.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex,
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn hmac_sha256_is_deterministic_and_key_sensitive() {
        let a = hmac_sha256(b"key", b"hello");
        let b = hmac_sha256(b"key", b"hello");
        let c = hmac_sha256(b"different-key", b"hello");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    // -- constant-time comparison --

    #[test]
    fn constant_time_eq_works() {
        assert!(constant_time_eq(b"hello", b"hello"));
        assert!(constant_time_eq(b"", b""));
        assert!(!constant_time_eq(b"hello", b"world"));
        assert!(!constant_time_eq(b"hello", b"hell"));
        assert!(!constant_time_eq(b"", b"x"));
    }

    // -- random bytes --

    #[test]
    fn random_bytes_returns_correct_length_and_varies() {
        let a = random_bytes(32).unwrap();
        let b = random_bytes(32).unwrap();
        assert_eq!(a.len(), 32);
        assert_ne!(a, b, "two draws must differ");
        assert!(random_bytes(0).unwrap().is_empty());
    }

    // -- JWT --

    #[test]
    fn jwt_sign_and_verify_roundtrip() {
        let mut claims = claims_with_sub("user-123");
        claims.insert("email".to_string(), serde_json::json!("test@example.com"));

        let token = jwt_sign(claims, Duration::from_secs(3600), SECRET).unwrap();
        assert_eq!(token.split('.').count(), 3);

        let verified = jwt_verify(&token, SECRET, JwtExpPolicy::Required).unwrap();
        assert_eq!(verified["sub"], "user-123");
        assert_eq!(verified["email"], "test@example.com");
        assert!(verified.contains_key("iat"), "sign must stamp iat");
        assert!(verified.contains_key("exp"), "sign must stamp exp");
    }

    #[test]
    fn jwt_sign_overwrites_caller_supplied_exp() {
        let mut claims = claims_with_sub("u1");
        claims.insert("exp".to_string(), serde_json::json!(1));
        let token = jwt_sign(claims, Duration::from_secs(3600), SECRET).unwrap();
        // Were the caller's exp kept, the token would be long expired.
        let verified = jwt_verify(&token, SECRET, JwtExpPolicy::Required).unwrap();
        assert!(verified["exp"].as_i64().unwrap() > chrono::Utc::now().timestamp());
    }

    #[test]
    fn jwt_sign_rejects_unrepresentable_expiry() {
        // std::Duration::MAX is well over the i64-milliseconds range chrono
        // can represent — must surface as a SignError, not silently cap.
        let err = jwt_sign(claims_with_sub("u1"), Duration::MAX, SECRET)
            .expect_err("MAX expiry should error");
        match err {
            CryptoError::SignError(msg) => assert!(
                msg.contains("expiry out of range"),
                "expected expiry-range error, got: {msg}"
            ),
            other => panic!("expected SignError, got: {other:?}"),
        }
    }

    #[test]
    fn jwt_verify_rejects_wrong_secret() {
        let token = jwt_sign(claims_with_sub("u1"), Duration::from_secs(3600), SECRET).unwrap();
        let err = jwt_verify(&token, b"another-secret", JwtExpPolicy::Required)
            .expect_err("wrong secret must fail");
        assert!(matches!(err, CryptoError::VerifyError(_)));
    }

    #[test]
    fn jwt_verify_rejects_tampered_payload() {
        let token = jwt_sign(claims_with_sub("u1"), Duration::from_secs(3600), SECRET).unwrap();
        let parts: Vec<&str> = token.split('.').collect();
        let tampered_payload = b64url_encode(br#"{"sub":"admin","exp":9999999999}"#);
        let tampered = format!("{}.{tampered_payload}.{}", parts[0], parts[2]);
        assert!(jwt_verify(&tampered, SECRET, JwtExpPolicy::Required).is_err());
    }

    #[test]
    fn jwt_verify_rejects_malformed_tokens() {
        for bad in ["", "only-one-part", "two.parts", "a.b.c.d"] {
            assert!(
                jwt_verify(bad, SECRET, JwtExpPolicy::AllowMissing).is_err(),
                "must reject: {bad:?}"
            );
        }
    }

    #[test]
    fn jwt_verify_rejects_expired_token() {
        let past = chrono::Utc::now().timestamp() - 60;
        let mut claims = claims_with_sub("u1");
        claims.insert("exp".to_string(), serde_json::json!(past));
        let token = jwt_encode_unstamped(&claims, SECRET).unwrap();

        for policy in [JwtExpPolicy::Required, JwtExpPolicy::AllowMissing] {
            let err = jwt_verify(&token, SECRET, policy).expect_err("expired token must fail");
            match err {
                CryptoError::VerifyError(msg) => assert!(
                    msg.contains("expired"),
                    "expected expiry error under {policy:?}, got: {msg}"
                ),
                other => panic!("expected VerifyError, got: {other:?}"),
            }
        }
    }

    #[test]
    fn jwt_verify_exp_policy_governs_missing_exp() {
        let token = jwt_encode_unstamped(&claims_with_sub("u1"), SECRET).unwrap();

        let err = jwt_verify(&token, SECRET, JwtExpPolicy::Required)
            .expect_err("Required must reject exp-less token");
        match err {
            CryptoError::VerifyError(msg) => assert!(
                msg.contains("missing exp"),
                "expected missing-exp error, got: {msg}"
            ),
            other => panic!("expected VerifyError, got: {other:?}"),
        }

        let claims = jwt_verify(&token, SECRET, JwtExpPolicy::AllowMissing)
            .expect("AllowMissing must accept exp-less token");
        assert_eq!(claims["sub"], "u1");
    }

    #[test]
    fn jwt_verify_required_treats_non_numeric_exp_as_missing() {
        let mut claims = claims_with_sub("u1");
        claims.insert("exp".to_string(), serde_json::json!("not-a-number"));
        let token = jwt_encode_unstamped(&claims, SECRET).unwrap();
        assert!(jwt_verify(&token, SECRET, JwtExpPolicy::Required).is_err());
    }

    /// Header variants: a re-ordered (but valid HS256) header must verify;
    /// missing / non-string / non-HS256 `alg` must be rejected with specific
    /// errors.
    #[test]
    fn jwt_verify_header_alg_handling() {
        fn token_with_header(header_json: &str) -> String {
            let header_b64 = b64url_encode(header_json.as_bytes());
            let payload_b64 = b64url_encode(br#"{"sub":"u1"}"#);
            let signing_input = format!("{header_b64}.{payload_b64}");
            let sig = hmac_sha256(SECRET, signing_input.as_bytes());
            format!("{signing_input}.{}", b64url_encode(&sig))
        }

        // Key order differs from the canonical header — still HS256, accepted.
        let reordered = token_with_header(r#"{"typ":"JWT","alg":"HS256"}"#);
        assert!(jwt_verify(&reordered, SECRET, JwtExpPolicy::AllowMissing).is_ok());

        let missing = token_with_header(r#"{"typ":"JWT"}"#);
        let err = jwt_verify(&missing, SECRET, JwtExpPolicy::AllowMissing)
            .expect_err("missing alg must fail");
        assert!(
            err.to_string().contains("missing `alg`"),
            "expected missing-alg error, got: {err}"
        );

        let non_string = token_with_header(r#"{"alg":42,"typ":"JWT"}"#);
        let err = jwt_verify(&non_string, SECRET, JwtExpPolicy::AllowMissing)
            .expect_err("non-string alg must fail");
        assert!(
            err.to_string().contains("not a string"),
            "expected non-string-alg error, got: {err}"
        );

        let wrong_alg = token_with_header(r#"{"alg":"none","typ":"JWT"}"#);
        let err = jwt_verify(&wrong_alg, SECRET, JwtExpPolicy::AllowMissing)
            .expect_err("alg=none must fail");
        assert!(
            err.to_string().contains("unsupported algorithm: none"),
            "expected unsupported-alg error, got: {err}"
        );
    }

    // -- per-block key derivation --

    /// Pinned known-answer vector: HKDF-SHA256(salt=∅, ikm="test-master-secret",
    /// info="wafer-jwt|my-org/auth", 32 bytes) hex-encoded.
    ///
    /// This is a CROSS-REPO CONTRACT — every consuming application component
    /// (native, Cloudflare, and browser builds) derives verification keys with
    /// the same inputs and must produce this exact value. If this test breaks,
    /// token verification between components breaks. Do not update the expected
    /// value to make the test pass; fix the derivation instead.
    #[test]
    fn derive_block_key_known_answer() {
        assert_eq!(
            derive_block_key(b"test-master-secret", "my-org/auth"),
            "dd87bf9e0e9b5fd74c6aab9cacf016fdf64e084731630d2a37cb813badac3757"
        );
    }

    #[test]
    fn derive_block_key_is_deterministic_and_block_scoped() {
        let a1 = derive_block_key(b"master", "my-org/auth");
        let a2 = derive_block_key(b"master", "my-org/auth");
        let b = derive_block_key(b"master", "my-org/admin");
        assert_eq!(a1, a2);
        assert_ne!(a1, b);
        assert_eq!(a1.len(), 64, "32-byte key hex-encodes to 64 chars");
        assert!(a1.chars().all(|c| c.is_ascii_hexdigit()));
    }

    // -- password hashing --

    #[test]
    fn hash_and_verify_password_default_cost() {
        let hash = hash_password("correcthorsebatterystaple", Argon2Cost::Default).unwrap();
        assert!(hash.starts_with("$argon2id$"));
        verify_password("correcthorsebatterystaple", &hash).unwrap();
        assert!(matches!(
            verify_password("wrongpassword", &hash),
            Err(CryptoError::PasswordMismatch)
        ));
    }

    #[test]
    fn hash_and_verify_password_constrained_cost() {
        let hash = hash_password("hunter2hunter2", Argon2Cost::Constrained).unwrap();
        assert!(
            hash.contains("m=4096,t=2,p=1"),
            "constrained cost params must be encoded in the PHC string: {hash}"
        );
        // verify_password reads the parameters from the hash itself, so a
        // constrained hash verifies without knowing the cost up front.
        verify_password("hunter2hunter2", &hash).unwrap();
        assert!(matches!(
            verify_password("wrong", &hash),
            Err(CryptoError::PasswordMismatch)
        ));
    }

    #[test]
    fn verify_password_rejects_garbage_hash() {
        assert!(matches!(
            verify_password("anything", "not-a-hash"),
            Err(CryptoError::HashError(_))
        ));
        assert!(matches!(
            verify_password("anything", ""),
            Err(CryptoError::HashError(_))
        ));
    }
}

#[cfg(test)]
mod pbkdf2_tests {
    use super::*;

    /// A PBKDF2-HMAC-SHA256 known-answer vector, computed by an independent
    /// implementation (Python's `hashlib.pbkdf2_hmac`) over a fixed salt, and
    /// formatted the way [`pbkdf2_hash`] formats one.
    ///
    /// This is the compatibility anchor for the whole scheme. Password hashes
    /// are stored, not recomputed, so the encoding here — the
    /// `$pbkdf2-sha256$i=N$salt$hash` layout, **standard** (not URL-safe)
    /// base64 *with* padding, a 16-byte salt and a 32-byte derived key — is a
    /// persisted format, and a credential written by an existing deployment
    /// must keep verifying. Changing any of it silently locks users out.
    ///
    /// Password: `correcthorsebatterystaple`; salt: bytes `00..0f`.
    const KAT_HASH: &str =
        "$pbkdf2-sha256$i=1000$AAECAwQFBgcICQoLDA0ODw==$/6tPyT3P0FDTAPcc3qfsdyi1rxNk5iabYJNHAbMJ8Mg=";
    const KAT_PASSWORD: &str = "correcthorsebatterystaple";

    #[test]
    fn verifies_a_hash_from_an_independent_implementation() {
        pbkdf2_verify(KAT_PASSWORD, KAT_HASH).expect("known-answer vector must verify");
    }

    #[test]
    fn rejects_the_wrong_password_for_the_known_answer_vector() {
        assert!(matches!(
            pbkdf2_verify("wrong", KAT_HASH),
            Err(CryptoError::PasswordMismatch)
        ));
    }

    #[test]
    fn hash_round_trips_and_has_the_documented_shape() {
        let hash = pbkdf2_hash("hunter2hunter2", 10_000).expect("hash");
        let parts: Vec<&str> = hash.split('$').collect();
        assert_eq!(parts.len(), 5, "PHC layout: {hash}");
        assert_eq!(parts[0], "");
        assert_eq!(parts[1], "pbkdf2-sha256");
        assert_eq!(parts[2], "i=10000");

        use base64ct::{Base64, Encoding};
        assert_eq!(Base64::decode_vec(parts[3]).expect("salt b64").len(), 16);
        assert_eq!(Base64::decode_vec(parts[4]).expect("hash b64").len(), 32);

        pbkdf2_verify("hunter2hunter2", &hash).expect("round trip");
        assert!(matches!(
            pbkdf2_verify("wrong", &hash),
            Err(CryptoError::PasswordMismatch)
        ));
    }

    #[test]
    fn each_hash_uses_a_fresh_salt() {
        let a = pbkdf2_hash("same", 10_000).expect("hash a");
        let b = pbkdf2_hash("same", 10_000).expect("hash b");
        assert_ne!(a, b, "a reused salt would make hashes rainbow-tableable");
    }

    /// The iteration count is read from the stored hash, not from the
    /// caller's current policy, so raising the recommended count does not
    /// invalidate credentials already on disk.
    #[test]
    fn verification_uses_the_iteration_count_in_the_hash() {
        let low = pbkdf2_hash("pw", 10_000).expect("hash");
        assert!(low.contains("i=10000"));
        pbkdf2_verify("pw", &low).expect("an old, cheaper hash still verifies");
    }

    /// A weak iteration count is refused when **writing** a new hash…
    #[test]
    fn hashing_below_the_floor_is_refused() {
        let err = pbkdf2_hash("pw", PBKDF2_SHA256_MIN_ITERATIONS - 1)
            .expect_err("below the floor must fail");
        assert!(
            matches!(&err, CryptoError::HashError(m) if m.contains("iterations")),
            "got {err:?}"
        );
    }

    /// …but never when **reading** one. Refusing to verify an existing
    /// credential because its stored cost is now considered low would lock
    /// the user out of their own account; the floor is a policy for new
    /// hashes only.
    #[test]
    fn verifying_below_the_floor_is_allowed() {
        // `KAT_HASH` is i=1000, below the floor.
        const { assert!(1000 < PBKDF2_SHA256_MIN_ITERATIONS) };
        pbkdf2_verify(KAT_PASSWORD, KAT_HASH).expect("an under-cost stored hash still verifies");
    }

    #[test]
    fn malformed_hashes_are_verify_errors_not_panics() {
        for bad in [
            "",
            "not-a-hash",
            "$argon2id$v=19$m=4096,t=2,p=1$c2FsdA$aGFzaA",
            "$pbkdf2-sha256$i=1000$AAECAwQFBgcICQoLDA0ODw==",
            "$pbkdf2-sha512$i=1000$AAECAwQFBgcICQoLDA0ODw==$/6tPyT3P0FDTAPcc3qfsdyi1rxNk5iabYJNHAbMJ8Mg=",
            "$pbkdf2-sha256$rounds=1000$AAECAwQFBgcICQoLDA0ODw==$/6tPyT3P0FDTAPcc3qfsdyi1rxNk5iabYJNHAbMJ8Mg=",
            "$pbkdf2-sha256$i=notanumber$AAECAwQFBgcICQoLDA0ODw==$/6tPyT3P0FDTAPcc3qfsdyi1rxNk5iabYJNHAbMJ8Mg=",
            "$pbkdf2-sha256$i=1000$!!!not-base64!!!$/6tPyT3P0FDTAPcc3qfsdyi1rxNk5iabYJNHAbMJ8Mg=",
            "$pbkdf2-sha256$i=1000$AAECAwQFBgcICQoLDA0ODw==$!!!not-base64!!!",
        ] {
            match pbkdf2_verify(KAT_PASSWORD, bad) {
                Err(CryptoError::VerifyError(_)) => {}
                other => panic!("expected VerifyError for {bad:?}, got {other:?}"),
            }
        }
    }

    /// PBKDF2 with a shorter `dkLen` returns a *prefix* of the longer
    /// output, so a stored hash truncated to 16 bytes would verify against
    /// the right password at half the strength if the derived length were
    /// read from the stored string. It is fixed instead, and a hash of any
    /// other length is malformed.
    #[test]
    fn a_truncated_derived_key_does_not_verify() {
        let truncated = "$pbkdf2-sha256$i=1000$AAECAwQFBgcICQoLDA0ODw==$/6tPyT3P0FDTAPcc3qfsdw==";
        match pbkdf2_verify(KAT_PASSWORD, truncated) {
            Err(CryptoError::VerifyError(m)) => {
                assert!(m.contains("derived key"), "got {m}");
            }
            other => panic!("a prefix of the real derived key must be refused, got {other:?}"),
        }
    }

    #[test]
    fn an_empty_salt_does_not_verify() {
        let no_salt = "$pbkdf2-sha256$i=1000$$/6tPyT3P0FDTAPcc3qfsdyi1rxNk5iabYJNHAbMJ8Mg=";
        assert!(matches!(
            pbkdf2_verify(KAT_PASSWORD, no_salt),
            Err(CryptoError::VerifyError(_))
        ));
    }
}

#[cfg(test)]
mod scheme_dispatch_tests {
    use super::*;

    const KAT_HASH: &str =
        "$pbkdf2-sha256$i=1000$AAECAwQFBgcICQoLDA0ODw==$/6tPyT3P0FDTAPcc3qfsdyi1rxNk5iabYJNHAbMJ8Mg=";
    const KAT_PASSWORD: &str = "correcthorsebatterystaple";

    /// Verification is driven by the **stored hash's** format, not by the
    /// scheme a service is currently configured to write. A deployment that
    /// switches schemes keeps every credential it already had.
    #[test]
    fn accepts_both_schemes() {
        let argon2 = hash_password("pw-a", Argon2Cost::Constrained).expect("argon2 hash");
        verify_password_any_scheme("pw-a", &argon2).expect("argon2 accepted");
        verify_password_any_scheme(KAT_PASSWORD, KAT_HASH).expect("pbkdf2 accepted");
    }

    #[test]
    fn rejects_the_wrong_password_under_either_scheme() {
        let argon2 = hash_password("pw-a", Argon2Cost::Constrained).expect("argon2 hash");
        assert!(matches!(
            verify_password_any_scheme("nope", &argon2),
            Err(CryptoError::PasswordMismatch)
        ));
        assert!(matches!(
            verify_password_any_scheme("nope", KAT_HASH),
            Err(CryptoError::PasswordMismatch)
        ));
    }

    /// An unrecognised or malformed stored hash is an error, never an
    /// accept.
    #[test]
    fn refuses_an_unrecognised_hash() {
        for bad in [
            "",
            "not-a-hash",
            "$scrypt$ln=16,r=8,p=1$c2FsdA$aGFzaA",
            "$bcrypt$v=2b$c2FsdA$aGFzaA",
        ] {
            match verify_password_any_scheme("anything", bad) {
                // An unknown scheme is a broken stored credential, not a
                // wrong password: an operator reading `PasswordMismatch`
                // here would chase the user instead of the database.
                Err(CryptoError::VerifyError(m)) => {
                    assert!(m.contains("unrecognised"), "got {m} for {bad:?}");
                }
                other => panic!("must refuse {bad:?} as unrecognised, got {other:?}"),
            }
        }
    }

    /// The default scheme is unchanged from before schemes existed: argon2id
    /// at the crate's default cost.
    #[test]
    fn the_default_scheme_is_argon2_at_default_cost() {
        assert_eq!(
            PasswordScheme::default(),
            PasswordScheme::Argon2(Argon2Cost::Default)
        );
    }
}
