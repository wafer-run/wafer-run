use sea_query::Value;

/// Convert sea_query Values back to serde_json Values for use with the database client API.
pub fn sea_values_to_json(values: Vec<Value>) -> Vec<serde_json::Value> {
    values.into_iter().map(sea_value_to_json).collect()
}

/// Convert a single sea_query Value to a serde_json Value.
pub fn sea_value_to_json(v: Value) -> serde_json::Value {
    match v {
        Value::Bool(Some(b)) => serde_json::Value::Bool(b),
        Value::TinyInt(Some(i)) => serde_json::json!(i),
        Value::SmallInt(Some(i)) => serde_json::json!(i),
        Value::Int(Some(i)) => serde_json::json!(i),
        Value::BigInt(Some(i)) => serde_json::json!(i),
        Value::TinyUnsigned(Some(i)) => serde_json::json!(i),
        Value::SmallUnsigned(Some(i)) => serde_json::json!(i),
        Value::Unsigned(Some(i)) => serde_json::json!(i),
        Value::BigUnsigned(Some(i)) => serde_json::json!(i),
        Value::Float(Some(f)) => serde_json::json!(f),
        Value::Double(Some(f)) => serde_json::json!(f),
        Value::String(Some(s)) => serde_json::Value::String(*s),
        Value::Json(Some(j)) => *j,
        _ => serde_json::Value::Null,
    }
}

/// Convert a serde_json::Value to a sea_query::Value for use as a query parameter.
pub fn json_to_sea_value(v: &serde_json::Value) -> Value {
    match v {
        serde_json::Value::Null => Value::String(None),
        serde_json::Value::Bool(b) => Value::Bool(Some(*b)),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::BigInt(Some(i))
            } else if let Some(f) = n.as_f64() {
                Value::Double(Some(f))
            } else {
                Value::String(Some(Box::new(n.to_string())))
            }
        }
        serde_json::Value::String(s) => Value::String(Some(Box::new(s.clone()))),
        // Arrays and objects are carried as JSON values so each backend keeps
        // its own bind policy (SQLite serialises to JSON text, Postgres binds
        // JSONB) — `sea_value_to_json` returns the original value unchanged.
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
            Value::Json(Some(Box::new(v.clone())))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_null() {
        assert_eq!(
            json_to_sea_value(&serde_json::Value::Null),
            Value::String(None)
        );
    }

    #[test]
    fn test_bool() {
        assert_eq!(
            json_to_sea_value(&serde_json::Value::Bool(true)),
            Value::Bool(Some(true))
        );
    }

    #[test]
    fn test_integer() {
        assert_eq!(
            json_to_sea_value(&serde_json::json!(42)),
            Value::BigInt(Some(42))
        );
    }

    #[test]
    fn test_float() {
        assert_eq!(
            json_to_sea_value(&serde_json::json!(2.5)),
            Value::Double(Some(2.5))
        );
    }

    #[test]
    fn test_string() {
        assert_eq!(
            json_to_sea_value(&serde_json::json!("hello")),
            Value::String(Some(Box::new("hello".to_string())))
        );
    }

    #[test]
    fn test_array() {
        let v = serde_json::json!([1, 2, 3]);
        assert_eq!(
            json_to_sea_value(&v),
            Value::Json(Some(Box::new(v.clone())))
        );
    }

    #[test]
    fn test_object_roundtrips_unchanged() {
        // Objects/arrays must survive Statement params unchanged so each
        // backend applies its own bind policy (TEXT on SQLite, JSONB on
        // Postgres) — collapsing to a JSON-text string here would force a
        // TEXT bind on every backend.
        let v = serde_json::json!({"key": "val", "n": 1});
        assert_eq!(sea_value_to_json(json_to_sea_value(&v)), v);
        let a = serde_json::json!([1, "two", null]);
        assert_eq!(sea_value_to_json(json_to_sea_value(&a)), a);
    }

    #[test]
    fn test_sea_value_to_json_string() {
        let v = Value::String(Some(Box::new("hello".to_string())));
        assert_eq!(sea_value_to_json(v), serde_json::json!("hello"));
    }

    #[test]
    fn test_sea_value_to_json_bigint() {
        let v = Value::BigInt(Some(42));
        assert_eq!(sea_value_to_json(v), serde_json::json!(42));
    }

    #[test]
    fn test_sea_value_to_json_null() {
        assert_eq!(
            sea_value_to_json(Value::String(None)),
            serde_json::Value::Null
        );
    }

    #[test]
    fn test_sea_values_to_json_roundtrip() {
        let original = [
            serde_json::json!("hello"),
            serde_json::json!(42),
            serde_json::json!(true),
            serde_json::Value::Null,
        ];
        let sea_vals: Vec<Value> = original.iter().map(json_to_sea_value).collect();
        let back = sea_values_to_json(sea_vals);
        assert_eq!(back[0], serde_json::json!("hello"));
        assert_eq!(back[1], serde_json::json!(42));
        assert_eq!(back[2], serde_json::json!(true));
        assert_eq!(back[3], serde_json::Value::Null);
    }
}
