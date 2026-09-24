//! JSON deserialization for [`WaferFlow`] documents.

use crate::{error::ParseError, types::WaferFlow};

/// Parse a JSON string into a [`WaferFlow`] definition.
///
/// Performs no semantic validation — call [`crate::validate`] on the result
/// before handing the flow to the runtime.
pub fn parse(json: &str) -> Result<WaferFlow, ParseError> {
    let flow: WaferFlow = serde_json::from_str(json)?;
    Ok(flow)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_flow() {
        let json = r#"{
            "id": "test-flow",
            "name": "Test Flow",
            "version": "0.1.0",
            "steps": [
                { "id": "step1", "block": "my-block" }
            ]
        }"#;
        let flow = parse(json).unwrap();
        assert_eq!(flow.id, "test-flow");
        assert_eq!(flow.steps.len(), 1);
        assert_eq!(flow.steps[0].id, "step1");
    }

    #[test]
    fn parse_full_flow() {
        let json = r#"{
            "id": "login-flow",
            "name": "User Login",
            "version": "1.0.0",
            "description": "Authenticate a user",
            "input": {
                "type": "object",
                "properties": {
                    "email": { "type": "string" },
                    "password": { "type": "string" }
                },
                "required": ["email", "password"]
            },
            "steps": [
                {
                    "id": "find-user",
                    "block": "database/query",
                    "input": { "email": "$.input.email" }
                },
                {
                    "id": "check-password",
                    "block": "crypto/verify",
                    "input": {
                        "hash": "$.find-user.password_hash",
                        "password": "$.input.password"
                    },
                    "next": [
                        { "when": "$.check-password.match == true", "step": "create-token" },
                        { "step": "reject" }
                    ]
                },
                {
                    "id": "create-token",
                    "block": "crypto/jwt-sign",
                    "input": { "user_id": "$.find-user.id" }
                },
                {
                    "id": "reject",
                    "block": "respond/error",
                    "input": { "status": 401, "message": "Invalid credentials" }
                }
            ],
            "config": {
                "timeout_ms": 5000,
                "max_steps": 100,
                "on_error": "stop"
            }
        }"#;
        let flow = parse(json).unwrap();
        assert_eq!(flow.id, "login-flow");
        assert_eq!(flow.steps.len(), 4);
        assert!(flow.steps[1].next.is_some());
        assert_eq!(flow.steps[1].next.as_ref().unwrap().len(), 2);
        assert!(flow.config.is_some());
        assert_eq!(
            flow.config.as_ref().unwrap().timeout(),
            Some(std::time::Duration::from_millis(5000))
        );
    }

    #[test]
    fn parse_each_step() {
        let json = r#"{
            "id": "batch-flow",
            "name": "Batch",
            "version": "0.1.0",
            "steps": [
                {
                    "id": "process",
                    "block": "transform",
                    "each": "$.input.items",
                    "input": { "item": "$.each.item" }
                }
            ]
        }"#;
        let flow = parse(json).unwrap();
        assert_eq!(flow.steps[0].each.as_deref(), Some("$.input.items"));
    }

    #[test]
    fn parse_invalid_json() {
        let result = parse("not json");
        assert!(result.is_err());
    }

    #[test]
    fn parse_missing_required_fields() {
        let json = r#"{ "id": "test" }"#;
        let result = parse(json);
        assert!(result.is_err());
    }

    fn parse_config(config: &str) -> Result<WaferFlow, ParseError> {
        parse(&format!(
            r#"{{ "id": "f", "name": "F", "version": "0.1.0",
                 "steps": [ {{ "id": "a", "block": "b" }} ], "config": {config} }}"#
        ))
    }

    #[test]
    fn on_error_accepts_exactly_stop_and_continue() {
        use crate::types::OnError;
        let stop = parse_config(r#"{ "on_error": "stop" }"#).unwrap();
        assert_eq!(stop.config.unwrap().on_error(), OnError::Stop);
        let cont = parse_config(r#"{ "on_error": "continue" }"#).unwrap();
        assert_eq!(cont.config.unwrap().on_error(), OnError::Continue);
        let unset = parse_config("{}").unwrap();
        assert_eq!(unset.config.unwrap().on_error(), OnError::Stop);
    }

    #[test]
    fn on_error_other_than_stop_or_continue_fails_to_parse() {
        for bad in ["Stop", "STOP", "skip", "retry", ""] {
            let err = parse_config(&format!(r#"{{ "on_error": "{bad}" }}"#))
                .expect_err(&format!("on_error {bad:?} must not parse"));
            assert!(
                err.to_string().contains("unknown variant"),
                "{bad:?}: {err}"
            );
        }
    }

    #[test]
    fn unknown_config_key_fails_to_parse() {
        let err = parse_config(r#"{ "timout": "30s" }"#).unwrap_err();
        assert!(err.to_string().contains("unknown field `timout`"), "{err}");
    }

    #[test]
    fn timeout_string_is_parsed_and_kept_as_written() {
        use std::time::Duration;
        let flow = parse_config(r#"{ "timeout": "30s" }"#).unwrap();
        let config = flow.config.as_ref().unwrap();
        assert_eq!(config.timeout(), Some(Duration::from_secs(30)));
        let json = serde_json::to_value(&flow).unwrap();
        assert_eq!(json["config"]["timeout"], "30s");
    }

    #[test]
    fn timeout_ms_is_the_timeout_when_the_string_is_unset() {
        use std::time::Duration;
        let flow = parse_config(r#"{ "timeout_ms": 1500 }"#).unwrap();
        assert_eq!(
            flow.config.unwrap().timeout(),
            Some(Duration::from_millis(1500))
        );
        assert_eq!(parse_config("{}").unwrap().config.unwrap().timeout(), None);
    }

    #[test]
    fn malformed_or_zero_timeouts_fail_to_parse() {
        for bad in [
            r#"{ "timeout": "30 seconds" }"#,
            r#"{ "timeout": "30sec" }"#,
            r#"{ "timeout": " 30s" }"#,
            r#"{ "timeout": "-5s" }"#,
            r#"{ "timeout": "" }"#,
            r#"{ "timeout": "0s" }"#,
            r#"{ "timeout": "0" }"#,
            r#"{ "timeout_ms": 0 }"#,
            r#"{ "max_steps": 0 }"#,
        ] {
            assert!(parse_config(bad).is_err(), "{bad} must not parse");
        }
    }
}
