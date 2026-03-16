use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR")
        .expect("CARGO_MANIFEST_DIR not set (this should only happen outside cargo)");
    let out_dir = env::var("OUT_DIR")
        .expect("OUT_DIR not set (this should only happen outside cargo)");

    let content_dir = Path::new(&manifest_dir).join("content");
    let public_dir = Path::new(&manifest_dir).join("public");
    let partials_dir = content_dir.join("_partials");

    // Two output targets: OUT_DIR (for include_str!) and dist/ (for web block serving)
    let out_content_dir = Path::new(&out_dir).join("content");
    let dist_dir = Path::new(&manifest_dir).join("dist");

    // Create output directories
    for dir in [
        out_content_dir.join("docs"),
        dist_dir.join("docs"),
        dist_dir.join("css"),
        dist_dir.join("images"),
    ] {
        if let Err(e) = fs::create_dir_all(&dir) {
            eprintln!("cargo:warning=failed to create directory {:?}: {}", dir, e);
            return;
        }
    }

    // Load all partials into a HashMap
    let mut partials: HashMap<String, String> = HashMap::new();
    if partials_dir.exists() {
        if let Ok(entries) = fs::read_dir(&partials_dir) {
            for entry in entries.flatten() {
                if entry.file_type().map_or(false, |ft| ft.is_file()) {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if let Ok(contents) = fs::read_to_string(entry.path()) {
                        partials.insert(name, contents);
                    } else {
                        eprintln!("cargo:warning=failed to read partial: {:?}", entry.path());
                    }
                }
            }
        }
    }

    let doc_head = match partials.get("doc_head.html") {
        Some(h) => h.clone(),
        None => {
            eprintln!("cargo:warning=missing doc_head.html partial — skipping doc page assembly");
            return;
        }
    };
    let doc_foot = match partials.get("doc_foot.html") {
        Some(f) => f.clone(),
        None => {
            eprintln!("cargo:warning=missing doc_foot.html partial — skipping doc page assembly");
            return;
        }
    };

    // Process doc pages (content/docs.html + content/docs/*.html)
    for page_path in collect_doc_pages(&content_dir) {
        let raw = match fs::read_to_string(&page_path) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("cargo:warning=failed to read {:?}: {}", page_path, e);
                continue;
            }
        };
        let assembled = assemble_doc_page(&raw, &doc_head, &doc_foot, &partials);
        if let Ok(rel) = page_path.strip_prefix(&content_dir) {
            // Write to OUT_DIR (for include_str in playground/registry blocks)
            let out_path = out_content_dir.join(rel);
            if let Some(parent) = out_path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            if let Err(e) = fs::write(&out_path, &assembled) {
                eprintln!("cargo:warning=failed to write {:?}: {}", out_path, e);
            }

            // Write to dist/ (for web block serving)
            let dist_path = dist_dir.join(rel);
            if let Some(parent) = dist_path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            if let Err(e) = fs::write(&dist_path, &assembled) {
                eprintln!("cargo:warning=failed to write {:?}: {}", dist_path, e);
            }
        }
    }

    // Copy non-doc content pages to both OUT_DIR and dist/
    for name in &["index.html", "theme.css"] {
        let src = content_dir.join(name);
        if src.exists() {
            // OUT_DIR
            let _ = fs::copy(&src, out_content_dir.join(name));
            // dist/ — theme.css goes under css/
            let dist_dest = if *name == "theme.css" {
                dist_dir.join("css").join(name)
            } else {
                dist_dir.join(name)
            };
            let _ = fs::copy(&src, &dist_dest);
        }
    }

    // playground.html and registry.html only to OUT_DIR (served by their own blocks)
    for name in &["playground.html", "registry.html"] {
        let src = content_dir.join(name);
        if src.exists() {
            let _ = fs::copy(&src, out_content_dir.join(name));
        }
    }

    // Copy public/ assets to dist/
    copy_dir_recursive(&public_dir, &dist_dir);

    // favicon.ico needs to be at the root (HTML references /favicon.ico)
    let favicon_src = dist_dir.join("images").join("favicon.ico");
    if favicon_src.exists() {
        let _ = fs::copy(&favicon_src, dist_dir.join("favicon.ico"));
    }

    println!("cargo:rerun-if-changed=content");
    println!("cargo:rerun-if-changed=public");
}

/// Recursively copy a directory, preserving structure.
fn copy_dir_recursive(src: &Path, dst: &Path) {
    if !src.exists() {
        return;
    }
    let entries = match fs::read_dir(src) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let src_path = entry.path();
        let rel = src_path.strip_prefix(src).unwrap();
        let dst_path = dst.join(rel);
        if src_path.is_dir() {
            let _ = fs::create_dir_all(&dst_path);
            copy_dir_recursive(&src_path, &dst_path);
        } else {
            if let Some(parent) = dst_path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let _ = fs::copy(&src_path, &dst_path);
        }
    }
}

/// Collect all doc page paths: content/docs.html + content/docs/*.html
fn collect_doc_pages(content_dir: &Path) -> Vec<PathBuf> {
    let mut pages = vec![];

    let docs_html = content_dir.join("docs.html");
    if docs_html.exists() {
        pages.push(docs_html);
    }

    let docs_dir = content_dir.join("docs");
    if docs_dir.exists() {
        if let Ok(entries) = fs::read_dir(&docs_dir) {
            let mut entries: Vec<_> = entries
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().map_or(false, |ext| ext == "html"))
                .collect();
            entries.sort();
            pages.extend(entries);
        }
    }

    pages
}

/// Parse front-matter from `<!-- ... -->` comment block.
/// Returns (metadata map, body after the closing `-->`)
fn parse_front_matter(raw: &str) -> (HashMap<String, String>, &str) {
    let mut meta = HashMap::new();

    let start = match raw.find("<!--") {
        Some(i) => i + 4,
        None => return (meta, raw),
    };
    let end = match raw[start..].find("-->") {
        Some(i) => start + i,
        None => return (meta, raw),
    };

    let front = &raw[start..end];
    let body = &raw[end + 3..];

    let lines: Vec<&str> = front.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim();
        if line.is_empty() {
            i += 1;
            continue;
        }

        if line.starts_with("EXTRA_STYLES_INLINE:") {
            // Multi-line value terminated by END_EXTRA_STYLES_INLINE
            let mut value = String::new();
            i += 1;
            while i < lines.len() {
                let l = lines[i];
                if l.trim() == "END_EXTRA_STYLES_INLINE" {
                    break;
                }
                value.push_str(l);
                value.push('\n');
                i += 1;
            }
            meta.insert("EXTRA_STYLES_INLINE".to_string(), value);
        } else if let Some(colon_pos) = line.find(':') {
            let key = line[..colon_pos].trim().to_string();
            let value = line[colon_pos + 1..].trim().to_string();
            meta.insert(key, value);
        }

        i += 1;
    }

    (meta, body)
}

/// Assemble a full HTML page from front-matter metadata + body + partials.
fn assemble_doc_page(
    raw: &str,
    doc_head: &str,
    doc_foot: &str,
    partials: &HashMap<String, String>,
) -> String {
    let (meta, body) = parse_front_matter(raw);

    let mut head = doc_head.to_string();

    // {{TITLE}}
    let title = meta.get("TITLE").cloned().unwrap_or_default();
    head = head.replace("{{TITLE}}", &title);

    // {{ACTIVE_*}} placeholders
    let active = meta.get("ACTIVE").cloned().unwrap_or_default();
    let sidebar_items = [
        "quick-start",
        "core-concepts",
        "creating-a-block",
        "running-a-block",
        "wasm-blocks",
        "cli",
        "flow-configuration",
        "built-in-blocks",
        "services",
        "http-bridge",
        "api-runtime",
        "api-services",
        "api-sdk",
        "api-types",
        "registry",
        "deployment",
    ];
    for item in &sidebar_items {
        let placeholder = format!("{{{{ACTIVE_{}}}}}", item);
        let replacement = if *item == active {
            " class=\"active\""
        } else {
            ""
        };
        head = head.replace(&placeholder, replacement);
    }

    // {{EXTRA_STYLES}} — combine file-based and inline styles
    let mut extra_styles = String::new();
    if let Some(filename) = meta.get("EXTRA_STYLES") {
        if let Some(contents) = partials.get(filename) {
            extra_styles.push_str(contents);
        }
    }
    if let Some(inline) = meta.get("EXTRA_STYLES_INLINE") {
        extra_styles.push_str(inline);
    }
    head = head.replace("{{EXTRA_STYLES}}", &extra_styles);

    // {{EXTRA_STYLES_MOBILE}}
    let mut extra_mobile = String::new();
    if let Some(filename) = meta.get("EXTRA_STYLES_MOBILE") {
        if let Some(contents) = partials.get(filename) {
            extra_mobile.push_str(contents);
        }
    }
    head = head.replace("{{EXTRA_STYLES_MOBILE}}", &extra_mobile);

    // {{AFTER_BODY_OPEN}}
    let mut after_body = String::new();
    if let Some(filename) = meta.get("AFTER_BODY_OPEN") {
        if let Some(contents) = partials.get(filename) {
            after_body.push_str(contents);
        }
    }
    head = head.replace("{{AFTER_BODY_OPEN}}", &after_body);

    // Build footer with {{BEFORE_BODY_CLOSE}}
    let mut foot = doc_foot.to_string();
    let mut before_close = String::new();
    if let Some(filename) = meta.get("BEFORE_BODY_CLOSE") {
        if let Some(contents) = partials.get(filename) {
            before_close.push_str(contents);
        }
    }
    foot = foot.replace("{{BEFORE_BODY_CLOSE}}", &before_close);

    // Assemble: head + body + foot
    let mut result = head;
    result.push_str(body);
    result.push_str(&foot);
    result
}
