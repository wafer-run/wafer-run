//! The one extension → content type table.
//!
//! Every type a browser decodes as text declares `charset=utf-8`. Without
//! it, a browser decodes `text/*` as windows-1252, so a non-ASCII byte
//! becomes mojibake. Binary types declare no charset, because one would be a
//! false claim about their bytes.

use std::path::Path;

/// What [`mime_for_ext`] and [`mime_for_ext_str`] answer for an extension
/// they do not know (or a name without one). It means "the type is
/// unknown", not "the bytes are binary".
pub const UNKNOWN: &str = "application/octet-stream";

/// Guess the MIME content type from a file extension.
///
/// Accepts a file path (or bare filename) and returns the MIME type string
/// based on its extension. An unknown extension, or none, returns
/// [`UNKNOWN`].
pub fn mime_for_ext(path: &Path) -> &'static str {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");

    mime_for_ext_str(ext)
}

/// Guess the MIME content type from a bare extension string (without the
/// dot), matched without regard to case.
///
/// Rust and TOML source is `text/plain`, so a browser shows it rather than
/// downloading it.
pub fn mime_for_ext_str(ext: &str) -> &'static str {
    match ext.to_ascii_lowercase().as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "application/javascript; charset=utf-8",
        "json" | "map" => "application/json; charset=utf-8",
        "xml" => "application/xml; charset=utf-8",
        "svg" => "image/svg+xml; charset=utf-8",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "eot" => "application/vnd.ms-fontobject",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "wasm" => "application/wasm",
        "txt" | "rs" | "toml" => "text/plain; charset=utf-8",
        "md" => "text/markdown; charset=utf-8",
        "csv" => "text/csv; charset=utf-8",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        "ogg" => "audio/ogg",
        _ => UNKNOWN,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every type a browser decodes as text declares UTF-8. Without a
    /// charset, a browser decodes `text/*` as windows-1252 and an
    /// `image/svg+xml` document by sniffing, so a non-ASCII byte becomes
    /// mojibake.
    #[test]
    fn every_textual_type_declares_utf8() {
        for ext in [
            "html", "htm", "css", "js", "mjs", "json", "map", "xml", "svg", "txt", "md", "csv",
            "rs", "toml",
        ] {
            let ty = mime_for_ext_str(ext);
            assert!(
                ty.ends_with("; charset=utf-8"),
                "{ext} is served as {ty:?}, which declares no UTF-8 charset"
            );
        }
    }

    #[test]
    fn plain_and_data_text_types_are_the_ones_a_site_publishes() {
        for (name, expected) in [
            ("llms.txt", "text/plain; charset=utf-8"),
            ("README.md", "text/markdown; charset=utf-8"),
            ("feed.xml", "application/xml; charset=utf-8"),
            ("data.csv", "text/csv; charset=utf-8"),
            ("logo.svg", "image/svg+xml; charset=utf-8"),
            ("app.js.map", "application/json; charset=utf-8"),
            ("src/lib.rs", "text/plain; charset=utf-8"),
            ("Cargo.toml", "text/plain; charset=utf-8"),
        ] {
            assert_eq!(mime_for_ext(Path::new(name)), expected, "{name}");
        }
    }

    /// A charset on a binary type is a false claim about its bytes.
    #[test]
    fn binary_types_declare_no_charset() {
        for ext in [
            "png", "jpg", "jpeg", "gif", "webp", "avif", "ico", "woff", "woff2", "ttf", "otf",
            "eot", "pdf", "zip", "wasm", "mp4", "webm", "mp3", "ogg",
        ] {
            let ty = mime_for_ext_str(ext);
            assert!(!ty.contains("charset"), "{ext} is served as {ty:?}");
            assert_ne!(ty, UNKNOWN, "{ext} must be a known type");
        }
    }

    #[test]
    fn an_unknown_extension_is_the_unknown_type() {
        assert_eq!(UNKNOWN, "application/octet-stream");
        for name in ["a.xyz", "noext", ".gitignore"] {
            assert_eq!(mime_for_ext(Path::new(name)), UNKNOWN, "{name}");
        }
    }

    #[test]
    fn the_extension_is_matched_without_regard_to_case() {
        assert_eq!(mime_for_ext_str("TXT"), "text/plain; charset=utf-8");
        assert_eq!(mime_for_ext(Path::new("PHOTO.JPG")), "image/jpeg");
    }
}
