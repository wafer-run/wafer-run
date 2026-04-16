use crate::{error::ParseError, types::WaferFlow};

/// Parse a JSON string into a WaferFlow definition.
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
        assert_eq!(flow.config.as_ref().unwrap().timeout_ms, Some(5000));
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
}
