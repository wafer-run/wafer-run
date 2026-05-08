//! Wire-format types for the database service.
//!
//! Mirrors `crates/wafer-core/src/interfaces/database/handler.rs` and
//! `crates/wafer-core/src/clients/database.rs`. `Record` and `RecordList`
//! match the runtime types in `interfaces::database::service`.
//!
//! Filter values and record fields are JSON-typed (`serde_json::Value`).
//! BLOB columns flow through as `serde_json::Value::Array` of integers
//! today — there is no dedicated `Vec<u8>` field on the wire — so this
//! module does not include a no-inflation test.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

// --- Filter / sort sub-types ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilterDef {
    pub field: String,
    #[serde(default = "default_operator")]
    pub operator: String,
    #[serde(default)]
    pub value: serde_json::Value,
}

fn default_operator() -> String {
    "eq".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SortFieldDef {
    pub field: String,
    #[serde(default)]
    pub desc: bool,
}

// --- Requests ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetRequest {
    pub collection: String,
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListRequest {
    pub collection: String,
    #[serde(default)]
    pub filters: Vec<FilterDef>,
    #[serde(default)]
    pub sort: Vec<SortFieldDef>,
    #[serde(default)]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateRequest {
    pub collection: String,
    pub data: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateRequest {
    pub collection: String,
    pub id: String,
    pub data: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteRequest {
    pub collection: String,
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CountRequest {
    pub collection: String,
    #[serde(default)]
    pub filters: Vec<FilterDef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SumRequest {
    pub collection: String,
    pub field: String,
    #[serde(default)]
    pub filters: Vec<FilterDef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryRawRequest {
    pub query: String,
    #[serde(default)]
    pub args: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecRawRequest {
    pub query: String,
    #[serde(default)]
    pub args: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteWhereRequest {
    pub collection: String,
    #[serde(default)]
    pub filters: Vec<FilterDef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteWhereCountRequest {
    pub collection: String,
    #[serde(default)]
    pub filters: Vec<FilterDef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TakeWhereRequest {
    pub collection: String,
    #[serde(default)]
    pub filters: Vec<FilterDef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateWhereRequest {
    pub collection: String,
    #[serde(default)]
    pub filters: Vec<FilterDef>,
    pub data: HashMap<String, serde_json::Value>,
}

// --- Responses ---

/// Single record returned by `get`, `create`, `update`. Matches
/// `interfaces::database::service::Record`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub id: String,
    pub data: HashMap<String, serde_json::Value>,
}

/// Paginated list of records returned by `list`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordList {
    pub records: Vec<Record>,
    pub total_count: i64,
    pub page: i64,
    pub page_size: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CountResponse {
    pub count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SumResponse {
    pub sum: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteWhereCountResponse {
    pub count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TakeWhereResponse {
    pub records: Vec<Record>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecRawResponse {
    pub rows_affected: i64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec;

    // -----------------------------------------------------------------------
    // Round-trip tests
    // -----------------------------------------------------------------------

    #[test]
    fn get_request_round_trips() {
        let original = GetRequest {
            collection: "users".into(),
            id: "u1".into(),
        };
        let encoded = codec::encode(&original).expect("encode");
        let decoded: GetRequest = codec::decode(&encoded).expect("decode");
        assert_eq!(decoded.collection, original.collection);
        assert_eq!(decoded.id, original.id);
    }

    #[test]
    fn list_request_round_trips() {
        let original = ListRequest {
            collection: "users".into(),
            filters: vec![FilterDef {
                field: "active".into(),
                operator: "eq".into(),
                value: serde_json::json!(true),
            }],
            sort: vec![SortFieldDef {
                field: "created_at".into(),
                desc: true,
            }],
            limit: 50,
            offset: 100,
        };
        let encoded = codec::encode(&original).expect("encode");
        let decoded: ListRequest = codec::decode(&encoded).expect("decode");
        assert_eq!(decoded.collection, original.collection);
        assert_eq!(decoded.limit, 50);
        assert_eq!(decoded.offset, 100);
        assert_eq!(decoded.filters.len(), 1);
        assert_eq!(decoded.sort.len(), 1);
        assert!(decoded.sort[0].desc);
    }

    #[test]
    fn create_request_round_trips() {
        let mut data = HashMap::new();
        data.insert("name".into(), serde_json::json!("Alice"));
        let original = CreateRequest {
            collection: "users".into(),
            data,
        };
        let encoded = codec::encode(&original).expect("encode");
        let decoded: CreateRequest = codec::decode(&encoded).expect("decode");
        assert_eq!(decoded.collection, original.collection);
        assert_eq!(decoded.data.get("name"), Some(&serde_json::json!("Alice")));
    }

    #[test]
    fn record_round_trips() {
        let mut data = HashMap::new();
        data.insert("k".into(), serde_json::json!("v"));
        let original = Record {
            id: "r1".into(),
            data,
        };
        let encoded = codec::encode(&original).expect("encode");
        let decoded: Record = codec::decode(&encoded).expect("decode");
        assert_eq!(decoded.id, original.id);
        assert_eq!(decoded.data.get("k"), Some(&serde_json::json!("v")));
    }

    #[test]
    fn record_list_round_trips() {
        let original = RecordList {
            records: vec![Record {
                id: "r1".into(),
                data: HashMap::new(),
            }],
            total_count: 1,
            page: 1,
            page_size: 20,
        };
        let encoded = codec::encode(&original).expect("encode");
        let decoded: RecordList = codec::decode(&encoded).expect("decode");
        assert_eq!(decoded.records.len(), 1);
        assert_eq!(decoded.total_count, 1);
        assert_eq!(decoded.page, 1);
        assert_eq!(decoded.page_size, 20);
    }

    #[test]
    fn query_raw_request_round_trips() {
        let original = QueryRawRequest {
            query: "SELECT 1".into(),
            args: vec![serde_json::json!(1), serde_json::json!("x")],
        };
        let encoded = codec::encode(&original).expect("encode");
        let decoded: QueryRawRequest = codec::decode(&encoded).expect("decode");
        assert_eq!(decoded.query, original.query);
        assert_eq!(decoded.args.len(), 2);
    }

    // -----------------------------------------------------------------------
    // Schema-lock tests
    // -----------------------------------------------------------------------

    #[test]
    fn schema_lock_get_request() {
        let req = GetRequest {
            collection: String::new(),
            id: String::new(),
        };
        let encoded = codec::encode(&req).expect("encode");
        let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex, "82aa636f6c6c656374696f6ea0a26964a0",
            "GetRequest schema changed — review consumer impact before updating this literal"
        );
    }

    #[test]
    fn schema_lock_list_request() {
        let req = ListRequest {
            collection: String::new(),
            filters: vec![],
            sort: vec![],
            limit: 0,
            offset: 0,
        };
        let encoded = codec::encode(&req).expect("encode");
        let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex, "85aa636f6c6c656374696f6ea0a766696c7465727390a4736f727490a56c696d697400a66f666673657400",
            "ListRequest schema changed — review consumer impact before updating this literal"
        );
    }

    #[test]
    fn schema_lock_record() {
        let r = Record {
            id: String::new(),
            data: HashMap::new(),
        };
        let encoded = codec::encode(&r).expect("encode");
        let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex, "82a26964a0a46461746180",
            "Record schema changed — review consumer impact before updating this literal"
        );
    }

    #[test]
    fn schema_lock_record_list() {
        let rl = RecordList {
            records: vec![],
            total_count: 0,
            page: 0,
            page_size: 0,
        };
        let encoded = codec::encode(&rl).expect("encode");
        let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex, "84a77265636f72647390ab746f74616c5f636f756e7400a47061676500a9706167655f73697a6500",
            "RecordList schema changed — review consumer impact before updating this literal"
        );
    }

    #[test]
    fn schema_lock_count_response() {
        let r = CountResponse { count: 0 };
        let encoded = codec::encode(&r).expect("encode");
        let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex, "81a5636f756e7400",
            "CountResponse schema changed — review consumer impact before updating this literal"
        );
    }

    #[test]
    fn schema_lock_exec_raw_response() {
        let r = ExecRawResponse { rows_affected: 0 };
        let encoded = codec::encode(&r).expect("encode");
        let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex, "81ad726f77735f616666656374656400",
            "ExecRawResponse schema changed — review consumer impact before updating this literal"
        );
    }

    #[test]
    fn schema_lock_delete_where_count_request() {
        let req = DeleteWhereCountRequest {
            collection: String::new(),
            filters: vec![],
        };
        let encoded = codec::encode(&req).expect("encode");
        let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex, "82aa636f6c6c656374696f6ea0a766696c7465727390",
            "DeleteWhereCountRequest schema changed — review consumer impact before updating this literal"
        );
    }

    #[test]
    fn schema_lock_take_where_request() {
        let req = TakeWhereRequest {
            collection: String::new(),
            filters: vec![],
        };
        let encoded = codec::encode(&req).expect("encode");
        let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex, "82aa636f6c6c656374696f6ea0a766696c7465727390",
            "TakeWhereRequest schema changed — review consumer impact before updating this literal"
        );
    }

    #[test]
    fn schema_lock_delete_where_count_response() {
        let r = DeleteWhereCountResponse { count: 0 };
        let encoded = codec::encode(&r).expect("encode");
        let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex, "81a5636f756e7400",
            "DeleteWhereCountResponse schema changed — review consumer impact before updating this literal"
        );
    }

    #[test]
    fn schema_lock_take_where_response() {
        let r = TakeWhereResponse { records: vec![] };
        let encoded = codec::encode(&r).expect("encode");
        let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex, "81a77265636f72647390",
            "TakeWhereResponse schema changed — review consumer impact before updating this literal"
        );
    }
}
