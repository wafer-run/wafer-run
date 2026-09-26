use std::sync::Arc;

use wafer_block::{
    config::BlockConfig, ConfigVar, ErrorCode, InputType, LifecycleEvent, LifecycleType, WaferError,
};

use crate::interfaces::crypto::{
    handler,
    service::{CryptoService, PasswordPepperConfig},
};

/// Config key holding the key new password hashes are peppered with:
/// standard base64 of at least 32 random bytes. Its value must come from the
/// embedder's secret store or process environment, never from the database
/// the hashes live in — a pepper stored beside the hashes protects nothing.
pub const PASSWORD_PEPPER_KEY: &str = "WAFER_RUN__CRYPTO__PASSWORD_PEPPER_KEY";

/// Config key holding earlier pepper keys, comma-separated, that stored
/// hashes may still name. They verify and never hash. Held outside the
/// database for the same reason as [`PASSWORD_PEPPER_KEY`].
pub const PASSWORD_PEPPER_PREVIOUS_KEY: &str = "WAFER_RUN__CRYPTO__PASSWORD_PEPPER_PREVIOUS_KEY";

/// Config key turning on "every password is peppered": `"true"` or
/// `"false"` (the default).
pub const PASSWORD_PEPPER_REQUIRED: &str = "WAFER_RUN__CRYPTO__PASSWORD_PEPPER_REQUIRED";

crate::service_block! {
    /// Unified crypto block. Wraps any `CryptoService` implementation.
    ///
    /// Declares the password-pepper config keys and hands their values to
    /// the service at `lifecycle(Init)`
    /// ([`CryptoService::configure_password_pepper`]); settings the service
    /// refuses fail the Init, so the block never answers under them.
    block: pub CryptoBlock,
    name: "wafer-run/crypto",
    version: "0.0.1",
    interface: "crypto@v1",
    description: "Cryptographic operations (hashing, JWT, random bytes)",
    category: Service,
    fields: { service: Arc<dyn CryptoService> },
    info_extras: |_this, info| info.config_keys(pepper_config_vars()),
    handle: |this, ctx, msg, body| {
        handler::handle_message(this.service.as_ref(), ctx, ctx.caller_id(), &msg, &body).await
    },
    lifecycle: |this, _ctx, event| {
        if event.event_type == LifecycleType::Init {
            let config = pepper_config_from(&event)?;
            this.service
                .configure_password_pepper(&config)
                .map_err(|e| WaferError::new(ErrorCode::FailedPrecondition, e.to_string()))?;
        }
        Ok(())
    },
}

fn pepper_config_vars() -> Vec<ConfigVar> {
    vec![
        ConfigVar::new(
            PASSWORD_PEPPER_KEY,
            "Secret key every new password hash is peppered with (HMAC-SHA-256 over the \
             Argon2id output): standard base64 of at least 32 random bytes, e.g. \
             `openssl rand -base64 32`. Supply it from a secret store or the process \
             environment, never from the database. Losing it makes every password \
             peppered with it unverifiable. Unset: new hashes are not peppered.",
            "",
        )
        .name("Password Pepper Key")
        .input_type(InputType::Password)
        .warning(
            "Changing this without moving the old key to the previous-keys setting \
             locks out every user whose password was peppered with it.",
        )
        .optional(),
        ConfigVar::new(
            PASSWORD_PEPPER_PREVIOUS_KEY,
            "Earlier pepper keys, comma-separated standard base64, that stored hashes may \
             still name. They verify existing hashes and never pepper a new one. Move the \
             current key here when rotating.",
            "",
        )
        .name("Previous Password Pepper Keys")
        .input_type(InputType::Password)
        .optional(),
        ConfigVar::new(
            PASSWORD_PEPPER_REQUIRED,
            "When \"true\", every password is peppered: the block refuses to start without \
             a pepper key, and a stored hash without a pepper is refused instead of \
             verified. Turn on once every stored hash is peppered.",
            "false",
        )
        .name("Require Password Pepper")
        .input_type(InputType::Toggle),
    ]
}

/// Read the pepper settings from the Init payload. An empty value is unset.
/// The required flag is parsed strictly: a value that is neither true nor
/// false fails the Init rather than reading as "not required".
fn pepper_config_from(event: &LifecycleEvent) -> Result<PasswordPepperConfig, WaferError> {
    let cfg = BlockConfig::from_event(event);
    let set = |key: &str| {
        let value = cfg.str(key).trim();
        (!value.is_empty()).then(|| value.to_string())
    };
    let required = match cfg.get(PASSWORD_PEPPER_REQUIRED) {
        None => false,
        Some(serde_json::Value::Bool(b)) => *b,
        Some(serde_json::Value::String(s)) => match s.trim().to_ascii_lowercase().as_str() {
            "" | "false" | "0" => false,
            "true" | "1" => true,
            other => return Err(invalid_required(other)),
        },
        Some(other) => return Err(invalid_required(&other.to_string())),
    };
    Ok(PasswordPepperConfig {
        current_key: set(PASSWORD_PEPPER_KEY),
        previous_keys: set(PASSWORD_PEPPER_PREVIOUS_KEY),
        required,
    })
}

fn invalid_required(value: &str) -> WaferError {
    WaferError::new(
        ErrorCode::InvalidArgument,
        format!("{PASSWORD_PEPPER_REQUIRED} must be \"true\" or \"false\"; got {value:?}"),
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use wafer_block::Block;

    use super::*;
    use crate::interfaces::crypto::service::CryptoError;

    /// Records what the block handed it, and refuses on request.
    #[derive(Default)]
    struct RecordingCrypto {
        seen: Mutex<Vec<PasswordPepperConfig>>,
        refuse: bool,
    }

    #[wafer_block::wafer_async_trait]
    impl CryptoService for RecordingCrypto {
        async fn hash(&self, _: &str) -> Result<String, CryptoError> {
            unimplemented!()
        }
        async fn compare_hash(&self, _: &str, _: &str) -> Result<(), CryptoError> {
            unimplemented!()
        }
        fn configure_password_pepper(
            &self,
            config: &PasswordPepperConfig,
        ) -> Result<(), CryptoError> {
            self.seen.lock().unwrap().push(config.clone());
            if self.refuse {
                return Err(CryptoError::Pepper("refused".into()));
            }
            Ok(())
        }
        async fn sign_for(
            &self,
            _: &str,
            _: std::collections::BTreeMap<String, serde_json::Value>,
            _: std::time::Duration,
        ) -> Result<String, CryptoError> {
            unimplemented!()
        }
        async fn verify_for(
            &self,
            _: &str,
            _: &str,
        ) -> Result<std::collections::BTreeMap<String, serde_json::Value>, CryptoError> {
            unimplemented!()
        }
        async fn random_bytes(&self, _: usize) -> Result<Vec<u8>, CryptoError> {
            unimplemented!()
        }
    }

    fn init(data: serde_json::Value) -> LifecycleEvent {
        LifecycleEvent {
            event_type: LifecycleType::Init,
            data: serde_json::to_vec(&data).unwrap(),
        }
    }

    async fn run_init(
        svc: &Arc<RecordingCrypto>,
        data: serde_json::Value,
    ) -> Result<(), WaferError> {
        let block = CryptoBlock::new(Arc::clone(svc) as Arc<dyn CryptoService>);
        let ctx = crate::test_support::noop_context();
        block.lifecycle(&*ctx, init(data)).await
    }

    /// The keys are the block's own, so the runtime resolves them for it,
    /// and both key settings are sensitive.
    #[test]
    fn declares_the_pepper_keys_under_its_prefix() {
        let info = CryptoBlock::new(Arc::new(RecordingCrypto::default())).info();
        info.validate(CryptoBlock::NAME)
            .expect("the keys carry the block's prefix");
        let keys: Vec<&ConfigVar> = info.config_keys.iter().collect();
        let key = |k: &str| keys.iter().find(|v| v.key == k).copied().expect(k);
        assert!(key(PASSWORD_PEPPER_KEY).is_sensitive());
        assert!(key(PASSWORD_PEPPER_PREVIOUS_KEY).is_sensitive());
        assert!(key(PASSWORD_PEPPER_KEY).optional);
        assert!(key(PASSWORD_PEPPER_PREVIOUS_KEY).optional);
        assert_eq!(key(PASSWORD_PEPPER_REQUIRED).default, "false");
    }

    #[tokio::test]
    async fn init_hands_the_resolved_settings_to_the_service() {
        let svc = Arc::new(RecordingCrypto::default());
        run_init(
            &svc,
            serde_json::json!({
                PASSWORD_PEPPER_KEY: " current ",
                PASSWORD_PEPPER_PREVIOUS_KEY: "a,b",
                PASSWORD_PEPPER_REQUIRED: "true",
            }),
        )
        .await
        .expect("init");
        assert_eq!(
            svc.seen.lock().unwrap().as_slice(),
            [PasswordPepperConfig {
                current_key: Some("current".into()),
                previous_keys: Some("a,b".into()),
                required: true,
            }]
        );
    }

    /// Nothing configured is "no pepper, not required", and the service
    /// still hears it, so an Init retry can clear an earlier setting.
    #[tokio::test]
    async fn init_without_settings_configures_no_pepper() {
        let svc = Arc::new(RecordingCrypto::default());
        run_init(&svc, serde_json::json!({ PASSWORD_PEPPER_KEY: "" }))
            .await
            .expect("init");
        assert_eq!(
            svc.seen.lock().unwrap().as_slice(),
            [PasswordPepperConfig::default()]
        );
    }

    /// A typo in the flag must not read as "not required".
    #[tokio::test]
    async fn an_unparseable_required_flag_fails_init() {
        let svc = Arc::new(RecordingCrypto::default());
        let err = run_init(&svc, serde_json::json!({ PASSWORD_PEPPER_REQUIRED: "yes" }))
            .await
            .expect_err("\"yes\" is not a flag value");
        assert_eq!(err.code, ErrorCode::InvalidArgument);
        assert!(err.message.contains(PASSWORD_PEPPER_REQUIRED), "{err}");
        assert!(svc.seen.lock().unwrap().is_empty());
    }

    /// Settings the service refuses fail the Init permanently (not a
    /// retryable code), so the block never answers under them.
    #[tokio::test]
    async fn a_refused_configuration_fails_init() {
        let svc = Arc::new(RecordingCrypto {
            refuse: true,
            ..Default::default()
        });
        let err = run_init(&svc, serde_json::json!({ PASSWORD_PEPPER_KEY: "k" }))
            .await
            .expect_err("the service refused");
        assert_eq!(err.code, ErrorCode::FailedPrecondition);
        assert!(err.message.contains("password pepper: refused"), "{err}");
    }
}
