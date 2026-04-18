use serde_json::json;
use wafer_block::{ExternalAsset, SkillTool};

#[test]
fn skill_tool_round_trips() {
    let tool = SkillTool {
        description: "Crop, resize, transcode files via ffmpeg.".to_string(),
        parameters: json!({
            "type": "object",
            "properties": {
                "inputFileId": { "type": "string" },
                "operation":   { "type": "string", "enum": ["resize","crop","trim","transcode"] },
                "params":      { "type": "object" }
            },
            "required": ["inputFileId", "operation"]
        }),
    };

    let s = serde_json::to_string(&tool).expect("serialize");
    let back: SkillTool = serde_json::from_str(&s).expect("deserialize");

    assert_eq!(back.description, tool.description);
    assert_eq!(back.parameters, tool.parameters);
}

#[test]
fn external_asset_round_trips() {
    let asset = ExternalAsset {
        id: "ffmpeg".to_string(),
        loader: "ffmpeg.wasm".to_string(),
        version: "0.12.6".to_string(),
        url: "https://cdn.jsdelivr.net/npm/@ffmpeg/core@0.12.6/dist/umd/ffmpeg-core.wasm"
            .to_string(),
        sha256: "abc123".to_string(),
    };

    let s = serde_json::to_string(&asset).expect("serialize");
    let back: ExternalAsset = serde_json::from_str(&s).expect("deserialize");

    assert_eq!(back.id, asset.id);
    assert_eq!(back.url, asset.url);
    assert_eq!(back.sha256, asset.sha256);
}

#[test]
fn block_info_skill_fields_default_to_none() {
    use wafer_block::BlockInfo;
    let info = BlockInfo::new("foo/bar", "0.1.0", "handler@v1", "test");
    assert!(info.role.is_none());
    assert!(info.tool.is_none());
    assert!(info.external_assets.is_empty());
}

#[test]
fn block_info_skill_builder_sets_fields() {
    use wafer_block::{BlockInfo, ExternalAsset, SkillRole, SkillTool};
    let info = BlockInfo::new("gizza-ai/clock", "0.1.0", "handler@v1", "Current time")
        .role(SkillRole::Skill)
        .tool(SkillTool {
            description: "Get the current time.".to_string(),
            parameters: serde_json::json!({"type":"object","properties":{}}),
        })
        .external_assets(vec![ExternalAsset {
            id: "ffmpeg".to_string(),
            loader: "ffmpeg.wasm".to_string(),
            version: "0.12.6".to_string(),
            url: "https://example.test/ffmpeg.wasm".to_string(),
            sha256: "deadbeef".to_string(),
        }]);

    assert!(matches!(info.role, Some(SkillRole::Skill)));
    assert!(info.tool.is_some());
    assert_eq!(info.external_assets.len(), 1);
    assert_eq!(info.external_assets[0].id, "ffmpeg");
}
