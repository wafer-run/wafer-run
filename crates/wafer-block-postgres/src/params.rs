//! Binding JSON parameter values by the type PostgreSQL gives each parameter.
//!
//! The shared executor hands every statement's parameters over as
//! [`serde_json::Value`]s. JSON has one number type and no notion of a
//! column's type, so the value alone cannot say how to bind it: a `null` for
//! an `INTEGER` column, `2` for a `DOUBLE PRECISION` column and `"…"` for a
//! `TIMESTAMPTZ` column each need the column's type, not the value's.
//!
//! So a statement is prepared first with every parameter's type left open,
//! PostgreSQL infers each one from where the parameter appears (the column an
//! `INSERT` writes, the operand a comparison is against), and each value is
//! encoded for that type. The prepared statement is cached per connection by
//! its SQL text, and every parameterized statement this backend runs is
//! prepared this way, so the cached parameter types are always the inferred
//! ones and a later execution of the same SQL encodes for the same types.
//! Binding by the value's type instead fixes the parameter types from
//! whichever values the first execution carried: a `null` bound as `text`
//! cannot be written to an `INTEGER` column at all, and once `1.5` has
//! prepared an `INSERT` with a `float8` parameter, the `int8` bytes of a later
//! `2` are read as a float.

use std::str::FromStr as _;

use base64ct::{Base64, Encoding as _};
use sqlx::{
    encode::IsNull,
    error::BoxDynError,
    postgres::{types::Oid, PgArgumentBuffer, PgArguments, PgConnection, PgTypeInfo},
    Arguments as _, Either, Executor as _, Postgres, Statement as _, TypeInfo as _,
};
use wafer_core::interfaces::database::service::DatabaseError;

use crate::errors::sqlx_error;

/// The arguments for `sql`, each of `params` encoded for the type PostgreSQL
/// infers for its parameter, preparing (and caching) `sql` on `conn`.
///
/// Execute the result on the same `conn`: it is that connection's cached
/// statement whose parameter types the arguments were encoded for.
pub(crate) async fn bind(
    conn: &mut PgConnection,
    sql: &str,
    params: &[serde_json::Value],
) -> Result<PgArguments, DatabaseError> {
    let statement = conn.prepare(sql).await.map_err(|e| sqlx_error(&e))?;
    let types = match statement.parameters() {
        Some(Either::Left(types)) => types,
        _ => &[],
    };
    if types.len() != params.len() {
        return Err(DatabaseError::Internal(format!(
            "statement has {} parameters but {} values were given",
            types.len(),
            params.len()
        )));
    }
    let mut args = PgArguments::default();
    for (index, (value, ty)) in params.iter().zip(types).enumerate() {
        add(&mut args, value, ty).map_err(|e| e.at(index + 1, ty))?;
    }
    Ok(args)
}

/// Why a value could not be bound to a parameter.
enum BindError {
    /// The value does not fit the parameter's type; the caller's mistake.
    Mismatch(String),
    /// The encoder itself failed.
    Encode(BoxDynError),
}

impl BindError {
    fn at(self, position: usize, ty: &PgTypeInfo) -> DatabaseError {
        match self {
            Self::Mismatch(why) => DatabaseError::InvalidArgument(format!(
                "parameter ${position} ({}): {why}",
                ty.name()
            )),
            Self::Encode(e) => DatabaseError::Internal(format!(
                "encode parameter ${position} ({}): {e}",
                ty.name()
            )),
        }
    }
}

fn push<'q, T>(args: &mut PgArguments, value: T) -> Result<(), BindError>
where
    T: sqlx::Encode<'q, Postgres> + sqlx::Type<Postgres> + 'q,
{
    args.add(value).map_err(BindError::Encode)
}

fn kind(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "a boolean",
        serde_json::Value::Number(_) => "a number",
        serde_json::Value::String(_) => "a string",
        serde_json::Value::Array(_) => "an array",
        serde_json::Value::Object(_) => "an object",
    }
}

fn mismatch(value: &serde_json::Value, expected: &str) -> BindError {
    BindError::Mismatch(format!("expected {expected}, got {}", kind(value)))
}

/// Encode `value` for a parameter of type `ty`.
///
/// SQL `NULL` binds to a parameter of any type. Otherwise the value must fit:
/// an integral number (or a boolean, as `1`/`0`, how the SQLite family stores
/// one) for an integer type, a number for a floating-point or `NUMERIC` type,
/// a boolean for `BOOLEAN`, and for a text type anything, as the text it
/// spells (a number or boolean as its JSON text, an object or array as its
/// JSON). A `json`/`jsonb` parameter takes any value; a string there is taken
/// as JSON text when it parses as JSON, as the SQLite family reads the text of
/// a JSON column, and as a JSON string otherwise. `TIMESTAMPTZ`, `TIMESTAMP`,
/// `DATE` and `UUID` take a string in their standard text form (RFC 3339 for
/// the timestamps), and `BYTEA` takes base64, the form it reads back in. A
/// parameter of any other type cannot be bound from JSON.
fn add(
    args: &mut PgArguments,
    value: &serde_json::Value,
    ty: &PgTypeInfo,
) -> Result<(), BindError> {
    use serde_json::Value;

    if value.is_null() {
        return push(args, TypedNull(ty.clone()));
    }
    match ty.name() {
        "INT2" => push(
            args,
            i16::try_from(integer(value)?).map_err(|_| out_of_range())?,
        ),
        "INT4" => push(
            args,
            i32::try_from(integer(value)?).map_err(|_| out_of_range())?,
        ),
        "INT8" => push(args, integer(value)?),
        "FLOAT4" => {
            #[expect(
                clippy::cast_possible_truncation,
                reason = "a REAL column holds single precision; Postgres rounds the same way"
            )]
            let f = float(value)? as f32;
            push(args, f)
        }
        "FLOAT8" => push(args, float(value)?),
        "NUMERIC" => match value {
            Value::Number(n) => push(
                args,
                sqlx::types::BigDecimal::from_str(&n.to_string())
                    .map_err(|e| BindError::Mismatch(format!("not a decimal: {e}")))?,
            ),
            other => Err(mismatch(other, "a number")),
        },
        "BOOL" => match value {
            Value::Bool(b) => push(args, *b),
            other => Err(mismatch(other, "a boolean")),
        },
        "TEXT" | "VARCHAR" | "CHAR" | "NAME" => match value {
            Value::String(s) => push(args, s.clone()),
            other => push(args, other.to_string()),
        },
        "JSON" | "JSONB" => match value {
            Value::String(s) => push(
                args,
                serde_json::from_str::<Value>(s).unwrap_or_else(|_| value.clone()),
            ),
            other => push(args, other.clone()),
        },
        "TIMESTAMPTZ" => {
            let s = string(value)?;
            let at = chrono::DateTime::parse_from_rfc3339(s)
                .map_err(|e| BindError::Mismatch(format!("not an RFC 3339 timestamp: {e}")))?;
            push(args, at.with_timezone(&chrono::Utc))
        }
        "TIMESTAMP" => {
            let s = string(value)?;
            let at = chrono::DateTime::parse_from_rfc3339(s)
                .map(|at| at.naive_utc())
                .or_else(|_| chrono::NaiveDateTime::from_str(s))
                .map_err(|e| BindError::Mismatch(format!("not a timestamp: {e}")))?;
            push(args, at)
        }
        "DATE" => {
            let day = chrono::NaiveDate::from_str(string(value)?)
                .map_err(|e| BindError::Mismatch(format!("not a date: {e}")))?;
            push(args, day)
        }
        "UUID" => {
            let id = uuid::Uuid::parse_str(string(value)?)
                .map_err(|e| BindError::Mismatch(format!("not a UUID: {e}")))?;
            push(args, id)
        }
        "BYTEA" => {
            let bytes = Base64::decode_vec(string(value)?)
                .map_err(|e| BindError::Mismatch(format!("not base64: {e}")))?;
            push(args, bytes)
        }
        other => Err(BindError::Mismatch(format!(
            "a {other} parameter cannot be bound from JSON"
        ))),
    }
}

fn out_of_range() -> BindError {
    BindError::Mismatch("integer out of range".to_string())
}

/// The integer a JSON value names: an integral number, or a boolean as
/// `1`/`0`. A fractional number is refused rather than rounded.
fn integer(value: &serde_json::Value) -> Result<i64, BindError> {
    match value {
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                return Ok(i);
            }
            match n.as_f64() {
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "the value is integral and inside the i64 range, checked just before"
                )]
                Some(f) if f.fract() == 0.0 && f >= i64::MIN as f64 && f < i64::MAX as f64 => {
                    Ok(f as i64)
                }
                Some(_) if n.is_u64() => Err(out_of_range()),
                _ => Err(BindError::Mismatch(format!("expected an integer, got {n}"))),
            }
        }
        serde_json::Value::Bool(b) => Ok(i64::from(*b)),
        other => Err(mismatch(other, "an integer")),
    }
}

fn float(value: &serde_json::Value) -> Result<f64, BindError> {
    match value {
        serde_json::Value::Number(n) => n
            .as_f64()
            .ok_or_else(|| BindError::Mismatch(format!("not a float: {n}"))),
        other => Err(mismatch(other, "a number")),
    }
}

fn string(value: &serde_json::Value) -> Result<&str, BindError> {
    value.as_str().ok_or_else(|| mismatch(value, "a string"))
}

/// SQL `NULL` bound as the parameter's own type, so a statement prepared
/// without this cache (a connection configured with no statement cache
/// re-prepares with the argument types) still gets the inferred type.
struct TypedNull(PgTypeInfo);

impl sqlx::Type<Postgres> for TypedNull {
    fn type_info() -> PgTypeInfo {
        // `unknown`: never used, since `produces` always names the type.
        PgTypeInfo::with_oid(Oid(705))
    }
}

impl sqlx::Encode<'_, Postgres> for TypedNull {
    fn encode_by_ref(&self, _buf: &mut PgArgumentBuffer) -> Result<IsNull, BoxDynError> {
        Ok(IsNull::Yes)
    }

    fn produces(&self) -> Option<PgTypeInfo> {
        Some(self.0.clone())
    }
}
