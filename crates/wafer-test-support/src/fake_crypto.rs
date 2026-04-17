//! Crypto fake implementing the `crypto@v1` interface using real HMAC-SHA256.

use std::sync::Arc;

use base64ct::{Base64UrlUnpadded, Encoding};
use hmac::{Hmac, Mac};
use parking_lot::Mutex;
use sha2::Sha256;
use wafer_block::{
    common::ErrorCode,
    streams::{input::InputStream, output::OutputStream},
    Block, BlockCategory, BlockInfo, Context, InstanceMode, LifecycleEvent, Message, WaferError,
};

use crate::fake_db::FailureMode;

type HmacSha256 = Hmac<Sha256>;

pub(crate) struct FakeCryptoState {
    pub jwt_secret: Vec<u8>,
    pub failure: FailureMode,
}

pub struct FakeCrypto {
    pub(crate) state: Arc<Mutex<FakeCryptoState>>,
}

impl Default for FakeCrypto {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeCrypto {
    pub fn new() -> Self {
        Self::with_secret(b"test-secret-do-not-use-in-prod".to_vec())
    }

    pub fn with_secret(secret: impl Into<Vec<u8>>) -> Self {
        Self {
            state: Arc::new(Mutex::new(FakeCryptoState {
                jwt_secret: secret.into(),
                failure: FailureMode::None,
            })),
        }
    }

    pub fn set_failure(&self, mode: FailureMode) {
        self.state.lock().failure = mode;
    }

    fn should_fail(&self) -> bool {
        let mut s = self.state.lock();
        match s.failure {
            FailureMode::None => false,
            FailureMode::Unavailable => true,
            FailureMode::FailNextCall(n) => {
                if n <= 1 {
                    s.failure = FailureMode::None;
                } else {
                    s.failure = FailureMode::FailNextCall(n - 1);
                }
                true
            }
        }
    }
}

#[async_trait::async_trait]
impl Block for FakeCrypto {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "test/fake-crypto",
            "0.1.0",
            "crypto@v1",
            "Crypto fake using real HMAC-SHA256",
        )
        .instance_mode(InstanceMode::Singleton)
        .category(BlockCategory::Infrastructure)
    }

    async fn handle(&self, _ctx: &dyn Context, msg: Message, input: InputStream) -> OutputStream {
        if self.should_fail() {
            return OutputStream::error(WaferError::new(
                ErrorCode::INTERNAL,
                "fake-crypto unavailable",
            ));
        }

        let body = input.collect_to_bytes().await;
        let req: serde_json::Value = match serde_json::from_slice(&body) {
            Ok(v) => v,
            Err(e) => {
                return OutputStream::error(WaferError::new(
                    ErrorCode::INVALID_ARGUMENT,
                    format!("fake-crypto: bad request: {e}"),
                ));
            }
        };

        // `msg.kind` carries the action when dispatched directly via `run_block`.
        // `msg.action()` reads `META_REQ_ACTION` meta, which is only set on the
        // HTTP pipeline path. Fall back to `msg.action()` if kind is empty.
        let action = if msg.kind.is_empty() {
            msg.action().to_string()
        } else {
            msg.kind.clone()
        };
        match action.as_str() {
            "crypto.jwt_sign" => self.handle_jwt_sign(&req),
            "crypto.jwt_verify" => self.handle_jwt_verify(&req),
            "crypto.hash" => self.handle_hash(&req),
            other => OutputStream::error(WaferError::new(
                ErrorCode::INVALID_ARGUMENT,
                format!("fake-crypto: action '{other}' not implemented"),
            )),
        }
    }

    async fn lifecycle(
        &self,
        _ctx: &dyn Context,
        _event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        Ok(())
    }
}

impl FakeCrypto {
    fn handle_jwt_sign(&self, req: &serde_json::Value) -> OutputStream {
        let claims = &req["claims"];
        let header = serde_json::json!({"alg": "HS256", "typ": "JWT"});
        let header_b64 = Base64UrlUnpadded::encode_string(&serde_json::to_vec(&header).unwrap());
        let claims_b64 = Base64UrlUnpadded::encode_string(&serde_json::to_vec(claims).unwrap());
        let signing_input = format!("{header_b64}.{claims_b64}");

        let secret = self.state.lock().jwt_secret.clone();
        let mut mac = HmacSha256::new_from_slice(&secret).unwrap();
        mac.update(signing_input.as_bytes());
        let sig_b64 = Base64UrlUnpadded::encode_string(&mac.finalize().into_bytes());

        let token = format!("{signing_input}.{sig_b64}");
        OutputStream::respond(serde_json::to_vec(&serde_json::json!({"token": token})).unwrap())
    }

    fn handle_jwt_verify(&self, req: &serde_json::Value) -> OutputStream {
        let token = match req["token"].as_str() {
            Some(t) => t,
            None => {
                return OutputStream::error(WaferError::new(
                    ErrorCode::INVALID_ARGUMENT,
                    "fake-crypto: missing token",
                ))
            }
        };
        let parts: Vec<&str> = token.split('.').collect();
        if parts.len() != 3 {
            return OutputStream::error(WaferError::new(
                ErrorCode::UNAUTHENTICATED,
                "invalid signature",
            ));
        }
        let signing_input = format!("{}.{}", parts[0], parts[1]);
        let sig_bytes = match Base64UrlUnpadded::decode_vec(parts[2]) {
            Ok(b) => b,
            Err(_) => {
                return OutputStream::error(WaferError::new(
                    ErrorCode::UNAUTHENTICATED,
                    "invalid signature",
                ))
            }
        };

        let secret = self.state.lock().jwt_secret.clone();
        let mut mac = HmacSha256::new_from_slice(&secret).unwrap();
        mac.update(signing_input.as_bytes());
        if mac.verify_slice(&sig_bytes).is_err() {
            return OutputStream::error(WaferError::new(
                ErrorCode::UNAUTHENTICATED,
                "invalid signature",
            ));
        }

        let claims_bytes = match Base64UrlUnpadded::decode_vec(parts[1]) {
            Ok(b) => b,
            Err(_) => {
                return OutputStream::error(WaferError::new(
                    ErrorCode::UNAUTHENTICATED,
                    "invalid claims",
                ))
            }
        };
        let claims: serde_json::Value = match serde_json::from_slice(&claims_bytes) {
            Ok(v) => v,
            Err(_) => {
                return OutputStream::error(WaferError::new(
                    ErrorCode::UNAUTHENTICATED,
                    "invalid claims",
                ))
            }
        };

        OutputStream::respond(
            serde_json::to_vec(&serde_json::json!({"valid": true, "claims": claims})).unwrap(),
        )
    }

    fn handle_hash(&self, req: &serde_json::Value) -> OutputStream {
        use sha2::Digest;
        let data = req["data"].as_str().unwrap_or("");
        let digest = Sha256::digest(data.as_bytes());
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        OutputStream::respond(serde_json::to_vec(&serde_json::json!({"hash": hex})).unwrap())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wafer_block::streams::output::TerminalNotResponse;
    use wafer_run::Wafer;

    #[tokio::test]
    async fn sign_and_verify_roundtrip() {
        let crypto = Arc::new(FakeCrypto::new());
        let mut w = Wafer::new();
        w.register_block("test/fake-crypto", crypto.clone())
            .unwrap();
        w.add_alias("wafer-run/crypto", "test/fake-crypto");
        let wafer = w.start().await.unwrap();

        let sign_req = json!({"claims": {"sub": "u1"}});
        let sign_msg = Message::new("crypto.jwt_sign");
        let sign_out = wafer
            .run_block(
                "wafer-run/crypto",
                sign_msg,
                InputStream::from_bytes(serde_json::to_vec(&sign_req).unwrap()),
            )
            .await;
        let sign_buf = sign_out.collect_buffered().await.expect("sign ok");
        let sign_resp: serde_json::Value = serde_json::from_slice(&sign_buf.body).unwrap();
        let token = sign_resp["token"].as_str().unwrap().to_string();

        let verify_req = json!({"token": token});
        let verify_msg = Message::new("crypto.jwt_verify");
        let verify_out = wafer
            .run_block(
                "wafer-run/crypto",
                verify_msg,
                InputStream::from_bytes(serde_json::to_vec(&verify_req).unwrap()),
            )
            .await;
        let verify_buf = verify_out.collect_buffered().await.expect("verify ok");
        let verify_resp: serde_json::Value = serde_json::from_slice(&verify_buf.body).unwrap();
        assert_eq!(verify_resp["valid"], true);
        assert_eq!(verify_resp["claims"]["sub"], "u1");
    }

    #[tokio::test]
    async fn verify_fails_on_wrong_secret() {
        let signing = Arc::new(FakeCrypto::with_secret(b"secret-a".to_vec()));
        let verifying = Arc::new(FakeCrypto::with_secret(b"secret-b".to_vec()));

        let mut w1 = Wafer::new();
        w1.register_block("test/fake-crypto", signing.clone())
            .unwrap();
        w1.add_alias("wafer-run/crypto", "test/fake-crypto");
        let wafer1 = w1.start().await.unwrap();
        let sign_msg = Message::new("crypto.jwt_sign");
        let sign_out = wafer1
            .run_block(
                "wafer-run/crypto",
                sign_msg,
                InputStream::from_bytes(serde_json::to_vec(&json!({"claims": {}})).unwrap()),
            )
            .await;
        let token: String = serde_json::from_slice::<serde_json::Value>(
            &sign_out.collect_buffered().await.unwrap().body,
        )
        .unwrap()["token"]
            .as_str()
            .unwrap()
            .to_string();

        let mut w2 = Wafer::new();
        w2.register_block("test/fake-crypto", verifying.clone())
            .unwrap();
        w2.add_alias("wafer-run/crypto", "test/fake-crypto");
        let wafer2 = w2.start().await.unwrap();
        let verify_msg = Message::new("crypto.jwt_verify");
        let verify_out = wafer2
            .run_block(
                "wafer-run/crypto",
                verify_msg,
                InputStream::from_bytes(serde_json::to_vec(&json!({"token": token})).unwrap()),
            )
            .await;
        match verify_out.collect_buffered().await {
            Err(TerminalNotResponse::Error(e)) => {
                assert_eq!(e.code, ErrorCode::UNAUTHENTICATED);
            }
            other => panic!("expected signature failure, got {other:?}"),
        }
    }
}
