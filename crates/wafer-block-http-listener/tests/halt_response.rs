//! Integration test for Wave 13 PR A: a flow that emits Halt must surface
//! on the HTTP wire as a real response with status + headers + body
//! (semantically identical to Ok(buf) at the wire; distinct at the executor).

use http_body_util::BodyExt;
use wafer_block::{core_types::MetaEntry, streams::output::OutputStream};
use wafer_block_http_listener::wafer_output_to_response;

#[tokio::test]
async fn halt_propagates_to_http_listener_as_204_with_headers() {
    let stream = OutputStream::halt(
        Vec::new(),
        vec![
            MetaEntry {
                key: "resp.status".into(),
                value: "204".into(),
            },
            MetaEntry {
                key: "resp.header.Access-Control-Allow-Origin".into(),
                value: "https://example.com".into(),
            },
        ],
    );

    let response = wafer_output_to_response(stream).await;

    assert_eq!(response.status().as_u16(), 204);
    assert_eq!(
        response
            .headers()
            .get("access-control-allow-origin")
            .expect("ACAO header present")
            .to_str()
            .unwrap(),
        "https://example.com",
    );
    let body_bytes = response.into_body().collect().await.unwrap().to_bytes();
    assert!(body_bytes.is_empty(), "204 body should be empty");
}

#[tokio::test]
async fn halt_with_body_propagates_as_200_default() {
    let stream = OutputStream::halt(
        b"hello".to_vec(),
        vec![MetaEntry {
            key: "resp.header.X-Custom".into(),
            value: "yes".into(),
        }],
    );

    let response = wafer_output_to_response(stream).await;

    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(
        response
            .headers()
            .get("x-custom")
            .expect("custom header present")
            .to_str()
            .unwrap(),
        "yes",
    );
    let body_bytes = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(body_bytes.as_ref(), b"hello");
}
