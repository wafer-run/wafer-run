use std::path::Path;

use anyhow::bail;

/// The programming language of a block project.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Rust,
    Go,
    TypeScript,
}

impl Lang {
    /// Parse a language string from the CLI (case-insensitive).
    pub fn from_str(s: &str) -> anyhow::Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "rust" | "rs" => Ok(Lang::Rust),
            "go" | "golang" => Ok(Lang::Go),
            "typescript" | "ts" => Ok(Lang::TypeScript),
            other => bail!("Unknown language {other:?}. Supported: rust, go, typescript"),
        }
    }
}

/// Detect the language of an existing block project in `dir` by inspecting
/// well-known files.
#[allow(dead_code)]
///
/// Detection order:
///   1. `Cargo.toml`  → Rust
///   2. `go.mod`      → Go
///   3. `package.json` → TypeScript
pub fn detect_language(dir: &Path) -> anyhow::Result<Lang> {
    if dir.join("Cargo.toml").exists() {
        return Ok(Lang::Rust);
    }
    if dir.join("go.mod").exists() {
        return Ok(Lang::Go);
    }
    if dir.join("package.json").exists() {
        return Ok(Lang::TypeScript);
    }
    bail!(
        "Could not detect language in {}: no Cargo.toml, go.mod, or package.json found",
        dir.display()
    )
}
