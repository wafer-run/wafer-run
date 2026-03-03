use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let out_dir = env::var("OUT_DIR").unwrap();

    let content_dir = Path::new(&manifest_dir).join("content");
    let partials_dir = content_dir.join("_partials");
    let out_content_dir = Path::new(&out_dir).join("content");

    // Create output directories
    fs::create_dir_all(out_content_dir.join("docs")).unwrap();

    // Load all partials into a HashMap
    let mut partials: HashMap<String, String> = HashMap::new();
    if partials_dir.exists() {
        for entry in fs::read_dir(&partials_dir).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_file() {
                let name = entry.file_name().to_string_lossy().to_string();
                let contents = fs::read_to_string(entry.path()).unwrap();
                partials.insert(name, contents);
            }
        }
    }

    let doc_head = partials.get("doc_head.html").expect("missing doc_head.html partial");
    let doc_foot = partials.get("doc_foot.html").expect("missing doc_foot.html partial");

    // Process doc pages (content/docs.html + content/docs/*.html)
    for page_path in collect_doc_pages(&content_dir) {
        let raw = fs::read_to_string(&page_path).unwrap();
        let assembled = assemble_doc_page(&raw, doc_head, doc_foot, &partials);
        let rel = page_path.strip_prefix(&content_dir).unwrap();
        let out_path = out_content_dir.join(rel);
        fs::create_dir_all(out_path.parent().unwrap()).unwrap();
        fs::write(&out_path, assembled).unwrap();
    }

    // Copy non-doc pages verbatim
    for name in &["index.html", "playground.html", "registry.html", "theme.css"] {
        let src = content_dir.join(name);
        let dst = out_content_dir.join(name);
        if src.exists() {
            fs::copy(&src, &dst).unwrap();
        }
    }

    println!("cargo:rerun-if-changed=content");
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
        let mut entries: Vec<_> = fs::read_dir(&docs_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().map_or(false, |ext| ext == "html"))
            .collect();
        entries.sort();
        pages.extend(entries);
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
        "chain-configuration",
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
