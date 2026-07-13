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
