# TODO: Move rate limiting and monitoring into http-listener

## IP Rate Limiting

Move per-IP rate limiting into `wafer-block-http-listener` since it's fundamentally a transport-level concern (tied to client IP, should reject before any block processing).

- Configurable `max_requests` and `window_seconds`
- Per-IP sliding window with `Retry-After` and `X-RateLimit-*` headers
- The existing `wafer-block-ip-rate-limit` crate has the logic, just needs to be integrated into http-listener

## Request Monitoring

Add basic request metrics to `wafer-block-http-listener` or `suppers-ai/router`:

- Total requests, status code distribution, top paths
- Expose via `/_stats` endpoint (localhost-only or behind auth)
- Consider pluggable backends (in-memory for dev, external for prod)

## Notes

- For Cloudflare deployments, use CF-native solutions (Rate Limiting Rules, Analytics Engine) instead
- The `wafer-block-ip-rate-limit` and `wafer-block-monitoring` crates in wafer-run still exist if needed as reference
