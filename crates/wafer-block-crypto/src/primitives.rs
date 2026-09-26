//! Pure cryptographic primitives shared by every WAFER crypto consumer.
//!
//! Single source of truth for the HS256 JWT stack used across the WAFER
//! ecosystem: base64url encoding, HMAC-SHA256, JWT sign/verify, HKDF
//! per-block key derivation, argon2id password hashing and its pepper,
//! constant-time comparison, and CSPRNG byte generation.
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
//! - **The JWT payload is canonical JSON**: object keys in sorted order at
//!   every depth, no insignificant whitespace. The payload is therefore a
//!   function of the claim set as signed (the caller's claims with `iat`
//!   and `exp` stamped over them): whatever order the caller built them
//!   in, equal claim sets encode to equal bytes and different claim sets
//!   to different bytes. See [`jwt_sign`] for when two whole tokens match.
//! - **JWT secrets should be at least [`MIN_JWT_SECRET_LEN`] bytes.** The
//!   primitives themselves accept any key length (HMAC does); enforcing
//!   the minimum is a construction-time policy for service wrappers.
//!
//! [`Argon2JwtCryptoService`]: crate::service::Argon2JwtCryptoService

use std::{collections::BTreeMap, time::Duration};

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
    use hmac::{Hmac, KeyInit, Mac};
    use sha2::Sha256;
    let mut mac =
        <Hmac<Sha256> as KeyInit>::new_from_slice(key).expect("HMAC accepts any key length");
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
    let mut buf = vec![0u8; n];
    getrandom::fill(&mut buf).map_err(|e| CryptoError::Other(format!("rng error: {e}")))?;
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
/// The payload is canonical: keys sorted at every depth. Two calls in the
/// same second with equal claims, the same `expiry` and the same `secret`
/// (for a per-block service, the same calling block's derived key) return
/// byte-identical tokens. Claims that differ only in `iat`/`exp` count as
/// equal, since both are overwritten. A caller that needs two tokens to
/// differ must put something that differs in the claims (a random `jti`),
/// never rely on the encoding to vary.
///
/// Errors when `now + expiry` is not a representable date (chrono's
/// calendar ends in year 262143) — a misconfigured expiry must neither
/// panic nor silently produce a token with a different lifetime than the
/// caller asked for.
pub fn jwt_sign(
    mut claims: BTreeMap<String, serde_json::Value>,
    expiry: Duration,
    secret: &[u8],
) -> Result<String, CryptoError> {
    let now = chrono::Utc::now();
    let chrono_expiry = chrono::Duration::from_std(expiry)
        .map_err(|e| CryptoError::SignError(format!("expiry out of range: {e}")))?;
    let exp = now
        .checked_add_signed(chrono_expiry)
        .ok_or_else(|| CryptoError::SignError("expiry out of range: past the last date".into()))?;

    claims.insert("iat".to_string(), serde_json::json!(now.timestamp()));
    claims.insert("exp".to_string(), serde_json::json!(exp.timestamp()));

    jwt_encode_unstamped(claims, secret)
}

/// Encode claims as [`canonical_json`] and sign them — no `iat`/`exp`
/// stamping.
///
/// Private on purpose: production tokens must carry `exp` (see
/// [`JwtExpPolicy`]). Used by tests to craft tokens with arbitrary claims.
fn jwt_encode_unstamped(
    claims: BTreeMap<String, serde_json::Value>,
    secret: &[u8],
) -> Result<String, CryptoError> {
    let payload_json = canonical_json(claims)?;
    let payload_b64 = b64url_encode(payload_json.as_bytes());

    let signing_input = format!("{JWT_HEADER_B64}.{payload_b64}");
    let sig = hmac_sha256(secret, signing_input.as_bytes());
    let sig_b64 = b64url_encode(&sig);

    Ok(format!("{signing_input}.{sig_b64}"))
}

/// Serialize `claims` as canonical JSON: keys sorted at every depth.
///
/// The `BTreeMap` sorts the top level. Nested objects are
/// `serde_json::Map`s, which keep insertion order instead of sorting when
/// any crate in the final build enables serde_json's `preserve_order`
/// feature; [`serde_json::Value::sort_all_objects`] sorts them in that
/// build and does nothing in the default one.
fn canonical_json(mut claims: BTreeMap<String, serde_json::Value>) -> Result<String, CryptoError> {
    claims
        .values_mut()
        .for_each(serde_json::Value::sort_all_objects);
    serde_json::to_string(&claims).map_err(|e| CryptoError::SignError(e.to_string()))
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
) -> Result<BTreeMap<String, serde_json::Value>, CryptoError> {
    use hmac::{Hmac, KeyInit, Mac};
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
        <Hmac<Sha256> as KeyInit>::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(signing_input.as_bytes());
    mac.verify_slice(&sig_bytes)
        .map_err(|_| CryptoError::VerifyError("signature mismatch".to_string()))?;

    // Decode payload.
    let payload_bytes = b64url_decode(payload_b64)?;
    let claims: BTreeMap<String, serde_json::Value> = serde_json::from_slice(&payload_bytes)
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
/// argon2 parameters up to [`ARGON2_MAX_M_COST`] and its siblings)
/// transparently. The presets are the only argon2 costs this crate writes,
/// and a compile-time assertion holds each under the verify ceilings, so it
/// never writes a hash it would refuse to check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Argon2Cost {
    /// `argon2` crate defaults: 19 MiB memory, 2 iterations, 1 lane —
    /// OWASP's recommended argon2id configuration. For native deployments.
    Default,
    /// Low-cost parameters (4 MiB memory, 2 iterations, 1 lane) for runtimes
    /// with a tight per-request CPU budget, such as Cloudflare Workers.
    /// Below OWASP's minimum argon2id configuration (7 MiB at 5 iterations,
    /// or an equivalent trade), and about a fifth of [`Self::Default`]'s
    /// work. Memory is not the constraint: [`Self::Default`] grows wasm32
    /// linear memory by 19 MiB, far inside a Worker isolate's 128 MB.
    Constrained,
}

impl Argon2Cost {
    /// `(m_cost KiB, t_cost, p_cost)` of the preset.
    const fn costs(self) -> (u32, u32, u32) {
        match self {
            Self::Default => (
                argon2::Params::DEFAULT_M_COST,
                argon2::Params::DEFAULT_T_COST,
                argon2::Params::DEFAULT_P_COST,
            ),
            Self::Constrained => (4096, 2, 1),
        }
    }
}

// Every preset verifies on every target: none exceeds a ceiling.
const _: () = {
    let presets = [Argon2Cost::Default, Argon2Cost::Constrained];
    let mut i = 0;
    while i < presets.len() {
        let (m, t, p) = presets[i].costs();
        assert!(m <= ARGON2_MAX_M_COST && t <= ARGON2_MAX_T_COST && p <= ARGON2_MAX_P_COST);
        i += 1;
    }
    // `Argon2Memory::derive` takes the first class that holds a derivation,
    // which is the smallest only while the list ascends.
    let mut c = 1;
    while c < ARGON2_MEMORY_CLASSES.len() {
        assert!(ARGON2_MEMORY_CLASSES[c - 1] < ARGON2_MEMORY_CLASSES[c]);
        c += 1;
    }
};

/// Hash a password with argon2id at the given cost, producing a PHC-format
/// string (`$argon2id$...`) with a random 16-byte salt. Not peppered; see
/// [`hash_password_peppered`].
pub fn hash_password(password: &str, cost: Argon2Cost) -> Result<String, CryptoError> {
    use argon2::password_hash::phc::{Output, ParamsString, PasswordHash, Salt};

    let salt = random_bytes(ARGON2_SALT_LEN)?;
    let derived = argon2_derive_new(password, cost, &salt)?;
    let phc = PasswordHash {
        algorithm: argon2::Algorithm::Argon2id.ident(),
        version: Some(argon2::Version::V0x13.into()),
        params: ParamsString::try_from(&derived.params).map_err(|e| argon2_hash_error(&e))?,
        salt: Some(Salt::new(&salt).map_err(|e| argon2_hash_error(&e))?),
        hash: Some(Output::new(&*derived.output).map_err(|e| argon2_hash_error(&e))?),
    };
    Ok(phc.to_string())
}

fn argon2_hash_error(e: &dyn core::fmt::Display) -> CryptoError {
    CryptoError::HashError(format!("argon2: {e}"))
}

/// An argon2id output and the parameters that produced it.
struct Argon2Derived {
    params: argon2::Params,
    output: zeroize::Zeroizing<[u8; argon2::Params::DEFAULT_OUTPUT_LEN]>,
}

/// The argon2id derivation behind every hash this crate writes: `cost`'s
/// parameters, version 0x13, a [`argon2::Params::DEFAULT_OUTPUT_LEN`]-byte
/// output.
fn argon2_derive_new(
    password: &str,
    cost: Argon2Cost,
    salt: &[u8],
) -> Result<Argon2Derived, CryptoError> {
    use argon2::{Algorithm, Argon2, Version};

    let (m, t, p) = cost.costs();
    let params = argon2::Params::new(m, t, p, None).map_err(|e| argon2_hash_error(&e))?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params.clone());
    let mut output = zeroize::Zeroizing::new([0u8; argon2::Params::DEFAULT_OUTPUT_LEN]);
    with_argon2_memory(|memory| memory.derive(&argon2, password.as_bytes(), salt, &mut *output))
        .map_err(|e| argon2_hash_error(&e))?;
    Ok(Argon2Derived { params, output })
}

/// Salt length in bytes for [`hash_password`]: the PHC string format's
/// recommended 16.
const ARGON2_SALT_LEN: usize = 16;

/// Highest argon2 memory cost (KiB) [`verify_password`] will run: 46 MiB,
/// the largest memory cost in OWASP's argon2id recommendations (46 MiB at
/// 1 iteration). The same on every target, so a stored hash verifies on
/// all of them or on none.
///
/// The cost parameters of a stored hash come from the stored string, and
/// the `argon2` crate accepts up to `u32::MAX` for each — one crafted hash
/// could allocate terabytes or pin a thread for hours. Verification refuses
/// anything above these ceilings as [`CryptoError::MalformedHash`]. There is
/// no floor: an old, cheap hash still verifies (see
/// [`PBKDF2_SHA256_MIN_ITERATIONS`] for why).
///
/// The memory ceiling is sized for a Cloudflare Workers isolate, whose
/// 128 MB limit also holds the compiled module, the JS heap and the rest of
/// the application. Every derivation runs in a buffer of one of the
/// [`ARGON2_MEMORY_CLASSES`], so the argon2 share of wasm32 linear memory is
/// at most their sum, 69 MiB (69.25 MiB measured with allocator overhead),
/// whatever sequence of stored hashes an isolate verifies. Hashes written
/// elsewhere with more memory — RFC 9106's 64 MiB option, which is
/// argon2-cffi's default — are refused; re-hash them within the ceiling
/// before importing them.
pub const ARGON2_MAX_M_COST: u32 = 46 * 1024;

/// Highest argon2 time cost (passes) [`verify_password`] will run: ten
/// times [`Argon2Cost::Default`]'s; see [`ARGON2_MAX_M_COST`].
pub const ARGON2_MAX_T_COST: u32 = 10 * argon2::Params::DEFAULT_T_COST;

/// Highest argon2 parallelism (lanes) [`verify_password`] will run: ten
/// times [`Argon2Cost::Default`]'s; see [`ARGON2_MAX_M_COST`].
pub const ARGON2_MAX_P_COST: u32 = 10 * argon2::Params::DEFAULT_P_COST;

/// Sizes (in 1 KiB argon2 blocks) of the working memory argon2 derivations
/// run in: [`Argon2Cost::Constrained`]'s, [`Argon2Cost::Default`]'s and
/// [`ARGON2_MAX_M_COST`]. A derivation takes the smallest that holds it.
///
/// On wasm32 each is allocated once per instance and kept. Linear memory
/// never shrinks, and an allocator handed requests of arbitrary sizes may
/// not reuse a freed block for a slightly larger one, so two stored hashes
/// just under the ceiling could otherwise grow memory by twice the ceiling.
/// Kept buffers of fixed sizes cap the growth at the sum of this list.
pub const ARGON2_MEMORY_CLASSES: [u32; 3] = [
    Argon2Cost::Constrained.costs().0,
    Argon2Cost::Default.costs().0,
    ARGON2_MAX_M_COST,
];

/// The working-memory buffers for argon2 derivations, one slot per entry of
/// [`ARGON2_MEMORY_CLASSES`], each allocated on first use.
struct Argon2Memory {
    classes: [Option<Vec<argon2::Block>>; ARGON2_MEMORY_CLASSES.len()],
}

impl Argon2Memory {
    const fn new() -> Self {
        Self {
            classes: [None, None, None],
        }
    }

    /// Run `argon2` over the smallest class buffer that holds its
    /// `block_count`, allocating that buffer if this is its first use. The
    /// derivation reads only the blocks it writes first, so a reused buffer
    /// gives the same output as fresh memory. The blocks it wrote are
    /// zeroised afterwards, whether it succeeded or not: on wasm32 the buffer
    /// lives as long as the instance, and would otherwise keep the last
    /// derivation's state, which is a function of the password.
    fn derive(
        &mut self,
        argon2: &argon2::Argon2<'_>,
        password: &[u8],
        salt: &[u8],
        out: &mut [u8],
    ) -> Result<(), argon2::Error> {
        let blocks = argon2.params().block_count();
        let class = ARGON2_MEMORY_CLASSES
            .iter()
            .position(|&size| blocks <= size as usize)
            .ok_or(argon2::Error::MemoryTooMuch)?;
        let buffer = self.classes[class].get_or_insert_with(|| {
            vec![argon2::Block::default(); ARGON2_MEMORY_CLASSES[class] as usize]
        });
        let result =
            argon2.hash_password_into_with_memory(password, salt, out, buffer.as_mut_slice());
        for block in buffer.iter_mut().take(blocks) {
            zeroize::Zeroize::zeroize(block.as_mut());
        }
        result
    }
}

/// Give `f` the argon2 working memory. On wasm32 it is one instance per
/// thread, kept for the life of the module (see [`ARGON2_MEMORY_CLASSES`]).
/// Elsewhere memory is returned to the system, and a thread pool would keep
/// a set per thread, so each call gets its own, freed on return.
fn with_argon2_memory<R>(f: impl FnOnce(&mut Argon2Memory) -> R) -> R {
    #[cfg(target_arch = "wasm32")]
    {
        thread_local! {
            static MEMORY: core::cell::RefCell<Argon2Memory> =
                const { core::cell::RefCell::new(Argon2Memory::new()) };
        }
        MEMORY.with(|memory| f(&mut memory.borrow_mut()))
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        f(&mut Argon2Memory::new())
    }
}

/// Verify a password against a PHC-format argon2 hash. The parameters are
/// read from the hash string itself, so any cost up to the
/// [`ARGON2_MAX_M_COST`] / [`ARGON2_MAX_T_COST`] / [`ARGON2_MAX_P_COST`]
/// ceilings verifies.
///
/// Returns [`CryptoError::PasswordMismatch`] when the password is wrong and
/// [`CryptoError::MalformedHash`] when the hash string is malformed, lacks
/// a salt or an output, or carries parameters the `argon2` crate or the
/// ceilings refuse.
pub fn verify_password(password: &str, hash: &str) -> Result<(), CryptoError> {
    use argon2::{password_hash::phc::PasswordHash, Algorithm, Argon2, Version};
    let malformed =
        |what: &dyn core::fmt::Display| CryptoError::MalformedHash(format!("argon2: {what}"));

    let parsed = PasswordHash::new(hash).map_err(|e| malformed(&e))?;
    let (Some(salt), Some(expected)) = (&parsed.salt, &parsed.hash) else {
        return Err(malformed(&"hash has no salt or no output"));
    };
    let params = argon2::Params::try_from(&parsed).map_err(|e| malformed(&e))?;
    check_argon2_ceilings(&params).map_err(|e| malformed(&e))?;
    let algorithm = Algorithm::try_from(parsed.algorithm.as_str()).map_err(|e| malformed(&e))?;
    // A PHC string without `v=` is version 0x13, as in the `argon2` crate's
    // own verifier.
    let version = parsed
        .version
        .map(Version::try_from)
        .transpose()
        .map_err(|e| malformed(&e))?
        .unwrap_or_default();

    let argon2 = Argon2::new(algorithm, version, params);
    let mut computed = vec![0u8; expected.len()];
    with_argon2_memory(|memory| {
        memory.derive(&argon2, password.as_bytes(), salt.as_ref(), &mut computed)
    })
    .map_err(|e| malformed(&e))?;

    if constant_time_eq(&computed, expected.as_bytes()) {
        Ok(())
    } else {
        Err(CryptoError::PasswordMismatch)
    }
}

/// Refuse stored argon2 parameters above the verify ceilings
/// ([`ARGON2_MAX_M_COST`] and its siblings). `Err` carries the reason.
fn check_argon2_ceilings(params: &argon2::Params) -> Result<(), String> {
    for (name, value, max) in [
        ("memory cost m", params.m_cost(), ARGON2_MAX_M_COST),
        ("time cost t", params.t_cost(), ARGON2_MAX_T_COST),
        ("parallelism p", params.p_cost(), ARGON2_MAX_P_COST),
    ] {
        if value > max {
            return Err(format!(
                "{name}={value} exceeds the ceiling of {max} this runtime will run"
            ));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Password pepper (argon2id, then HMAC-SHA-256)
// ---------------------------------------------------------------------------

/// PHC identifier of a peppered argon2id hash.
///
/// # Format
///
/// `$argon2id-hmac-sha256$v=19$m=<m>,t=<t>,p=<p>,pepper=<key id>$<salt>$<mac>`
///
/// - `m`, `t`, `p`, `v` and the salt mean what they mean in a plain
///   `$argon2id$` string: the argon2id parameters, version 0x13 and the
///   16-byte salt, in the PHC format's unpadded standard base64.
/// - `pepper` names the key the hash was peppered with: the
///   [`PepperKey::id`] of that key, 16 lowercase hex characters.
/// - `<mac>` is `HMAC-SHA-256(pepper key, argon2id output)`, where the
///   argon2id output is 32 bytes — the post-hashing pepper of the OWASP
///   Password Storage Cheat Sheet. 32 bytes, unpadded standard base64.
///
/// A distinct identifier, rather than an extra parameter on `$argon2id$`,
/// keeps the two formats from ever being read as each other: a verifier that
/// knows only plain argon2id refuses a peppered hash as an unknown scheme
/// instead of checking it without its pepper.
///
/// This is a **persisted credential format**, pinned by a known-answer
/// test. Changing any part of it strands every stored peppered hash.
pub const ARGON2ID_PEPPERED_ID: &str = "argon2id-hmac-sha256";

/// The PHC parameter of an [`ARGON2ID_PEPPERED_ID`] hash that names its
/// pepper key.
const PEPPER_ID_PARAM: &str = "pepper";

/// Shortest pepper key accepted, in bytes: HMAC-SHA-256's output length, the
/// key size at which HMAC gives its full strength (RFC 2104 §3). The key is
/// meant to be random bytes, e.g. `openssl rand -base64 32`.
pub const PASSWORD_PEPPER_MIN_LEN: usize = 32;

/// Bytes of key fingerprint in a [`PepperKey::id`].
const PEPPER_ID_LEN: usize = 8;

/// The message a key's fingerprint is the HMAC of. It can never equal an
/// argon2id output (32 bytes), the only other message a pepper key MACs.
const PEPPER_ID_LABEL: &[u8] = b"wafer-run password pepper id";

/// One password pepper key and the id stored hashes name it by.
///
/// The id is derived from the key — the first 8 bytes of
/// `HMAC-SHA-256(key, "wafer-run password pepper id")`, hex — rather than
/// numbered by hand, so a key can never be replaced under an id that stored
/// hashes already name: a new key is a new id, and a hash naming the old one
/// fails as a missing key ([`CryptoError::Pepper`]) instead of as a wrong
/// password. It reveals nothing useful about a random key of
/// [`PASSWORD_PEPPER_MIN_LEN`] bytes or more.
pub struct PepperKey {
    id: String,
    key: zeroize::Zeroizing<Vec<u8>>,
}

impl PepperKey {
    /// A key from raw bytes; at least [`PASSWORD_PEPPER_MIN_LEN`] of them.
    pub fn new(key: &[u8]) -> Result<Self, CryptoError> {
        if key.len() < PASSWORD_PEPPER_MIN_LEN {
            return Err(CryptoError::Pepper(format!(
                "a pepper key must be at least {PASSWORD_PEPPER_MIN_LEN} bytes; got {}",
                key.len()
            )));
        }
        let fingerprint = hmac_sha256(key, PEPPER_ID_LABEL);
        let id = fingerprint[..PEPPER_ID_LEN]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        Ok(Self {
            id,
            key: zeroize::Zeroizing::new(key.to_vec()),
        })
    }

    /// A key from standard, padded base64 (RFC 4648 §4) — what
    /// `openssl rand -base64 32` prints. The error never contains the input.
    pub fn from_base64(encoded: &str) -> Result<Self, CryptoError> {
        use base64ct::{Base64, Encoding};
        let bytes = zeroize::Zeroizing::new(Base64::decode_vec(encoded.trim()).map_err(|_| {
            CryptoError::Pepper("a pepper key must be standard padded base64".to_string())
        })?);
        Self::new(&bytes)
    }

    /// The id peppered hashes name this key by.
    pub fn id(&self) -> &str {
        &self.id
    }
}

impl core::fmt::Debug for PepperKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PepperKey")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

/// The pepper keys a service holds and whether it requires a pepper.
///
/// - **No keys** (the [`Default`]): new hashes are not peppered; stored
///   hashes of every scheme verify, and a peppered one fails with
///   [`CryptoError::Pepper`] because its key is missing.
/// - **A current key**: new argon2id hashes are peppered with it. Stored
///   hashes verify with the key they name — the current one or one of the
///   previous ones — and stored unpeppered hashes still verify, so turning a
///   pepper on locks nobody out.
/// - **Required**: as with a current key (one must be set), but a stored
///   hash without a pepper is refused with [`CryptoError::Pepper`]. Turn it
///   on once every stored hash is peppered: until then an attacker who can
///   write the credential table could plant an unpeppered hash of a password
///   they know.
///
/// Rotation: make the new key current and move the old one to the previous
/// keys. New hashes use the new key; a stored hash moves to it only when it
/// is rewritten (a password change, or a re-hash on login). Drop an old key
/// once no stored hash names it.
#[derive(Debug, Default)]
pub struct PasswordPeppers {
    current: Option<PepperKey>,
    previous: Vec<PepperKey>,
    required: bool,
}

impl PasswordPeppers {
    /// Assemble a set. Fails when `required` is set without a `current`
    /// key, when `previous` is non-empty without a `current` key (new hashes
    /// would silently stop being peppered), or when a key appears twice.
    pub fn new(
        current: Option<PepperKey>,
        previous: Vec<PepperKey>,
        required: bool,
    ) -> Result<Self, CryptoError> {
        if current.is_none() {
            if required {
                return Err(CryptoError::Pepper(
                    "a pepper is required but no current pepper key is configured".to_string(),
                ));
            }
            if !previous.is_empty() {
                return Err(CryptoError::Pepper(
                    "previous pepper keys are configured without a current key; new hashes \
                     would not be peppered"
                        .to_string(),
                ));
            }
        }
        let mut ids: Vec<&str> = current
            .iter()
            .chain(previous.iter())
            .map(PepperKey::id)
            .collect();
        ids.sort_unstable();
        if let Some(dup) = ids.windows(2).find(|w| w[0] == w[1]) {
            return Err(CryptoError::Pepper(format!(
                "pepper key {} is configured more than once",
                dup[0]
            )));
        }
        Ok(Self {
            current,
            previous,
            required,
        })
    }

    /// Build a set from configuration text: `current` one base64 key,
    /// `previous` base64 keys separated by commas, each as
    /// [`PepperKey::from_base64`] reads it. `None` or blank is unset.
    pub fn from_config(
        current: Option<&str>,
        previous: Option<&str>,
        required: bool,
    ) -> Result<Self, CryptoError> {
        let in_setting = |which: String| {
            move |e: CryptoError| match e {
                CryptoError::Pepper(msg) => CryptoError::Pepper(format!("{which}: {msg}")),
                other => other,
            }
        };
        let current = current
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| PepperKey::from_base64(s).map_err(in_setting("current key".into())))
            .transpose()?;
        let previous = match previous.map(str::trim).filter(|s| !s.is_empty()) {
            None => Vec::new(),
            Some(list) => list
                .split(',')
                .enumerate()
                .map(|(i, entry)| {
                    let which = format!("previous key {}", i + 1);
                    if entry.trim().is_empty() {
                        return Err(CryptoError::Pepper(format!("{which}: empty entry")));
                    }
                    PepperKey::from_base64(entry).map_err(in_setting(which))
                })
                .collect::<Result<_, _>>()?,
        };
        Self::new(current, previous, required)
    }

    /// The key new hashes are peppered with, if any.
    pub fn current(&self) -> Option<&PepperKey> {
        self.current.as_ref()
    }

    /// The previous keys, which verify and never hash.
    pub fn previous(&self) -> &[PepperKey] {
        &self.previous
    }

    /// Whether a stored hash without a pepper is refused.
    pub fn is_required(&self) -> bool {
        self.required
    }

    fn key(&self, id: &str) -> Option<&PepperKey> {
        self.current
            .iter()
            .chain(self.previous.iter())
            .find(|k| k.id == id)
    }
}

/// Hash a password with argon2id at `cost` and pepper it with `pepper`,
/// producing an [`ARGON2ID_PEPPERED_ID`] string with a random 16-byte salt.
pub fn hash_password_peppered(
    password: &str,
    cost: Argon2Cost,
    pepper: &PepperKey,
) -> Result<String, CryptoError> {
    let salt = random_bytes(ARGON2_SALT_LEN)?;
    hash_password_peppered_with_salt(password, cost, pepper, &salt)
}

fn hash_password_peppered_with_salt(
    password: &str,
    cost: Argon2Cost,
    pepper: &PepperKey,
    salt: &[u8],
) -> Result<String, CryptoError> {
    use argon2::password_hash::phc::{Ident, Output, ParamsString, PasswordHash, Salt};

    let derived = argon2_derive_new(password, cost, salt)?;
    let mac = hmac_sha256(&pepper.key, &*derived.output);
    let mut params = ParamsString::new();
    for (name, value) in [
        ("m", derived.params.m_cost()),
        ("t", derived.params.t_cost()),
        ("p", derived.params.p_cost()),
    ] {
        params
            .add_decimal(name, value)
            .map_err(|e| argon2_hash_error(&e))?;
    }
    params
        .add_str(PEPPER_ID_PARAM, pepper.id())
        .map_err(|e| argon2_hash_error(&e))?;
    let phc = PasswordHash {
        algorithm: Ident::new(ARGON2ID_PEPPERED_ID).map_err(|e| argon2_hash_error(&e))?,
        version: Some(argon2::Version::V0x13.into()),
        params,
        salt: Some(Salt::new(salt).map_err(|e| argon2_hash_error(&e))?),
        hash: Some(Output::new(&mac).map_err(|e| argon2_hash_error(&e))?),
    };
    Ok(phc.to_string())
}

/// Verify a password against an [`ARGON2ID_PEPPERED_ID`] hash, with the key
/// in `peppers` the hash names.
///
/// Returns [`CryptoError::PasswordMismatch`] only when the password is wrong.
/// A hash naming a key `peppers` does not hold is [`CryptoError::Pepper`],
/// decided before any argon2 work; a string that is not a well-formed
/// peppered hash, or whose costs exceed the verify ceilings
/// ([`ARGON2_MAX_M_COST`] and its siblings), is
/// [`CryptoError::MalformedHash`]. The MAC comparison is constant-time
/// ([`constant_time_eq`]).
pub fn verify_password_peppered(
    password: &str,
    hash: &str,
    peppers: &PasswordPeppers,
) -> Result<(), CryptoError> {
    use argon2::{password_hash::phc::PasswordHash, Algorithm, Argon2, Version};
    let malformed = |what: &dyn core::fmt::Display| {
        CryptoError::MalformedHash(format!("{ARGON2ID_PEPPERED_ID}: {what}"))
    };

    let parsed = PasswordHash::new(hash).map_err(|e| malformed(&e))?;
    if parsed.algorithm.as_str() != ARGON2ID_PEPPERED_ID {
        return Err(malformed(&"not a peppered argon2id hash"));
    }
    if parsed.version != Some(Version::V0x13.into()) {
        return Err(malformed(&"version must be v=19"));
    }
    let mut m = None;
    let mut t = None;
    let mut p = None;
    let mut key_id = None;
    for (name, value) in parsed.params.iter() {
        let slot_taken = match name.as_str() {
            "m" => m
                .replace(value.decimal().map_err(|e| malformed(&e))?)
                .is_some(),
            "t" => t
                .replace(value.decimal().map_err(|e| malformed(&e))?)
                .is_some(),
            "p" => p
                .replace(value.decimal().map_err(|e| malformed(&e))?)
                .is_some(),
            PEPPER_ID_PARAM => key_id.replace(value.as_str()).is_some(),
            other => return Err(malformed(&format!("unknown parameter {other}"))),
        };
        if slot_taken {
            return Err(malformed(&format!("parameter {} repeated", name.as_str())));
        }
    }
    let (Some(m), Some(t), Some(p), Some(key_id)) = (m, t, p, key_id) else {
        return Err(malformed(&"parameters m, t, p and pepper are all required"));
    };
    let (Some(salt), Some(expected)) = (&parsed.salt, &parsed.hash) else {
        return Err(malformed(&"hash has no salt or no output"));
    };
    if expected.len() != argon2::Params::DEFAULT_OUTPUT_LEN {
        return Err(malformed(&format!(
            "the MAC must be {} bytes, got {}",
            argon2::Params::DEFAULT_OUTPUT_LEN,
            expected.len()
        )));
    }
    let Some(pepper) = peppers.key(key_id) else {
        return Err(CryptoError::Pepper(format!(
            "the stored hash is peppered with key {key_id}, which is not configured"
        )));
    };

    let params = argon2::Params::new(m, t, p, Some(argon2::Params::DEFAULT_OUTPUT_LEN))
        .map_err(|e| malformed(&e))?;
    check_argon2_ceilings(&params).map_err(|e| malformed(&e))?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut derived = zeroize::Zeroizing::new([0u8; argon2::Params::DEFAULT_OUTPUT_LEN]);
    with_argon2_memory(|memory| {
        memory.derive(&argon2, password.as_bytes(), salt.as_ref(), &mut *derived)
    })
    .map_err(|e| malformed(&e))?;

    let mac = hmac_sha256(&pepper.key, &*derived);
    if constant_time_eq(&mac, expected.as_bytes()) {
        Ok(())
    } else {
        Err(CryptoError::PasswordMismatch)
    }
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
/// It runs in about 180 ms of wasm32 under V8 (measured), which is
/// acceptable for a login or password change — the only operations that
/// hash a password — and is not acceptable per request.
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

/// Highest iteration count [`pbkdf2_hash`] will write and [`pbkdf2_verify`]
/// will run: ten times [`PBKDF2_SHA256_RECOMMENDED_ITERATIONS`].
///
/// The count of a stored hash comes from the stored string; without a
/// ceiling one crafted `i=4294967295` pins a thread for hours. Hashing is
/// held to the same ceiling so this crate never writes a hash it would
/// refuse to verify.
pub const PBKDF2_SHA256_MAX_ITERATIONS: u32 = 10 * PBKDF2_SHA256_RECOMMENDED_ITERATIONS;

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
/// Errors when `iterations` is below [`PBKDF2_SHA256_MIN_ITERATIONS`] or
/// above [`PBKDF2_SHA256_MAX_ITERATIONS`].
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
/// [`CryptoError::MalformedHash`] when the string is not a well-formed
/// `pbkdf2-sha256` hash — including when it is some *other* scheme's hash,
/// which this function cannot check, and when its iteration count is zero
/// or above [`PBKDF2_SHA256_MAX_ITERATIONS`]. Use [`verify_password_any_scheme`] when
/// the stored hash may be of either scheme this crate supports.
pub fn pbkdf2_verify(password: &str, hash: &str) -> Result<(), CryptoError> {
    use base64ct::{Base64, Encoding};

    // `$pbkdf2-sha256$i=N$salt$dk` splits into a leading empty field plus
    // four populated ones.
    let parts: Vec<&str> = hash.split('$').collect();
    if parts.len() != 5 || !parts[0].is_empty() || parts[1] != PBKDF2_SHA256_ID {
        return Err(CryptoError::MalformedHash(format!(
            "not a {PBKDF2_SHA256_ID} hash"
        )));
    }

    let iterations: u32 = parts[2]
        .strip_prefix("i=")
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| CryptoError::MalformedHash("invalid iteration count".to_string()))?;

    let salt = Base64::decode_vec(parts[3])
        .map_err(|e| CryptoError::MalformedHash(format!("invalid salt: {e}")))?;
    if salt.is_empty() {
        return Err(CryptoError::MalformedHash("empty salt".to_string()));
    }
    let expected = Base64::decode_vec(parts[4])
        .map_err(|e| CryptoError::MalformedHash(format!("invalid hash: {e}")))?;

    // The derived-key length is fixed rather than taken from the stored
    // string. PBKDF2 with a shorter `dkLen` returns a PREFIX of the longer
    // output, so deriving `expected.len()` bytes would let a truncated
    // stored hash verify against the same password — an 8-byte "hash" would
    // be checked at 64 bits. Nothing this crate writes is anything but
    // `PBKDF2_DK_LEN`, so anything else is malformed.
    if expected.len() != PBKDF2_DK_LEN {
        return Err(CryptoError::MalformedHash(format!(
            "derived key must be {PBKDF2_DK_LEN} bytes, got {}",
            expected.len()
        )));
    }

    let computed = pbkdf2_derive(password, &salt, iterations, PBKDF2_DK_LEN)
        .map_err(CryptoError::MalformedHash)?;

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
    if iterations > PBKDF2_SHA256_MAX_ITERATIONS {
        return Err(format!(
            "PBKDF2 iteration count {iterations} exceeds the ceiling of \
             {PBKDF2_SHA256_MAX_ITERATIONS}"
        ));
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
/// default. PBKDF2 needs almost no memory, for a runtime an embedder does
/// not want to spend argon2id's in; it is not cheaper in CPU (about 180 ms
/// per hash at [`PBKDF2_SHA256_RECOMMENDED_ITERATIONS`] in wasm32 under V8,
/// against about 3-8 ms for [`Argon2Cost::Constrained`]), so under a tight
/// CPU budget choose `Argon2(Argon2Cost::Constrained)`.
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

/// Hash `password` under `scheme`, peppered with the current key of
/// `peppers` when it has one, producing that scheme's PHC-style string.
///
/// Only argon2id is peppered: PBKDF2 with a current pepper key is
/// [`CryptoError::Pepper`], never an unpeppered hash.
pub fn hash_password_with(
    password: &str,
    scheme: PasswordScheme,
    peppers: &PasswordPeppers,
) -> Result<String, CryptoError> {
    match (scheme, peppers.current()) {
        (PasswordScheme::Argon2(cost), Some(pepper)) => {
            hash_password_peppered(password, cost, pepper)
        }
        (PasswordScheme::Argon2(cost), None) => hash_password(password, cost),
        (PasswordScheme::Pbkdf2Sha256 { .. }, Some(_)) => Err(CryptoError::Pepper(
            "a pepper is configured, and only argon2id hashes can be peppered".to_string(),
        )),
        (PasswordScheme::Pbkdf2Sha256 { iterations }, None) => pbkdf2_hash(password, iterations),
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
/// A string that names no scheme this crate knows, or a malformed hash of a
/// known one, is [`CryptoError::MalformedHash`] — never an accept, and
/// never [`CryptoError::PasswordMismatch`], which means only that the
/// password is wrong.
///
/// A peppered hash ([`ARGON2ID_PEPPERED_ID`]) verifies with the key of
/// `peppers` it names; an unpeppered one of either scheme verifies unless
/// `peppers` requires a pepper. Either pepper fault is
/// [`CryptoError::Pepper`], reported before any derivation runs.
pub fn verify_password_any_scheme(
    password: &str,
    hash: &str,
    peppers: &PasswordPeppers,
) -> Result<(), CryptoError> {
    // The scheme identifier is the first field of a PHC string
    // (`$<id>$<params>$<salt>$<hash>`), so it is what follows the leading
    // `$`. Dispatching on it explicitly — rather than handing an unknown
    // string to one verifier and trusting it to refuse — is what makes
    // "never an accept" checkable, and it names the fault precisely: a hash
    // written by some third scheme is a broken stored credential, and
    // reporting it as a mismatch would tell the logs, forever, that the
    // user keeps typing the wrong password.
    match hash
        .strip_prefix('$')
        .and_then(|rest| rest.split('$').next())
    {
        Some(ARGON2ID_PEPPERED_ID) => verify_password_peppered(password, hash, peppers),
        Some(PBKDF2_SHA256_ID) | Some("argon2id" | "argon2i" | "argon2d") if peppers.required => {
            Err(CryptoError::Pepper(
                "the stored hash is not peppered, and a pepper is required".to_string(),
            ))
        }
        Some(PBKDF2_SHA256_ID) => pbkdf2_verify(password, hash),
        Some("argon2id" | "argon2i" | "argon2d") => verify_password(password, hash),
        _ => Err(CryptoError::MalformedHash(
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

    fn claims_with_sub(sub: &str) -> BTreeMap<String, serde_json::Value> {
        let mut m = BTreeMap::new();
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

    /// `from_std` admits this expiry (it is under `i64::MAX` milliseconds)
    /// but `now + expiry` is past the last date chrono can represent; adding
    /// the two with `+` panics.
    #[test]
    fn jwt_sign_rejects_an_expiry_past_the_last_date() {
        let err = jwt_sign(
            claims_with_sub("u1"),
            Duration::from_secs(10_000_000_000_000),
            SECRET,
        )
        .expect_err("an expiry past the last representable date must error");
        match err {
            CryptoError::SignError(msg) => assert!(msg.contains("expiry out of range"), "{msg}"),
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
        let token = jwt_encode_unstamped(claims, SECRET).unwrap();

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
        let token = jwt_encode_unstamped(claims_with_sub("u1"), SECRET).unwrap();

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
        let token = jwt_encode_unstamped(claims, SECRET).unwrap();
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
            Err(CryptoError::MalformedHash(_))
        ));
        assert!(matches!(
            verify_password("anything", ""),
            Err(CryptoError::MalformedHash(_))
        ));
    }

    /// A 16-byte salt and 32-byte output, valid PHC fields, so only the
    /// cost parameters of the strings built from it are in question.
    const ARGON2_TAIL: &str = "$AAECAwQFBgcICQoLDA0ODw$AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8";

    /// Run `f` on its own thread and fail if it has not returned within
    /// `secs`: a derivation at the costs these tests use runs for hours,
    /// which must read as a failure, not a hung suite.
    fn within_secs<T: Send + 'static>(secs: u64, f: impl FnOnce() -> T + Send + 'static) -> T {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(f());
        });
        rx.recv_timeout(std::time::Duration::from_secs(secs))
            .unwrap_or_else(|_| panic!("did not return within {secs}s"))
    }

    /// The costs of a stored hash come from the stored string. Above the
    /// ceilings, verification is refused before any derivation runs, as a
    /// malformed hash rather than a wrong password.
    #[test]
    fn argon2_costs_above_the_ceiling_are_refused_without_deriving() {
        for params in [
            "m=8,t=4294967295,p=1".to_string(),
            format!("m={},t=2,p=1", ARGON2_MAX_M_COST + 1),
            "m=4294967295,t=2,p=1".to_string(),
            format!("m=4096,t={},p=1", ARGON2_MAX_T_COST + 1),
            format!("m=4096,t=2,p={}", ARGON2_MAX_P_COST + 1),
        ] {
            let hash = format!("$argon2id$v=19${params}{ARGON2_TAIL}");
            let result = within_secs(5, move || verify_password("pw", &hash));
            match result {
                Err(CryptoError::MalformedHash(m)) => {
                    assert!(m.contains("ceiling"), "{params}: {m}");
                }
                other => panic!("{params}: expected MalformedHash, got {other:?}"),
            }
        }
    }

    /// argon2id known-answer vectors from an independent implementation
    /// (the reference C library, through Python's argon2-cffi 25.1
    /// `low_level.hash_secret`), all over the password below and salt bytes
    /// `00..0f`, each standing in for a credential already stored.
    const KAT_PASSWORD: &str = "correcthorsebatterystaple";
    /// [`Argon2Cost::Default`]'s costs.
    const KAT_DEFAULT: &str = "$argon2id$v=19$m=19456,t=2,p=1$AAECAwQFBgcICQoLDA0ODw$7RmKhudBqFsX3LFSkYHsVYCSSV/j3hL/zZ9AjkvygIo";
    /// [`Argon2Cost::Constrained`]'s costs.
    const KAT_CONSTRAINED: &str = "$argon2id$v=19$m=4096,t=2,p=1$AAECAwQFBgcICQoLDA0ODw$DOJe9Dre1CKOGYGj/SicLaOiPXVXJ1Jame2jnwMAoGU";
    /// Exactly at [`ARGON2_MAX_M_COST`]: OWASP's 46 MiB, 1-iteration option.
    const KAT_AT_CEILING: &str = "$argon2id$v=19$m=47104,t=1,p=1$AAECAwQFBgcICQoLDA0ODw$xnXTdHV0WrguRO6PuHmv73XvW60GvsB6rAVhzZddIts";
    /// argon2-cffi's `PasswordHasher` default (RFC 9106's 64 MiB option):
    /// a well-formed, correct hash above the memory ceiling.
    const KAT_OVER_CEILING: &str = "$argon2id$v=19$m=65536,t=3,p=4$AAECAwQFBgcICQoLDA0ODw$ig/1Ydv8lGLja+cEry2Q+/MeqvCw1xexf4oGjq9DiAQ";

    #[test]
    fn stored_hashes_within_the_ceilings_verify() {
        for hash in [KAT_DEFAULT, KAT_CONSTRAINED, KAT_AT_CEILING] {
            verify_password(KAT_PASSWORD, hash).unwrap_or_else(|e| panic!("{hash}: {e:?}"));
            assert!(
                matches!(
                    verify_password("wrong", hash),
                    Err(CryptoError::PasswordMismatch)
                ),
                "{hash}"
            );
        }
    }

    /// A correct password against a genuine hash whose memory cost is above
    /// what a Workers isolate can run is refused, before deriving, with an
    /// error that names the cost and the ceiling — not verified, and not
    /// reported as a wrong password.
    #[test]
    fn a_genuine_hash_above_the_memory_ceiling_is_refused() {
        match verify_password_any_scheme(
            KAT_PASSWORD,
            KAT_OVER_CEILING,
            &PasswordPeppers::default(),
        ) {
            Err(CryptoError::MalformedHash(m)) => assert_eq!(
                m,
                "argon2: memory cost m=65536 exceeds the ceiling of 47104 this runtime will run"
            ),
            other => panic!("expected MalformedHash, got {other:?}"),
        }
    }

    /// A class buffer is reused dirty by derivations of other sizes; each
    /// must still produce what the `argon2` crate computes in fresh memory.
    #[test]
    fn a_reused_class_buffer_derives_what_fresh_memory_does() {
        let mut memory = Argon2Memory::new();
        for (m, t, p) in [
            (47104, 1, 1),
            (20000, 2, 1),
            (46000, 1, 2),
            (19456, 2, 1),
            (4096, 2, 1),
            (3000, 3, 1),
            (19456, 1, 4),
        ] {
            let params = argon2::Params::new(m, t, p, None).expect("params");
            let argon2 =
                argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
            let salt = [7u8; 16];
            let mut pooled = [0u8; 32];
            let mut fresh = [0u8; 32];
            memory
                .derive(&argon2, b"pw", &salt, &mut pooled)
                .expect("pooled");
            argon2
                .hash_password_into(b"pw", &salt, &mut fresh)
                .expect("fresh");
            assert_eq!(pooled, fresh, "m={m} t={t} p={p}");
        }
        // Seven derivations of six sizes, three buffers.
        assert!(memory.classes.iter().all(Option::is_some));
    }

    /// A kept buffer holds nothing of the derivation that used it.
    #[test]
    fn a_class_buffer_is_zeroised_after_each_derivation() {
        let mut memory = Argon2Memory::new();
        for m in [4096, 20000, 19456] {
            let params = argon2::Params::new(m, 1, 1, None).expect("params");
            let argon2 =
                argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
            memory
                .derive(&argon2, b"pw", &[7u8; 16], &mut [0u8; 32])
                .expect("derive");
            for (class, buffer) in memory.classes.iter().enumerate() {
                if let Some(buffer) = buffer {
                    assert!(
                        buffer.iter().all(|b| b.as_ref().iter().all(|&w| w == 0)),
                        "class {class} holds derivation state after m={m}"
                    );
                }
            }
        }
    }

    /// Each derivation takes the smallest class that holds it.
    #[test]
    fn a_derivation_takes_the_smallest_class_that_holds_it() {
        for (m, class) in [
            (3000, 0),
            (4096, 0),
            (4100, 1),
            (19456, 1),
            (20000, 2),
            (47104, 2),
        ] {
            let mut memory = Argon2Memory::new();
            let params = argon2::Params::new(m, 1, 1, None).expect("params");
            let argon2 =
                argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
            memory
                .derive(&argon2, b"pw", &[7u8; 16], &mut [0u8; 32])
                .expect("derive");
            let used: Vec<usize> = (0..ARGON2_MEMORY_CLASSES.len())
                .filter(|&i| memory.classes[i].is_some())
                .collect();
            assert_eq!(used, vec![class], "m={m}");
        }
    }

    /// The memory ceiling is OWASP's largest argon2id memory cost, 46 MiB.
    #[test]
    fn the_memory_ceiling_is_46_mib() {
        assert_eq!(ARGON2_MAX_M_COST, 47104);
    }

    /// The ceilings sit above everything this crate writes, so no hash it
    /// produced is refused by them.
    #[test]
    fn the_argon2_presets_sit_under_the_ceilings() {
        for cost in [Argon2Cost::Default, Argon2Cost::Constrained] {
            let hash = hash_password("pw", cost).expect("hash");
            verify_password("pw", &hash).expect("a preset hash verifies");
        }
    }

    /// A hash without an output cannot be checked; the argon2 verifier
    /// reports that as a wrong password, which it is not.
    #[test]
    fn an_argon2_hash_without_output_is_malformed_not_a_mismatch() {
        let no_output = "$argon2id$v=19$m=4096,t=2,p=1$AAECAwQFBgcICQoLDA0ODw";
        assert!(matches!(
            verify_password("pw", no_output),
            Err(CryptoError::MalformedHash(_))
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
    fn malformed_hashes_are_malformed_hash_errors_not_panics() {
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
                Err(CryptoError::MalformedHash(_)) => {}
                other => panic!("expected MalformedHash for {bad:?}, got {other:?}"),
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
            Err(CryptoError::MalformedHash(m)) => {
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
            Err(CryptoError::MalformedHash(_))
        ));
    }

    /// The iteration count of a stored hash comes from the stored string;
    /// above the ceiling, verification is refused before deriving.
    #[test]
    fn an_iteration_count_above_the_ceiling_is_refused_without_deriving() {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(pbkdf2_verify(
                KAT_PASSWORD,
                "$pbkdf2-sha256$i=4294967295$AAECAwQFBgcICQoLDA0ODw==$/6tPyT3P0FDTAPcc3qfsdyi1rxNk5iabYJNHAbMJ8Mg=",
            ));
        });
        match rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(Err(CryptoError::MalformedHash(m))) => assert!(m.contains("ceiling"), "{m}"),
            Ok(other) => panic!("expected MalformedHash, got {other:?}"),
            Err(_) => panic!("i=4294967295 must be refused, not derived"),
        }
    }

    /// This crate never writes a hash it would refuse to verify.
    #[test]
    fn hashing_above_the_ceiling_is_refused() {
        let err = pbkdf2_hash("pw", PBKDF2_SHA256_MAX_ITERATIONS + 1)
            .expect_err("above the ceiling must be refused");
        assert!(
            matches!(&err, CryptoError::HashError(m) if m.contains("ceiling")),
            "{err:?}"
        );
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
        verify_password_any_scheme("pw-a", &argon2, &PasswordPeppers::default())
            .expect("argon2 accepted");
        verify_password_any_scheme(KAT_PASSWORD, KAT_HASH, &PasswordPeppers::default())
            .expect("pbkdf2 accepted");
    }

    #[test]
    fn rejects_the_wrong_password_under_either_scheme() {
        let argon2 = hash_password("pw-a", Argon2Cost::Constrained).expect("argon2 hash");
        assert!(matches!(
            verify_password_any_scheme("nope", &argon2, &PasswordPeppers::default()),
            Err(CryptoError::PasswordMismatch)
        ));
        assert!(matches!(
            verify_password_any_scheme("nope", KAT_HASH, &PasswordPeppers::default()),
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
            match verify_password_any_scheme("anything", bad, &PasswordPeppers::default()) {
                // An unknown scheme is a broken stored credential, not a
                // wrong password: an operator reading `PasswordMismatch`
                // here would chase the user instead of the database.
                Err(CryptoError::MalformedHash(m)) => {
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

#[cfg(test)]
mod pepper_tests {
    use super::*;

    const PASSWORD: &str = "correcthorsebatterystaple";
    /// Bytes 0x20..0x40, base64.
    const KEY_1_B64: &str = "ICEiIyQlJicoKSorLC0uLzAxMjM0NTY3ODk6Ozw9Pj8=";
    const KEY_1_ID: &str = "b57af81f66f733f4";
    /// Bytes 0x40..0x60, base64.
    const KEY_2_B64: &str = "QEFCQ0RFRkdISUpLTE1OT1BRUlNUVVZXWFlaW1xdXl8=";
    const KEY_2_ID: &str = "d78a88c0339a5e5d";

    /// `PASSWORD` under argon2id at `Argon2Cost::Constrained`, salt bytes
    /// 0..16, peppered with key 1. Computed independently of this crate:
    /// argon2-cffi's `hash_secret_raw` for the argon2id output, Python's
    /// `hmac` for the MAC and the key id.
    const KAT_PEPPERED: &str = "$argon2id-hmac-sha256$v=19$m=4096,t=2,p=1,\
        pepper=b57af81f66f733f4$AAECAwQFBgcICQoLDA0ODw$\
        zVRcJ4xagSyl/Se5+ELmH7dEUVf0ykQfI0sE0HlZofg";

    fn key1() -> PepperKey {
        PepperKey::from_base64(KEY_1_B64).unwrap()
    }
    fn key2() -> PepperKey {
        PepperKey::from_base64(KEY_2_B64).unwrap()
    }
    fn only(key: PepperKey) -> PasswordPeppers {
        PasswordPeppers::new(Some(key), Vec::new(), false).unwrap()
    }
    fn is_pepper_error(r: Result<(), CryptoError>) -> bool {
        matches!(r, Err(CryptoError::Pepper(_)))
    }

    #[test]
    fn known_answer() {
        assert_eq!(key1().id(), KEY_1_ID);
        assert_eq!(key2().id(), KEY_2_ID);
        let salt: Vec<u8> = (0u8..16).collect();
        let hash =
            hash_password_peppered_with_salt(PASSWORD, Argon2Cost::Constrained, &key1(), &salt)
                .unwrap();
        assert_eq!(hash, KAT_PEPPERED);
        verify_password_peppered(PASSWORD, KAT_PEPPERED, &only(key1())).expect("verifies");
        assert!(matches!(
            verify_password_peppered("wrong", KAT_PEPPERED, &only(key1())),
            Err(CryptoError::PasswordMismatch)
        ));
    }

    /// A hash names its key; with that key missing — no pepper at all, or
    /// only some other key — verification fails as a pepper fault, never as
    /// a wrong password, whatever the password.
    #[test]
    fn a_missing_key_is_a_pepper_error_not_a_mismatch() {
        for peppers in [PasswordPeppers::default(), only(key2())] {
            for password in [PASSWORD, "wrong"] {
                match verify_password_any_scheme(password, KAT_PEPPERED, &peppers) {
                    Err(CryptoError::Pepper(msg)) => assert!(msg.contains(KEY_1_ID), "{msg}"),
                    other => panic!("expected a pepper error, got {other:?}"),
                }
            }
        }
    }

    /// The MAC really is keyed by the pepper: the same hash relabelled to
    /// name a different configured key does not verify, even with the right
    /// password.
    #[test]
    fn the_pepper_key_is_what_verifies() {
        let relabelled = KAT_PEPPERED.replace(KEY_1_ID, KEY_2_ID);
        assert!(matches!(
            verify_password_peppered(PASSWORD, &relabelled, &only(key2())),
            Err(CryptoError::PasswordMismatch)
        ));
    }

    /// The stored value is not the argon2id output: stripped of its pepper
    /// label and read as a plain argon2id hash, it does not verify.
    #[test]
    fn the_stored_value_is_not_the_bare_argon2_output() {
        let bare = KAT_PEPPERED
            .replace(ARGON2ID_PEPPERED_ID, "argon2id")
            .replace(&format!(",pepper={KEY_1_ID}"), "");
        assert!(matches!(
            verify_password(PASSWORD, &bare),
            Err(CryptoError::PasswordMismatch)
        ));
    }

    /// Rotation: with a new current key and the old one kept as previous,
    /// old hashes verify and new ones name the new key. Dropping the old key
    /// strands only the hashes that name it.
    #[test]
    fn rotation() {
        let rotated = PasswordPeppers::new(Some(key2()), vec![key1()], false).unwrap();
        verify_password_any_scheme(PASSWORD, KAT_PEPPERED, &rotated).expect("old key verifies");

        let fresh = hash_password_with(
            PASSWORD,
            PasswordScheme::Argon2(Argon2Cost::Constrained),
            &rotated,
        )
        .unwrap();
        assert!(fresh.contains(&format!("pepper={KEY_2_ID}$")), "{fresh}");
        verify_password_any_scheme(PASSWORD, &fresh, &rotated).expect("new hash verifies");

        let dropped = only(key2());
        verify_password_any_scheme(PASSWORD, &fresh, &dropped).expect("still verifies");
        assert!(is_pepper_error(verify_password_any_scheme(
            PASSWORD,
            KAT_PEPPERED,
            &dropped
        )));
    }

    /// Unpeppered hashes of both schemes keep verifying once a pepper is
    /// configured, and are refused once one is required.
    #[test]
    fn unpeppered_hashes_verify_unless_a_pepper_is_required() {
        let argon2 = hash_password(PASSWORD, Argon2Cost::Constrained).unwrap();
        let pbkdf2 = pbkdf2_hash(PASSWORD, PBKDF2_SHA256_MIN_ITERATIONS).unwrap();
        let optional = only(key1());
        let required = PasswordPeppers::new(Some(key1()), Vec::new(), true).unwrap();
        for legacy in [&argon2, &pbkdf2] {
            verify_password_any_scheme(PASSWORD, legacy, &PasswordPeppers::default())
                .expect("no pepper");
            verify_password_any_scheme(PASSWORD, legacy, &optional).expect("pepper optional");
            assert!(matches!(
                verify_password_any_scheme("wrong", legacy, &optional),
                Err(CryptoError::PasswordMismatch)
            ));
            assert!(is_pepper_error(verify_password_any_scheme(
                PASSWORD, legacy, &required
            )));
        }
        verify_password_any_scheme(PASSWORD, KAT_PEPPERED, &required).expect("peppered verifies");
    }

    #[test]
    fn hashing_uses_the_current_key_or_none() {
        let scheme = PasswordScheme::Argon2(Argon2Cost::Constrained);
        let plain = hash_password_with(PASSWORD, scheme, &PasswordPeppers::default()).unwrap();
        assert!(plain.starts_with("$argon2id$"), "{plain}");
        let peppered = hash_password_with(PASSWORD, scheme, &only(key1())).unwrap();
        assert!(
            peppered.starts_with("$argon2id-hmac-sha256$v=19$m=4096,t=2,p=1,pepper="),
            "{peppered}"
        );
        let pbkdf2 = PasswordScheme::Pbkdf2Sha256 {
            iterations: PBKDF2_SHA256_MIN_ITERATIONS,
        };
        assert!(matches!(
            hash_password_with(PASSWORD, pbkdf2, &only(key1())),
            Err(CryptoError::Pepper(_))
        ));
    }

    /// The key lookup comes first: a hash naming an unknown key is refused
    /// without running argon2 at its (here absurd) stored cost.
    #[test]
    fn a_missing_key_is_refused_before_any_derivation() {
        let absurd = KAT_PEPPERED.replace("m=4096", "m=4294967295");
        assert!(is_pepper_error(verify_password_peppered(
            PASSWORD,
            &absurd,
            &PasswordPeppers::default()
        )));
        assert!(matches!(
            verify_password_peppered(PASSWORD, &absurd, &only(key1())),
            Err(CryptoError::MalformedHash(_))
        ));
    }

    #[test]
    fn malformed_peppered_hashes() {
        let peppers = only(key1());
        let pepper = format!("pepper={KEY_1_ID}");
        for bad in [
            KAT_PEPPERED.replace("v=19", "v=16"),
            KAT_PEPPERED.replace("$v=19", ""),
            KAT_PEPPERED.replace(&format!(",{pepper}"), ""),
            KAT_PEPPERED.replace("p=1,", "p=1,x=1,"),
            KAT_PEPPERED.replace("p=1,", "p=1,p=1,"),
            KAT_PEPPERED.replace(
                "zVRcJ4xagSyl/Se5+ELmH7dEUVf0ykQfI0sE0HlZofg",
                "zVRcJ4xagSyl/Se5",
            ),
            KAT_PEPPERED.replace("$zVRcJ4xagSyl/Se5+ELmH7dEUVf0ykQfI0sE0HlZofg", ""),
            KAT_PEPPERED.replace(ARGON2ID_PEPPERED_ID, "argon2id-hmac-sha512"),
        ] {
            assert!(
                matches!(
                    verify_password_peppered(PASSWORD, &bad, &peppers),
                    Err(CryptoError::MalformedHash(_))
                ),
                "{bad} must be malformed"
            );
        }
    }

    /// Structural guard, passes before and after by design: timing cannot
    /// be observed in a unit test, so this pins that the MAC comparison goes
    /// through `constant_time_eq`, and that `constant_time_eq` is `subtle`'s.
    #[test]
    fn the_mac_comparison_is_constant_time() {
        let src = include_str!("primitives.rs");
        let start = src
            .find("pub fn verify_password_peppered(")
            .expect("verifier present");
        let body = &src[start..start + src[start..].find("\n}\n").expect("fn end")];
        assert!(
            body.contains("if constant_time_eq(&mac, expected.as_bytes())"),
            "the MAC must be compared with constant_time_eq"
        );
        assert!(!body.contains("mac =="), "no `==` on the MAC");
        assert!(!body.contains("== expected"), "no `==` on the MAC");
        let ct = &src[src.find("pub fn constant_time_eq(").unwrap()..];
        let ct = &ct[..ct.find("\n}\n").unwrap()];
        assert!(
            ct.contains("ct_eq("),
            "constant_time_eq must use subtle: {ct}"
        );
    }

    #[test]
    fn configuration_is_validated() {
        let err = |r: Result<PasswordPeppers, CryptoError>| match r {
            Err(CryptoError::Pepper(msg)) => msg,
            other => panic!("expected a pepper error, got {other:?}"),
        };
        // 31 bytes.
        let short = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHg==";
        let msg = err(PasswordPeppers::from_config(Some(short), None, false));
        assert!(
            msg.contains("current key") && msg.contains("at least 32 bytes"),
            "{msg}"
        );
        let msg = err(PasswordPeppers::from_config(
            Some("not base64!"),
            None,
            false,
        ));
        assert!(msg.contains("base64"), "{msg}");
        assert!(
            !msg.contains("not base64!"),
            "the key text must not leak: {msg}"
        );
        let msg = err(PasswordPeppers::from_config(
            Some(KEY_1_B64),
            Some(&format!("{KEY_2_B64},{short}")),
            false,
        ));
        assert!(msg.contains("previous key 2"), "{msg}");
        let msg = err(PasswordPeppers::from_config(
            Some(KEY_1_B64),
            Some(&format!("{KEY_2_B64},")),
            false,
        ));
        assert!(msg.contains("previous key 2: empty entry"), "{msg}");
        let msg = err(PasswordPeppers::from_config(None, None, true));
        assert!(msg.contains("required"), "{msg}");
        let msg = err(PasswordPeppers::from_config(None, Some(KEY_1_B64), false));
        assert!(msg.contains("without a current key"), "{msg}");
        let msg = err(PasswordPeppers::from_config(
            Some(KEY_1_B64),
            Some(&format!("{KEY_2_B64}, {KEY_1_B64}")),
            false,
        ));
        assert!(
            msg.contains(KEY_1_ID) && msg.contains("more than once"),
            "{msg}"
        );

        let ok = PasswordPeppers::from_config(
            Some(&format!(" {KEY_2_B64} ")),
            Some(&format!("{KEY_1_B64} ")),
            true,
        )
        .unwrap();
        assert_eq!(ok.current().map(PepperKey::id), Some(KEY_2_ID));
        assert_eq!(ok.previous().len(), 1);
        assert!(ok.is_required());
        let none = PasswordPeppers::from_config(Some("  "), Some(""), false).unwrap();
        assert!(none.current().is_none() && !none.is_required());
    }

    #[test]
    fn debug_never_prints_key_material() {
        let dbg = format!("{:?}", only(key1()));
        assert!(dbg.contains(KEY_1_ID), "{dbg}");
        assert!(!dbg.contains("32, 33"), "{dbg}");
    }
}
