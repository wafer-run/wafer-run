//! Round-trip and builder tests for BlockInfo::flow_config (new field
//! introduced by the 2026-05-15 flow-config-api spec).

use wafer_block::{BlockInfo, ConfigVar};

#[test]
fn flow_config_default_is_empty() {
    let info = BlockInfo::new("a/b", "0.1.0", "iface@v1", "summary");
    assert!(
        info.flow_config.is_empty(),
        "default flow_config should be empty"
    );
}

#[test]
fn flow_config_builder_sets_field() {
    let info =
        BlockInfo::new("a/b", "0.1.0", "iface@v1", "summary").flow_config(vec![ConfigVar::new(
            "listen",
            "address to listen on",
            "0.0.0.0:8080",
        )]);
    assert_eq!(info.flow_config.len(), 1);
    assert_eq!(info.flow_config[0].key, "listen");
    assert_eq!(info.flow_config[0].default, "0.0.0.0:8080");
}

#[test]
fn flow_config_round_trips_through_serde_json() {
    let info = BlockInfo::new("a/b", "0.1.0", "iface@v1", "summary").flow_config(vec![
        ConfigVar::new("listen", "address to listen on", "0.0.0.0:8080"),
        ConfigVar::new("routes", "route table (JSON array)", "[]"),
    ]);
    let json = serde_json::to_string(&info).expect("serialize");
    let back: BlockInfo = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back.flow_config.len(), 2);
    assert_eq!(back.flow_config[0].key, "listen");
    assert_eq!(back.flow_config[1].key, "routes");
}

#[test]
fn empty_flow_config_is_skipped_in_serialization() {
    let info = BlockInfo::new("a/b", "0.1.0", "iface@v1", "summary");
    let json = serde_json::to_string(&info).expect("serialize");
    assert!(
        !json.contains("\"flow_config\""),
        "empty flow_config should be skipped via serde(skip_serializing_if): {json}"
    );
}
