//! Hardcoded catalog of supported embedding models.

/// Static metadata about an embedding model entry in the built-in catalog.
#[derive(Debug, Clone, Copy)]
pub struct ModelInfo {
    /// Canonical model identifier (matches the value passed to `embed`).
    pub id: &'static str,
    /// Output embedding dimensionality.
    pub dimensions: u32,
    /// Approximate on-disk size of the model weights, in bytes.
    pub approx_size_bytes: u64,
    /// Which embedding runtimes can serve this model.
    pub runtimes: RuntimeCompat,
}

/// Flags describing which embedding runtimes can host a given model.
#[derive(Debug, Clone, Copy)]
pub struct RuntimeCompat {
    /// Available via the native FastEmbed runtime.
    pub native_fastembed: bool,
    /// Available via Cloudflare Workers AI.
    pub workers_ai: bool,
    /// Available via the in-browser Transformers.js runtime.
    pub browser_transformers: bool,
}

const CATALOG: &[ModelInfo] = &[
    ModelInfo {
        id: "bge-m3",
        dimensions: 1024,
        approx_size_bytes: 2_300_000_000,
        runtimes: RuntimeCompat {
            native_fastembed: true,
            workers_ai: true,
            browser_transformers: false,
        },
    },
    ModelInfo {
        id: "multilingual-e5-small",
        dimensions: 384,
        approx_size_bytes: 470_000_000,
        runtimes: RuntimeCompat {
            native_fastembed: true,
            workers_ai: false,
            browser_transformers: true,
        },
    },
    ModelInfo {
        id: "paraphrase-multilingual-MiniLM-L12-v2",
        dimensions: 384,
        approx_size_bytes: 120_000_000,
        runtimes: RuntimeCompat {
            native_fastembed: true,
            workers_ai: false,
            browser_transformers: true,
        },
    },
];

/// Borrow the full hardcoded catalog of supported embedding models.
pub fn model_catalog() -> &'static [ModelInfo] {
    CATALOG
}

/// Look up the catalog entry for the given model `id`, if any.
pub fn get_model(id: &str) -> Option<&'static ModelInfo> {
    CATALOG.iter().find(|m| m.id == id)
}

/// Default embedding model used when callers do not specify one explicitly.
pub const DEFAULT_MODEL: &str = "bge-m3";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_known_model() {
        let m = get_model("bge-m3").expect("bge-m3 should be in catalog");
        assert_eq!(m.dimensions, 1024);
        assert!(m.runtimes.native_fastembed);
        assert!(m.runtimes.workers_ai);
        assert!(!m.runtimes.browser_transformers);
    }

    #[test]
    fn lookup_unknown_returns_none() {
        assert!(get_model("not-a-real-model").is_none());
    }

    #[test]
    fn all_ids_unique() {
        let mut ids: Vec<&str> = CATALOG.iter().map(|m| m.id).collect();
        ids.sort();
        let len = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), len, "duplicate model id in catalog");
    }

    #[test]
    fn default_model_exists() {
        assert!(get_model(DEFAULT_MODEL).is_some());
    }
}
