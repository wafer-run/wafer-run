//! Shared message handler logic for the network block.

use wafer_block::{
    codec,
    common::{ErrorCode, ServiceOp},
    streams::output::OutputStream,
    types::ResourceType,
    wire::network::{Request as WireRequest, ResponseHeader},
    *,
};

use super::service::{NetworkError, NetworkService, Request, Response, ResponseHead};
use crate::interfaces::handler_util::{decode_and_authorize, stream_with_header};

/// Maximum number of redirects the handler follows for one request before it
/// fails with `Unavailable`. Bounds redirect loops and amplification.
pub const MAX_REDIRECT_HOPS: usize = 10;

// --- Helpers ---

fn network_error_to_wafer(e: NetworkError) -> WaferError {
    match e {
        NetworkError::RequestError(msg) => WaferError::new(ErrorCode::Unavailable, msg),
        NetworkError::Other(msg) => WaferError::new(ErrorCode::Internal, msg),
    }
}

/// Request headers dropped when a redirect leaves the origin (scheme, host,
/// effective port) of the request it answers, so credentials meant for one
/// server are never replayed to another.
const CROSS_ORIGIN_STRIPPED_HEADERS: &[&str] = &[
    "authorization",
    "proxy-authorization",
    "cookie",
    "cookie2",
    "www-authenticate",
];

/// Request headers that describe the body; dropped when a redirect turns the
/// request into a body-less `GET`.
const BODY_HEADERS: &[&str] = &[
    "content-type",
    "content-length",
    "content-encoding",
    "transfer-encoding",
];

/// The request to issue next when `status` + `headers` answer `prev` with a
/// redirect the handler follows, or `None` when the response is final.
///
/// Followed: 301, 302, 303, 307 and 308 carrying exactly one `Location` that
/// resolves (relative to `prev.url`) to an absolute URL. Any other response —
/// including a 3xx without a usable `Location` — is returned to the caller as
/// is. The rewrite matches what HTTP clients do: 301/302/303 turn any method
/// but `GET`/`HEAD` into a body-less `GET`; 307/308 replay the method and body.
/// Leaving the origin drops [`CROSS_ORIGIN_STRIPPED_HEADERS`].
///
/// This only builds the next request. Whether the caller may reach it is the
/// grant check the handler runs on the returned URL before issuing it.
fn redirect_request(
    prev: &Request,
    status: u16,
    headers: &std::collections::HashMap<String, Vec<String>>,
) -> Option<Request> {
    let rewrite_to_get = match status {
        301..=303 => true,
        307 | 308 => false,
        _ => return None,
    };
    let mut locations = headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("location"))
        .flat_map(|(_, values)| values.iter());
    let location = locations.next()?;
    if locations.next().is_some() {
        return None;
    }
    let prev_url = url::Url::parse(&prev.url).ok()?;
    let next_url = prev_url.join(location).ok()?;

    let mut next = prev.clone();
    next.url = next_url.to_string();
    if rewrite_to_get {
        if !next.method.eq_ignore_ascii_case("GET") && !next.method.eq_ignore_ascii_case("HEAD") {
            next.method = "GET".to_string();
        }
        next.body = None;
        next.headers
            .retain(|name, _| !BODY_HEADERS.iter().any(|h| name.eq_ignore_ascii_case(h)));
    }
    if next_url.origin() != prev_url.origin() {
        next.headers.retain(|name, _| {
            !CROSS_ORIGIN_STRIPPED_HEADERS
                .iter()
                .any(|h| name.eq_ignore_ascii_case(h))
        });
    }
    Some(next)
}

/// Authorize one redirect hop exactly as the handler authorizes the first
/// request — `(url, Network, Read)` — and count it against
/// [`MAX_REDIRECT_HOPS`]. `hops` is the number of redirects already followed.
fn authorize_hop(ctx: &dyn Context, next: &Request, hops: usize) -> Result<(), WaferError> {
    if hops >= MAX_REDIRECT_HOPS {
        return Err(WaferError::new(
            ErrorCode::Unavailable,
            format!("too many redirects (limit {MAX_REDIRECT_HOPS})"),
        ));
    }
    ctx.check_resource_access(&next.url, ResourceType::Network, ResourceAccess::Read)
}

/// Issue `request` through `service`, following redirects hop by hop, within
/// the service's [`buffered_deadline`](NetworkService::buffered_deadline) for
/// the whole chain.
///
/// The service never follows a redirect itself (see [`NetworkService`]); each
/// hop comes back here, is authorized with [`authorize_hop`] — the same grant
/// check the first URL passed — and is issued as a new service call, so the
/// service's own per-request gates (SSRF on native) run on every hop too.
async fn do_request_following(
    service: &dyn NetworkService,
    ctx: &dyn Context,
    request: Request,
) -> Result<Response, WaferError> {
    let chain = std::pin::pin!(follow_buffered(service, ctx, request));
    let deadline = std::pin::pin!(service.buffered_deadline());
    match futures::future::select(chain, deadline).await {
        futures::future::Either::Left((result, _)) => result,
        futures::future::Either::Right(((), _)) => Err(WaferError::new(
            ErrorCode::Unavailable,
            "request timed out (the total covers every redirect hop)",
        )),
    }
}

/// The redirect loop of [`do_request_following`], without the deadline.
async fn follow_buffered(
    service: &dyn NetworkService,
    ctx: &dyn Context,
    mut request: Request,
) -> Result<Response, WaferError> {
    let mut hops = 0;
    loop {
        let resp = service
            .do_request(&request)
            .await
            .map_err(network_error_to_wafer)?;
        let Some(next) = redirect_request(&request, resp.status_code, &resp.headers) else {
            return Ok(resp);
        };
        authorize_hop(ctx, &next, hops)?;
        hops += 1;
        request = next;
    }
}

/// [`do_request_following`] for the streaming op. The body stream of every
/// redirect response is dropped unread, which cancels its producer.
async fn do_request_streaming_following(
    service: &dyn NetworkService,
    ctx: &dyn Context,
    mut request: Request,
) -> Result<(ResponseHead, OutputStream), WaferError> {
    let mut hops = 0;
    loop {
        let (head, body) = service
            .do_request_streaming(&request)
            .await
            .map_err(network_error_to_wafer)?;
        let Some(next) = redirect_request(&request, head.status_code, &head.headers) else {
            return Ok((head, body));
        };
        drop(body);
        authorize_hop(ctx, &next, hops)?;
        hops += 1;
        request = next;
    }
}

/// Handle a network message by delegating to the given service.
///
/// `ctx` is the trusted host-side authorization surface: `NETWORK_DO_REQUEST`
/// and `NETWORK_DO_REQUEST_STREAMING` both authorize via
/// [`decode_and_authorize`], which bundles the codec decode with a call to
/// `ctx.check_resource_access` so the arm cannot obtain its typed request
/// without also being checked. `is_write` is deliberately `false` for both —
/// outbound HTTP requests aren't a WRAP write in the resource sense, and
/// flipping it would regress read-only network grants. The streaming op uses
/// the identical `(url, Network, read)` authorization tuple as the buffered
/// op, so it can never be reached with a weaker grant.
///
/// Redirects are followed HERE, not by the service: a 3xx the service returns
/// is turned into the next request, whose URL must pass the same
/// `(url, Network, Read)` check before it is issued, up to
/// [`MAX_REDIRECT_HOPS`]. A hop the caller's grant does not cover fails the
/// request with the check's error; the redirect target is never contacted.
/// Without this, a granted URL that redirects would hand the caller a body
/// from anywhere.
///
/// SSRF protection is NOT included here — it is platform-specific and lives in
/// the service (the native `HttpNetworkService` gates every call). Because
/// every hop is a new service call, that gate runs on every hop.
///
/// Wire protocol: the request is a single MessagePack-encoded
/// [`wire::network::Request`]. The response is emitted as **two frames** on
/// the OutputStream — a [`wire::network::ResponseHeader`] chunk followed by a
/// body chunk. The body chunk is omitted entirely when the body is empty
/// (zero chunks → empty body on the consumer side). `NETWORK_DO_REQUEST`
/// buffers the whole body first; `NETWORK_DO_REQUEST_STREAMING` forwards the
/// service's `do_request_streaming` body chunks verbatim as they arrive,
/// under the same two-frame shape.
///
/// Both ops emit a [`wafer_block::stream::raw_frames_marker`] `Meta` event
/// between the header and the body: an HTTP response body is opaque
/// application bytes, not a codec-encoded DTO, so a consumer that re-encodes
/// frames for a guest on a different host codec must forward it verbatim.
pub async fn handle_message(
    service: &dyn NetworkService,
    ctx: &dyn Context,
    msg: &Message,
    body: &[u8],
) -> OutputStream {
    match msg.kind.as_str() {
        ServiceOp::NETWORK_DO_REQUEST => {
            let wire_req = match decode_and_authorize::<WireRequest>(ctx, body, "network.do", |r| {
                (r.url.clone(), ResourceType::Network, ResourceAccess::Read)
            }) {
                Ok(r) => r,
                Err(out) => return out,
            };

            let request = Request {
                method: wire_req.method,
                url: wire_req.url,
                headers: wire_req.headers,
                body: wire_req.body,
            };

            match do_request_following(service, ctx, request).await {
                Ok(resp) => {
                    let header = ResponseHeader {
                        status_code: resp.status_code,
                        headers: resp.headers,
                    };
                    let body_bytes = resp.body;
                    OutputStream::from_producer(|sink, _cancel| async move {
                        let header_bytes = match codec::encode(&header) {
                            Ok(b) => b,
                            Err(e) => {
                                let _ = sink
                                    .error(WaferError::new(
                                        ErrorCode::Internal,
                                        format!("encoding network response header: {e}"),
                                    ))
                                    .await;
                                return;
                            }
                        };
                        if sink.send_chunk(header_bytes).await.is_err() {
                            return;
                        }
                        // Everything after this marker is the response body:
                        // raw bytes, not a wire DTO.
                        if sink.send_meta(stream::raw_frames_marker()).await.is_err() {
                            return;
                        }
                        // Body is the second frame. Skip the chunk entirely
                        // when empty — consumers reconstruct an empty body
                        // from zero chunks.
                        if !body_bytes.is_empty() && sink.send_chunk(body_bytes).await.is_err() {
                            return;
                        }
                        let _ = sink.complete(vec![]).await;
                    })
                }
                Err(e) => OutputStream::error(e),
            }
        }
        ServiceOp::NETWORK_DO_REQUEST_STREAMING => {
            // Same request shape and WRAP authorization as `NETWORK_DO_REQUEST`
            // — a read of the target URL — so the streaming download can never
            // be reached with a weaker grant than the buffered request.
            let wire_req =
                match decode_and_authorize::<WireRequest>(ctx, body, "network.do_streaming", |r| {
                    (r.url.clone(), ResourceType::Network, ResourceAccess::Read)
                }) {
                    Ok(r) => r,
                    Err(out) => return out,
                };

            let request = Request {
                method: wire_req.method,
                url: wire_req.url,
                headers: wire_req.headers,
                body: wire_req.body,
            };

            match do_request_streaming_following(service, ctx, request).await {
                Ok((head, body_stream)) => {
                    // Two-frame response: a `ResponseHeader` (status + headers)
                    // header chunk followed by the body forwarded verbatim from
                    // the service's stream (never collapsed via
                    // `collect_buffered`).
                    let ResponseHead {
                        status_code,
                        headers,
                    } = head;
                    let header = ResponseHeader {
                        status_code,
                        headers,
                    };
                    stream_with_header(header, body_stream, "network.do_streaming")
                }
                Err(e) => OutputStream::error(e),
            }
        }
        other => OutputStream::error(WaferError::new(
            ErrorCode::Unimplemented,
            format!("unknown network operation: {other}"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use futures::StreamExt;
    use wafer_block::wire::network::Request as WireRequest;

    use super::*;
    use crate::interfaces::network::service::Response;

    /// `Context` stub that grants every resource-access check — these tests
    /// exercise the streaming wire-protocol shape, not authorization.
    struct AllowCtx;

    #[wafer_block::wafer_async_trait]
    impl Context for AllowCtx {
        async fn call_block(
            &self,
            _block_name: &str,
            _msg: Message,
            _input: wafer_block::streams::input::InputStream,
        ) -> OutputStream {
            unimplemented!("not exercised by these tests")
        }

        fn is_cancelled(&self) -> bool {
            unimplemented!("not exercised by these tests")
        }

        fn config_get(&self, _key: &str) -> Option<&str> {
            unimplemented!("not exercised by these tests")
        }

        fn clone_arc(&self) -> std::sync::Arc<dyn Context> {
            unimplemented!("not exercised by these tests")
        }

        fn check_resource_access(
            &self,
            _resource: &str,
            _resource_type: ResourceType,
            _access: ResourceAccess,
        ) -> Result<(), WaferError> {
            Ok(())
        }
        fn resource_access_admitted(
            &self,
            _resource: &str,
            _resource_type: ResourceType,
            _access: ResourceAccess,
        ) -> bool {
            true
        }
    }

    /// A `NetworkService` that returns a fixed-body 200 response.
    struct StubNet {
        body: Vec<u8>,
    }

    #[async_trait::async_trait]
    impl NetworkService for StubNet {
        async fn do_request(&self, _req: &Request) -> Result<Response, NetworkError> {
            Ok(Response {
                status_code: 200,
                headers: HashMap::new(),
                body: self.body.clone(),
            })
        }
    }

    fn do_request_body(url: &str) -> Vec<u8> {
        codec::encode(&WireRequest {
            method: "GET".to_string(),
            url: url.to_string(),
            headers: HashMap::new(),
            body: None,
        })
        .expect("encode wire request")
    }

    #[test]
    fn network_error_maps_to_unavailable_and_internal() {
        assert_eq!(
            network_error_to_wafer(NetworkError::RequestError("timeout".into())).code,
            ErrorCode::Unavailable,
        );
        assert_eq!(
            network_error_to_wafer(NetworkError::Other("boom".into())).code,
            ErrorCode::Internal,
        );
    }

    #[tokio::test]
    async fn empty_body_yields_single_header_frame() {
        let svc = StubNet { body: Vec::new() };
        let msg = Message::new(ServiceOp::NETWORK_DO_REQUEST);
        let out = handle_message(
            &svc,
            &AllowCtx,
            &msg,
            &do_request_body("http://example.com"),
        )
        .await;
        let chunks: Vec<Vec<u8>> = out
            .body_stream_or_error()
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<Result<_, _>>()
            .expect("the body arrives whole");
        assert_eq!(
            chunks.len(),
            1,
            "an empty response body must emit the header frame only (zero body chunks)"
        );
    }

    #[tokio::test]
    async fn non_empty_body_yields_header_and_body_frames() {
        let svc = StubNet {
            body: b"hello".to_vec(),
        };
        let msg = Message::new(ServiceOp::NETWORK_DO_REQUEST);
        let out = handle_message(
            &svc,
            &AllowCtx,
            &msg,
            &do_request_body("http://example.com"),
        )
        .await;
        let chunks: Vec<Vec<u8>> = out
            .body_stream_or_error()
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<Result<_, _>>()
            .expect("the body arrives whole");
        assert_eq!(
            chunks.len(),
            2,
            "a non-empty body must emit header + body frames"
        );
        assert_eq!(
            chunks[1], b"hello",
            "the second frame carries the body bytes"
        );
    }

    /// `Context` stub whose network grant is a real capability allowlist, so a
    /// redirect hop is authorized by the same `allows_network_url` rule the
    /// runtime applies.
    struct GrantCtx(wafer_block::capabilities::BlockCapabilities);

    impl GrantCtx {
        fn allowing(prefixes: &[&str]) -> Self {
            Self(wafer_block::capabilities::BlockCapabilities {
                network: wafer_block::capabilities::Allowlist::Only(
                    prefixes.iter().map(|p| p.to_string()).collect(),
                ),
                ..Default::default()
            })
        }
    }

    #[wafer_block::wafer_async_trait]
    impl Context for GrantCtx {
        async fn call_block(
            &self,
            _block_name: &str,
            _msg: Message,
            _input: wafer_block::streams::input::InputStream,
        ) -> OutputStream {
            unimplemented!("not exercised by these tests")
        }

        fn is_cancelled(&self) -> bool {
            unimplemented!("not exercised by these tests")
        }

        fn config_get(&self, _key: &str) -> Option<&str> {
            unimplemented!("not exercised by these tests")
        }

        fn clone_arc(&self) -> std::sync::Arc<dyn Context> {
            unimplemented!("not exercised by these tests")
        }

        fn check_resource_access(
            &self,
            resource: &str,
            resource_type: ResourceType,
            access: ResourceAccess,
        ) -> Result<(), WaferError> {
            if self.resource_access_admitted(resource, resource_type, access) {
                Ok(())
            } else {
                Err(WaferError::new(
                    ErrorCode::PermissionDenied,
                    format!("network access to {resource} denied"),
                ))
            }
        }
        fn resource_access_admitted(
            &self,
            resource: &str,
            resource_type: ResourceType,
            _access: ResourceAccess,
        ) -> bool {
            resource_type == ResourceType::Network && self.0.allows_network_url(resource)
        }
    }

    /// A `NetworkService` answering from a fixed `url → (status, Location)`
    /// table (200 with the URL as body when absent) and recording every
    /// request it is handed, so a test can prove a hop was never issued.
    struct ScriptedNet {
        redirects: HashMap<String, (u16, String)>,
        issued: std::sync::Mutex<Vec<Request>>,
        /// How long each `do_request` takes.
        hop_delay: std::time::Duration,
        /// `buffered_deadline`; `None` keeps the trait default (never).
        deadline: Option<std::time::Duration>,
    }

    impl ScriptedNet {
        fn new(redirects: &[(&str, u16, &str)]) -> Self {
            Self {
                redirects: redirects
                    .iter()
                    .map(|(from, status, to)| (from.to_string(), (*status, to.to_string())))
                    .collect(),
                issued: std::sync::Mutex::new(Vec::new()),
                hop_delay: std::time::Duration::ZERO,
                deadline: None,
            }
        }

        fn issued_urls(&self) -> Vec<String> {
            self.issued
                .lock()
                .unwrap()
                .iter()
                .map(|r| r.url.clone())
                .collect()
        }
    }

    #[async_trait::async_trait]
    impl NetworkService for ScriptedNet {
        async fn buffered_deadline(&self) {
            match self.deadline {
                Some(d) => tokio::time::sleep(d).await,
                None => futures::future::pending::<()>().await,
            }
        }

        async fn do_request(&self, req: &Request) -> Result<Response, NetworkError> {
            self.issued.lock().unwrap().push(req.clone());
            tokio::time::sleep(self.hop_delay).await;
            Ok(match self.redirects.get(&req.url) {
                Some((status, location)) => Response {
                    status_code: *status,
                    headers: HashMap::from([("location".to_string(), vec![location.clone()])]),
                    body: Vec::new(),
                },
                None => Response {
                    status_code: 200,
                    headers: HashMap::new(),
                    body: req.url.clone().into_bytes(),
                },
            })
        }
    }

    async fn run(op: &str, svc: &ScriptedNet, ctx: &GrantCtx, url: &str) -> OutputStream {
        handle_message(svc, ctx, &Message::new(op), &do_request_body(url)).await
    }

    async fn error_code(out: OutputStream) -> ErrorCode {
        match out.collect_buffered().await {
            Err(wafer_block::streams::output::TerminalNotResponse::Error(e)) => e.code,
            other => panic!("expected an error terminal, got {other:?}"),
        }
    }

    /// A redirect from a granted URL to one outside the grant fails the call
    /// with the grant check's `PermissionDenied`, on both ops, and the
    /// redirect target is never handed to the service.
    #[tokio::test]
    async fn redirect_outside_the_grant_is_denied_before_it_is_issued() {
        for op in [
            ServiceOp::NETWORK_DO_REQUEST,
            ServiceOp::NETWORK_DO_REQUEST_STREAMING,
        ] {
            let svc = ScriptedNet::new(&[(
                "https://api.example/v1/start",
                302,
                "https://internal.example/secret",
            )]);
            let ctx = GrantCtx::allowing(&["https://api.example/v1"]);
            let out = run(op, &svc, &ctx, "https://api.example/v1/start").await;
            assert_eq!(error_code(out).await, ErrorCode::PermissionDenied, "{op}");
            assert_eq!(
                svc.issued_urls(),
                vec!["https://api.example/v1/start".to_string()],
                "{op}: the denied hop must never reach the service"
            );
        }
    }

    /// A redirect inside the grant — relative `Location` included — is
    /// followed, and the caller receives the final response.
    #[tokio::test]
    async fn redirect_inside_the_grant_is_followed() {
        let svc = ScriptedNet::new(&[
            ("https://api.example/v1/a", 301, "https://api.example/v1/b"),
            ("https://api.example/v1/b", 307, "c"),
        ]);
        let ctx = GrantCtx::allowing(&["https://api.example/v1"]);
        let out = run(
            ServiceOp::NETWORK_DO_REQUEST,
            &svc,
            &ctx,
            "https://api.example/v1/a",
        )
        .await;
        let chunks: Vec<Vec<u8>> = out
            .body_stream_or_error()
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<Result<_, _>>()
            .expect("the body arrives whole");
        let header: ResponseHeader = codec::decode(&chunks[0]).expect("header frame");
        assert_eq!(header.status_code, 200);
        assert_eq!(chunks[1], b"https://api.example/v1/c");
        assert_eq!(
            svc.issued_urls(),
            [
                "https://api.example/v1/a",
                "https://api.example/v1/b",
                "https://api.example/v1/c"
            ]
        );
    }

    /// A redirect loop ends after `MAX_REDIRECT_HOPS` followed hops with
    /// `Unavailable`, not an unbounded chain.
    #[tokio::test]
    async fn redirect_loop_stops_at_the_hop_limit() {
        let svc = ScriptedNet::new(&[("https://api.example/loop", 302, "/loop")]);
        let ctx = GrantCtx::allowing(&["https://api.example/"]);
        let out = run(
            ServiceOp::NETWORK_DO_REQUEST,
            &svc,
            &ctx,
            "https://api.example/loop",
        )
        .await;
        assert_eq!(error_code(out).await, ErrorCode::Unavailable);
        assert_eq!(svc.issued_urls().len(), MAX_REDIRECT_HOPS + 1);
    }

    /// The buffered total bounds the whole redirect chain, not each hop: five
    /// 40 ms hops (each well inside the 100 ms total) fail once the chain
    /// passes 100 ms, and the chain stops being issued.
    #[tokio::test]
    async fn buffered_deadline_bounds_the_whole_redirect_chain() {
        let mut svc = ScriptedNet::new(&[
            ("https://api.example/1", 302, "/2"),
            ("https://api.example/2", 302, "/3"),
            ("https://api.example/3", 302, "/4"),
            ("https://api.example/4", 302, "/5"),
        ]);
        svc.hop_delay = std::time::Duration::from_millis(40);
        svc.deadline = Some(std::time::Duration::from_millis(100));
        let ctx = GrantCtx::allowing(&["https://api.example/"]);
        let out = run(
            ServiceOp::NETWORK_DO_REQUEST,
            &svc,
            &ctx,
            "https://api.example/1",
        )
        .await;
        assert_eq!(error_code(out).await, ErrorCode::Unavailable);
        assert!(
            svc.issued_urls().len() < 5,
            "the chain must stop at the deadline, issued: {:?}",
            svc.issued_urls()
        );
    }

    fn post_with_credentials(url: &str) -> Request {
        Request {
            method: "POST".into(),
            url: url.into(),
            headers: HashMap::from([
                ("Authorization".to_string(), "Bearer t".to_string()),
                ("Cookie".to_string(), "s=1".to_string()),
                ("Content-Type".to_string(), "application/json".to_string()),
                ("X-Trace".to_string(), "abc".to_string()),
            ]),
            body: Some(b"{}".to_vec()),
        }
    }

    fn location(to: &str) -> HashMap<String, Vec<String>> {
        HashMap::from([("Location".to_string(), vec![to.to_string()])])
    }

    #[test]
    fn redirect_request_rewrites_method_and_body_by_status() {
        let prev = post_with_credentials("https://a.example/x");
        for status in [301, 302, 303] {
            let next = redirect_request(&prev, status, &location("/y")).expect("followed");
            assert_eq!(next.method, "GET", "{status}");
            assert_eq!(next.body, None, "{status}");
            assert!(!next.headers.contains_key("Content-Type"), "{status}");
            assert_eq!(next.url, "https://a.example/y");
        }
        for status in [307, 308] {
            let next = redirect_request(&prev, status, &location("/y")).expect("followed");
            assert_eq!(next.method, "POST", "{status}");
            assert_eq!(next.body.as_deref(), Some(&b"{}"[..]), "{status}");
            assert!(next.headers.contains_key("Content-Type"), "{status}");
        }
        let head = Request {
            method: "HEAD".into(),
            ..prev
        };
        assert_eq!(
            redirect_request(&head, 303, &location("/y"))
                .unwrap()
                .method,
            "HEAD"
        );
    }

    #[test]
    fn redirect_request_keeps_credentials_only_within_the_origin() {
        let prev = post_with_credentials("https://a.example/x");
        let same = redirect_request(&prev, 307, &location("/y")).unwrap();
        assert!(same.headers.contains_key("Authorization"));
        assert!(same.headers.contains_key("Cookie"));
        for other in [
            "https://b.example/y",
            "http://a.example/y",
            "https://a.example:8443/y",
        ] {
            let next = redirect_request(&prev, 307, &location(other)).unwrap();
            assert!(!next.headers.contains_key("Authorization"), "{other}");
            assert!(!next.headers.contains_key("Cookie"), "{other}");
            assert!(next.headers.contains_key("X-Trace"), "{other}");
        }
    }

    #[test]
    fn redirect_request_leaves_other_responses_final() {
        let prev = post_with_credentials("https://a.example/x");
        assert!(redirect_request(&prev, 200, &location("/y")).is_none());
        assert!(redirect_request(&prev, 304, &location("/y")).is_none());
        assert!(redirect_request(&prev, 302, &HashMap::new()).is_none());
        let two = HashMap::from([("location".to_string(), vec!["/y".into(), "/z".into()])]);
        assert!(redirect_request(&prev, 302, &two).is_none());
        assert!(redirect_request(&prev, 302, &location("http://[::1")).is_none());
    }

    #[tokio::test]
    async fn unknown_operation_is_unimplemented() {
        let svc = StubNet { body: Vec::new() };
        let msg = Message::new("network.bogus");
        let out = handle_message(&svc, &AllowCtx, &msg, &[]).await;
        match out.collect_buffered().await {
            Err(wafer_block::streams::output::TerminalNotResponse::Error(e)) => {
                assert_eq!(e.code, ErrorCode::Unimplemented);
            }
            other => panic!("expected Unimplemented error terminal, got {other:?}"),
        }
    }
}
