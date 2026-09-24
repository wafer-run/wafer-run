use std::{collections::HashMap, time::Duration};

use thiserror::Error;
use wafer_block::{config::BlockConfig, OutputStream};
use wafer_block_macro::wafer_async_trait;

/// Errors returned by [`NetworkService`] operations.
#[derive(Error, Debug)]
pub enum NetworkError {
    /// Transport-level failure while issuing the request.
    #[error("request error: {0}")]
    RequestError(String),
    /// Catch-all variant carrying an arbitrary backend message.
    #[error("{0}")]
    Other(String),
}

/// Request represents an outbound network request.
#[derive(Debug, Clone)]
pub struct Request {
    /// HTTP method (e.g. `GET`, `POST`).
    pub method: String,
    /// Absolute request URL.
    pub url: String,
    /// Request headers as a flat name → value map.
    pub headers: HashMap<String, String>,
    /// Optional request body.
    pub body: Option<Vec<u8>>,
}

/// Response represents an outbound network response.
#[derive(Debug, Clone)]
pub struct Response {
    /// HTTP status code returned by the upstream server.
    pub status_code: u16,
    /// Response headers; one entry per header name with all values preserved.
    pub headers: HashMap<String, Vec<String>>,
    /// Response body bytes.
    pub body: Vec<u8>,
}

/// Config var key for [`NetworkLimits::max_response_bytes`]. Declared on the
/// `wafer-run/network` block, whose `lifecycle(Init)` hands the resolved
/// value to its service ([`NetworkService::configure`]).
pub const MAX_RESPONSE_BYTES_KEY: &str = "WAFER_RUN__NETWORK__MAX_RESPONSE_BYTES";

/// Config var key for [`NetworkLimits::connect_timeout`], in whole seconds.
pub const CONNECT_TIMEOUT_SECS_KEY: &str = "WAFER_RUN__NETWORK__CONNECT_TIMEOUT_SECS";

/// Config var key for [`NetworkLimits::read_timeout`], in whole seconds.
pub const READ_TIMEOUT_SECS_KEY: &str = "WAFER_RUN__NETWORK__READ_TIMEOUT_SECS";

/// Config var key for [`NetworkLimits::request_timeout`], in whole seconds.
pub const REQUEST_TIMEOUT_SECS_KEY: &str = "WAFER_RUN__NETWORK__REQUEST_TIMEOUT_SECS";

/// Config var key for [`NetworkLimits::stream_timeout`], in whole seconds;
/// unset or empty means no total.
pub const STREAM_TIMEOUT_SECS_KEY: &str = "WAFER_RUN__NETWORK__STREAM_TIMEOUT_SECS";

/// Default response body cap: 50 MiB. SEC-020 — prevents unbounded memory
/// growth from hostile or runaway upstream servers.
pub const DEFAULT_MAX_RESPONSE_BYTES: usize = 50 * 1024 * 1024;

/// Default [`NetworkLimits::connect_timeout`]: 10 s.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Default [`NetworkLimits::read_timeout`]: 30 s.
pub const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(30);

/// Default [`NetworkLimits::request_timeout`]: 30 s.
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Size and time limits of a [`NetworkService`].
///
/// The timeouts are split so a long streaming download is bounded by how long
/// the server goes quiet, not by how long the whole body takes:
/// `connect_timeout` and `read_timeout` apply to every request, the buffered
/// `do_request` also has the total `request_timeout`, and
/// `do_request_streaming` has a total only when `stream_timeout` is set.
///
/// Without `stream_timeout`, an upstream that trickles its body (one byte
/// just inside every `read_timeout`) holds a stream open for up to
/// `max_response_bytes` × `read_timeout` — in practice, indefinitely. Set it
/// when the streams the service serves have a known upper duration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetworkLimits {
    /// Response body cap in bytes (SEC-020), on both paths.
    pub max_response_bytes: usize,
    /// Longest wait to establish a connection (DNS, TCP and TLS).
    pub connect_timeout: Duration,
    /// Longest the connection may go without delivering a byte, while waiting
    /// for the response head and between body chunks. Resets on every read.
    pub read_timeout: Duration,
    /// Total time allowed for a buffered `do_request`, response body included;
    /// through the network handler, for the whole redirect chain (see
    /// [`NetworkService::buffered_deadline`]). Not applied to
    /// `do_request_streaming`, whose body may legitimately take longer than
    /// any fixed total while `read_timeout` still bounds a stall.
    pub request_timeout: Duration,
    /// Total time allowed for a `do_request_streaming` exchange, response
    /// body included, or `None` for no total (the default): a stream that
    /// keeps delivering bytes then lasts as long as its body.
    pub stream_timeout: Option<Duration>,
}

impl Default for NetworkLimits {
    fn default() -> Self {
        Self {
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            read_timeout: DEFAULT_READ_TIMEOUT,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            stream_timeout: None,
        }
    }
}

impl NetworkLimits {
    /// Read the limits from the network block's `lifecycle(Init)` config:
    /// each of the `*_KEY` values, as the runtime resolved it through the
    /// embedder's `ConfigSource` (a string) or as `add_block_config` set it
    /// (a string or a number).
    ///
    /// - Absent → the [`Default`] value (for [`STREAM_TIMEOUT_SECS_KEY`],
    ///   absent or empty → no total; for the others, empty is invalid).
    /// - Present but not a positive integer → an error naming the key and the
    ///   value, never a silent fallback to the default.
    pub fn from_config(config: &BlockConfig) -> Result<Self, NetworkError> {
        let defaults = Self::default();
        let secs = |key: &str, default: Duration| {
            positive(config, key, false).map(|v| v.map_or(default, Duration::from_secs))
        };
        let max_response_bytes = match positive(config, MAX_RESPONSE_BYTES_KEY, false)? {
            None => defaults.max_response_bytes,
            Some(v) => usize::try_from(v).map_err(|_| {
                NetworkError::Other(format!(
                    "{MAX_RESPONSE_BYTES_KEY}={v} is invalid: larger than this platform's \
                     address space"
                ))
            })?,
        };
        Ok(Self {
            max_response_bytes,
            connect_timeout: secs(CONNECT_TIMEOUT_SECS_KEY, defaults.connect_timeout)?,
            read_timeout: secs(READ_TIMEOUT_SECS_KEY, defaults.read_timeout)?,
            request_timeout: secs(REQUEST_TIMEOUT_SECS_KEY, defaults.request_timeout)?,
            stream_timeout: positive(config, STREAM_TIMEOUT_SECS_KEY, true)?
                .map(Duration::from_secs),
        })
    }
}

/// The positive integer `config` sets for `key`: `None` when absent or null
/// (or an empty string, when `empty_is_unset`), an error when present as
/// anything but a positive integer.
fn positive(
    config: &BlockConfig,
    key: &str,
    empty_is_unset: bool,
) -> Result<Option<u64>, NetworkError> {
    let invalid = |shown: &dyn std::fmt::Display| {
        NetworkError::Other(format!(
            "{key}={shown} is invalid: expected a positive integer (unset it to use the default)"
        ))
    };
    match config.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(raw)) if raw.is_empty() && empty_is_unset => Ok(None),
        Some(serde_json::Value::String(raw)) => match raw.parse::<u64>() {
            Ok(v) if v > 0 => Ok(Some(v)),
            _ => Err(invalid(&format!("{raw:?}"))),
        },
        Some(serde_json::Value::Number(n)) => match n.as_u64() {
            Some(v) if v > 0 => Ok(Some(v)),
            _ => Err(invalid(n)),
        },
        Some(other) => Err(invalid(other)),
    }
}

/// Response head (status + headers) paired with a streaming body by
/// [`NetworkService::do_request_streaming`].
///
/// Mirrors [`Response`] without the buffered `body` field — the body arrives
/// separately as an [`OutputStream`] of chunks so the whole response never
/// needs to sit in memory at once.
#[derive(Debug, Clone)]
pub struct ResponseHead {
    /// HTTP status code returned by the upstream server.
    pub status_code: u16,
    /// Response headers; one entry per header name with all values preserved.
    pub headers: HashMap<String, Vec<String>>,
}

/// Service provides outbound network connectivity.
///
/// **Implementations MUST NOT follow redirects.** A 3xx response is returned
/// to the caller as is, `Location` header included. The network block's
/// handler follows redirects itself and authorizes every hop against the
/// caller's grant before issuing it as a new request; a service that follows a
/// redirect internally takes the caller to a URL that check never saw. A
/// backend that cannot surface the 3xx (a browser `fetch` in `manual` mode
/// returns an opaque response with no readable `Location`) must fail the
/// request instead.
#[wafer_async_trait]
pub trait NetworkService: wafer_block::MaybeSend + wafer_block::MaybeSync {
    /// Issue `req` and return the upstream response, or a transport error.
    async fn do_request(&self, req: &Request) -> Result<Response, NetworkError>;

    /// Resolves once the total time allowed for one buffered request has
    /// elapsed, counted from this call. The network handler races a buffered
    /// request's whole redirect chain against it, so the total bounds the
    /// chain rather than each hop. Streaming requests are not raced against
    /// it. The default never resolves: a backend with no timer sets no total.
    async fn buffered_deadline(&self) {
        futures::future::pending::<()>().await
    }

    /// Apply `limits`, which the `wafer-run/network` block reads from its
    /// config at `lifecycle(Init)` ([`NetworkLimits::from_config`]), to every
    /// request issued after this call. The default ignores them: a backend
    /// whose transport has no such knobs (a platform `fetch`) keeps its own.
    fn configure(&self, limits: NetworkLimits) {
        let _ = limits;
    }

    /// Streaming variant of [`do_request`](Self::do_request): issue `req` and
    /// return the [`ResponseHead`] plus the response body as an
    /// [`OutputStream`] of chunks, rather than a fully-buffered [`Response`].
    ///
    /// The default forwards to [`do_request`](Self::do_request) and wraps the
    /// buffered body as a single-chunk stream, so existing backends keep
    /// working unchanged. Backends whose HTTP client exposes a chunked
    /// response body (e.g. reqwest's `bytes_stream`) SHOULD override this to
    /// avoid buffering the whole response in memory. A body-read failure is
    /// surfaced as an `Error` terminal on the returned stream.
    async fn do_request_streaming(
        &self,
        req: &Request,
    ) -> Result<(ResponseHead, OutputStream), NetworkError> {
        let resp = self.do_request(req).await?;
        let head = ResponseHead {
            status_code: resp.status_code,
            headers: resp.headers,
        };
        Ok((head, OutputStream::respond(resp.body)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(entries: &[(&str, serde_json::Value)]) -> BlockConfig {
        let map: serde_json::Map<String, serde_json::Value> = entries
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect();
        BlockConfig::from_event(&wafer_block::LifecycleEvent {
            event_type: wafer_block::LifecycleType::Init,
            data: serde_json::to_vec(&map).unwrap(),
        })
    }

    /// Absent → default; present → parsed (a string as the ConfigSource
    /// hands it over, or a number from `add_block_config`); present but not a
    /// positive integer → an error naming the key and the value.
    #[test]
    fn limits_read_from_the_init_config() {
        assert_eq!(
            NetworkLimits::from_config(&config(&[])).unwrap(),
            NetworkLimits::default()
        );
        assert_eq!(
            NetworkLimits::from_config(&config(&[
                (MAX_RESPONSE_BYTES_KEY, "1234".into()),
                (CONNECT_TIMEOUT_SECS_KEY, 3.into()),
                (READ_TIMEOUT_SECS_KEY, "4".into()),
                (REQUEST_TIMEOUT_SECS_KEY, "5".into()),
                (STREAM_TIMEOUT_SECS_KEY, "6".into()),
            ]))
            .unwrap(),
            NetworkLimits {
                max_response_bytes: 1234,
                connect_timeout: Duration::from_secs(3),
                read_timeout: Duration::from_secs(4),
                request_timeout: Duration::from_secs(5),
                stream_timeout: Some(Duration::from_secs(6)),
            }
        );
        // Empty is "no total" for the one optional limit.
        assert_eq!(
            NetworkLimits::from_config(&config(&[(STREAM_TIMEOUT_SECS_KEY, "".into())]))
                .unwrap()
                .stream_timeout,
            None
        );

        for key in [
            MAX_RESPONSE_BYTES_KEY,
            CONNECT_TIMEOUT_SECS_KEY,
            READ_TIMEOUT_SECS_KEY,
            REQUEST_TIMEOUT_SECS_KEY,
            STREAM_TIMEOUT_SECS_KEY,
        ] {
            for invalid in ["not-a-number", "0", "-5", "12.5", ""] {
                if invalid.is_empty() && key == STREAM_TIMEOUT_SECS_KEY {
                    continue;
                }
                let err = NetworkLimits::from_config(&config(&[(key, invalid.into())]))
                    .expect_err("a present-but-invalid value must fail");
                let msg = err.to_string();
                assert!(
                    msg.contains(key) && msg.contains(&format!("{invalid:?}")),
                    "the error must name the key and the offending value, got: {msg}"
                );
            }
            assert!(
                NetworkLimits::from_config(&config(&[(key, 0.into())])).is_err(),
                "{key}: a numeric 0 must fail too"
            );
        }
    }

    #[test]
    fn network_error_display_matches_variant() {
        assert_eq!(
            NetworkError::RequestError("connection refused".into()).to_string(),
            "request error: connection refused"
        );
        // `Other` passes the message through verbatim (no prefix).
        assert_eq!(
            NetworkError::Other("backend boom".into()).to_string(),
            "backend boom"
        );
    }
}
