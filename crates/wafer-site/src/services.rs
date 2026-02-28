//! Platform service initialization for wafer-site.
//!
//! Minimal version: only database (SQLite) and network (HTTP client).

use std::sync::Arc;

use wafer_run::services::config::{ConfigService, EnvConfigService};
use wafer_run::services::database_sqlite::SQLiteDatabaseService;
use wafer_run::services::Services;

/// Build platform services for wafer-site (database + network only).
pub fn build_platform_services() -> Services {
    let config_svc = EnvConfigService::new();

    // --- Database (SQLite) ---
    let db_path = config_svc
        .get("DB_PATH")
        .unwrap_or_else(|| "data/wafer-site.db".to_string());

    if let Some(parent) = std::path::Path::new(&db_path).parent() {
        std::fs::create_dir_all(parent).ok();
    }

    let database_svc = match SQLiteDatabaseService::open(&db_path) {
        Ok(svc) => {
            tracing::info!(path = %db_path, "SQLite database opened");
            Some(Arc::new(svc) as Arc<dyn wafer_run::services::database::DatabaseService>)
        }
        Err(e) => {
            tracing::error!(path = %db_path, error = %e, "failed to open SQLite database");
            None
        }
    };

    // --- Network (HTTP client) ---
    let network_svc: Option<Arc<dyn wafer_run::services::network::NetworkService>> =
        Some(Arc::new(HttpNetworkService));

    Services {
        database: database_svc,
        storage: None,
        logger: None,
        crypto: None,
        config: None,
        network: network_svc,
    }
}

/// Simple reqwest-based network service for outbound HTTP calls.
struct HttpNetworkService;

impl wafer_run::services::network::NetworkService for HttpNetworkService {
    fn do_request(
        &self,
        req: &wafer_run::services::network::Request,
    ) -> Result<wafer_run::services::network::Response, wafer_run::services::network::NetworkError>
    {
        // Clone request data for the spawned thread (reqwest::blocking::Client
        // creates an internal tokio runtime that panics if dropped inside an
        // existing async tokio context, so we run on a dedicated thread).
        let method_str = req.method.clone();
        let url = req.url.clone();
        let headers = req.headers.clone();
        let body = req.body.clone();

        let handle = std::thread::spawn(move || {
            let client = reqwest::blocking::Client::new();

            let method = method_str.parse::<reqwest::Method>().map_err(|e| {
                wafer_run::services::network::NetworkError::RequestError(format!(
                    "invalid method: {}",
                    e
                ))
            })?;

            let mut builder = client.request(method, &url);

            for (key, value) in &headers {
                builder = builder.header(key, value);
            }

            if let Some(body) = body {
                builder = builder.body(body);
            }

            let response = builder.send().map_err(|e| {
                wafer_run::services::network::NetworkError::RequestError(e.to_string())
            })?;

            let status_code = response.status().as_u16();

            let mut resp_headers = std::collections::HashMap::new();
            for (name, value) in response.headers() {
                let entry = resp_headers
                    .entry(name.to_string())
                    .or_insert_with(Vec::new);
                if let Ok(v) = value.to_str() {
                    entry.push(v.to_string());
                }
            }

            let body = response.bytes().map_err(|e| {
                wafer_run::services::network::NetworkError::RequestError(format!(
                    "reading body: {}",
                    e
                ))
            })?;

            Ok(wafer_run::services::network::Response {
                status_code,
                headers: resp_headers,
                body: body.to_vec(),
            })
        });

        handle.join().map_err(|_| {
            wafer_run::services::network::NetworkError::RequestError(
                "HTTP request thread panicked".to_string(),
            )
        })?
    }
}
