use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderMap, Method, StatusCode};
use axum::Router;
use std::collections::HashMap;
use std::sync::Arc;

use crate::meta::*;
use crate::runtime::Wafer;
use crate::types::*;

/// Convert an HTTP method to a semantic request action.
fn http_method_to_action(method: &Method) -> &'static str {
    match *method {
        Method::GET | Method::HEAD => "retrieve",
        Method::POST => "create",
        Method::PUT | Method::PATCH => "update",
        Method::DELETE => "delete",
        _ => "execute",
    }
}

/// Convert an HTTP request into a WAFER Message.
async fn http_to_message(
    method: Method,
    uri_path: &str,
    raw_query: &str,
    headers: &HeaderMap,
    remote_addr: &str,
    body: Vec<u8>,
) -> Message {
    let mut meta = HashMap::new();

    // HTTP-specific meta
    meta.insert("http.method".to_string(), method.to_string());
    meta.insert("http.path".to_string(), uri_path.to_string());
    meta.insert("http.raw_query".to_string(), raw_query.to_string());
    meta.insert("http.remote_addr".to_string(), remote_addr.to_string());
    meta.insert(
        "http.content_type".to_string(),
        headers
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string(),
    );
    meta.insert(
        "http.host".to_string(),
        headers
            .get("host")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string(),
    );

    // Normalized request meta
    meta.insert(
        META_REQ_ACTION.to_string(),
        http_method_to_action(&method).to_string(),
    );
    meta.insert(META_REQ_RESOURCE.to_string(), uri_path.to_string());
    meta.insert(META_REQ_CLIENT_IP.to_string(), remote_addr.to_string());
    meta.insert(
        META_REQ_CONTENT_TYPE.to_string(),
        headers
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string(),
    );

    // Copy headers to meta
    for (name, value) in headers {
        if let Ok(v) = value.to_str() {
            meta.insert(format!("http.header.{}", name), v.to_string());
        }
    }

    // Copy URL query params to meta
    if !raw_query.is_empty() {
        for pair in raw_query.split('&') {
            let mut parts = pair.splitn(2, '=');
            if let (Some(key), Some(val)) = (parts.next(), parts.next()) {
                let decoded_val = urlencoding_decode(val);
                meta.insert(format!("http.query.{}", key), decoded_val.clone());
                meta.insert(format!("{}{}", META_REQ_QUERY_PREFIX, key), decoded_val);
            }
        }
    }

    Message {
        kind: format!("{}:{}", method, uri_path),
        data: body,
        meta,
    }
}

fn urlencoding_decode(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.bytes();
    while let Some(b) = chars.next() {
        if b == b'+' {
            result.push(' ');
        } else if b == b'%' {
            let h1 = chars.next().and_then(|c| (c as char).to_digit(16));
            let h2 = chars.next().and_then(|c| (c as char).to_digit(16));
            if let (Some(h1), Some(h2)) = (h1, h2) {
                result.push((h1 * 16 + h2) as u8 as char);
            }
        } else {
            result.push(b as char);
        }
    }
    result
}

/// Apply response meta keys as HTTP response headers.
fn apply_response_meta(
    builder: axum::http::response::Builder,
    meta: &HashMap<String, String>,
) -> axum::http::response::Builder {
    let mut builder = builder;
    for (k, v) in meta {
        match k.as_str() {
            k if k == META_RESP_STATUS || k == "http.status" => continue,
            k if k.starts_with(META_RESP_COOKIE_PREFIX)
                || k.starts_with("http.resp.set-cookie.") =>
            {
                builder = builder.header("Set-Cookie", v);
            }
            k if k.starts_with(META_RESP_HEADER_PREFIX) => {
                let header_name = &k[META_RESP_HEADER_PREFIX.len()..];
                builder = builder.header(header_name, v);
            }
            k if k.starts_with("http.resp.header.") => {
                let header_name = &k[17..];
                builder = builder.header(header_name, v);
            }
            k if k == META_RESP_CONTENT_TYPE || k == "Content-Type" => {
                builder = builder.header("Content-Type", v);
            }
            _ => {}
        }
    }
    builder
}

/// Extract status code from meta.
fn get_status_code(meta: &HashMap<String, String>, default_code: u16) -> u16 {
    if let Some(code) = meta.get(META_RESP_STATUS) {
        if let Ok(n) = code.parse::<u16>() {
            return n;
        }
    }
    if let Some(code) = meta.get("http.status") {
        if let Ok(n) = code.parse::<u16>() {
            return n;
        }
    }
    default_code
}

/// Convert a WAFER Result to an HTTP response.
fn wafer_result_to_response(result: Result_) -> axum::http::Response<Body> {
    match result.action {
        Action::Respond => {
            let empty_meta = HashMap::new();
            let resp_meta = result
                .response
                .as_ref()
                .map(|r| &r.meta)
                .unwrap_or(&empty_meta);

            let status_code = get_status_code(resp_meta, 200);
            let mut builder = axum::http::Response::builder()
                .status(StatusCode::from_u16(status_code).unwrap_or(StatusCode::OK));

            builder = apply_response_meta(builder, resp_meta);

            if let Some(ref msg) = result.message {
                builder = apply_response_meta(builder, &msg.meta);
            }

            // Set default content type
            let has_ct = resp_meta.contains_key(META_RESP_CONTENT_TYPE)
                || resp_meta.contains_key("Content-Type");
            if !has_ct {
                builder = builder.header("Content-Type", "application/json");
            }

            let body = result
                .response
                .map(|r| r.data)
                .unwrap_or_default();

            builder.body(Body::from(body)).unwrap()
        }

        Action::Error => {
            let empty_meta = HashMap::new();
            let err_meta = result
                .error
                .as_ref()
                .map(|e| &e.meta)
                .unwrap_or(&empty_meta);

            let status_code = get_status_code(err_meta, 500);
            let mut builder = axum::http::Response::builder()
                .status(StatusCode::from_u16(status_code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR));

            builder = apply_response_meta(builder, err_meta);

            if let Some(ref msg) = result.message {
                builder = apply_response_meta(builder, &msg.meta);
            }

            builder = builder.header("Content-Type", "application/json");

            let body = if let Some(ref err) = result.error {
                serde_json::json!({
                    "error": err.code,
                    "message": err.message,
                })
                .to_string()
            } else {
                "{}".to_string()
            };

            builder.body(Body::from(body)).unwrap()
        }

        Action::Drop => {
            let mut builder =
                axum::http::Response::builder().status(StatusCode::NO_CONTENT);

            if let Some(ref msg) = result.message {
                builder = apply_response_meta(builder, &msg.meta);
            }

            builder.body(Body::empty()).unwrap()
        }

        Action::Continue => {
            let mut builder = axum::http::Response::builder().status(StatusCode::OK);

            if let Some(ref msg) = result.message {
                builder = apply_response_meta(builder, &msg.meta);
            }

            builder = builder.header("Content-Type", "application/json");

            let body = result
                .message
                .map(|m| m.data)
                .unwrap_or_default();

            builder.body(Body::from(body)).unwrap()
        }
    }
}

/// Create an axum handler that converts HTTP requests to WAFER messages.
pub fn wafer_handler(
    wafer: Arc<Wafer>,
    chain_id: String,
) -> axum::routing::MethodRouter {
    let w = wafer.clone();
    let cid = chain_id.clone();

    axum::routing::any(move |req: Request| {
        let w = w.clone();
        let cid = cid.clone();
        async move {
            let (parts, body) = req.into_parts();
            const MAX_BODY_SIZE: usize = 10 * 1024 * 1024; // 10 MB
            let body_bytes = axum::body::to_bytes(body, MAX_BODY_SIZE)
                .await
                .unwrap_or_default()
                .to_vec();

            let uri = &parts.uri;
            let path = uri.path();
            let query = uri.query().unwrap_or("");
            let remote_addr = parts
                .headers
                .get("x-forwarded-for")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("unknown")
                .to_string();

            let mut msg =
                http_to_message(parts.method, path, query, &parts.headers, &remote_addr, body_bytes)
                    .await;

            let result = w.execute(&cid, &mut msg);
            wafer_result_to_response(result)
        }
    })
}

/// AutoRegister reads all chains with HTTP route declarations and registers axum routes.
pub fn auto_register(wafer: Arc<Wafer>) -> Router {
    let mut router = Router::new();

    let chains = wafer.chains_with_http();
    for chain in chains {
        let chain_id = chain.id.clone();

        if let Some(ref http_def) = chain.http {
            for route in &http_def.routes {
                let handler = wafer_handler(wafer.clone(), chain_id.clone());

                // In axum 0.8, path parameters use {param} syntax (same as wafer)
                // and wildcards use {*rest} syntax
                let axum_path = route.path.clone();

                if route.path_prefix {
                    // For path prefix, add a wildcard using axum 0.8 syntax
                    let prefix_path = if axum_path.ends_with('/') {
                        format!("{}{{*rest}}", axum_path)
                    } else {
                        format!("{}/{{*rest}}", axum_path)
                    };
                    router = router.route(&prefix_path, handler.clone());
                    router = router.route(&axum_path, handler);
                } else {
                    router = router.route(&axum_path, handler);
                }
            }
        }
    }

    router
}
