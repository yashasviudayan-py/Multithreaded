//! MIME type detection from file paths.

use std::path::Path;

/// Return the MIME type string for the file at `path`, based on its extension.
///
/// Falls back to `"application/octet-stream"` for unknown or missing
/// extensions, which tells browsers to download rather than display the file.
pub fn from_path(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("html") | Some("htm") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js") | Some("mjs") => "application/javascript; charset=utf-8",
        Some("json") => "application/json",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("svg") => "image/svg+xml",
        Some("ico") => "image/x-icon",
        Some("txt") | Some("md") => "text/plain; charset=utf-8",
        Some("pdf") => "application/pdf",
        Some("wasm") => "application/wasm",
        Some("xml") => "application/xml; charset=utf-8",
        Some("webp") => "image/webp",
        Some("mp4") => "video/mp4",
        Some("mp3") => "audio/mpeg",
        Some("ogg") => "audio/ogg",
        Some("webm") => "video/webm",
        Some("ttf") | Some("otf") => "font/ttf",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("zip") => "application/zip",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn mime(name: &str) -> &'static str {
        from_path(&PathBuf::from(name))
    }

    #[test]
    fn html_files() {
        assert_eq!(mime("index.html"), "text/html; charset=utf-8");
        assert_eq!(mime("page.htm"), "text/html; charset=utf-8");
    }

    #[test]
    fn css_and_js() {
        assert_eq!(mime("app.css"), "text/css; charset=utf-8");
        assert_eq!(mime("bundle.js"), "application/javascript; charset=utf-8");
        assert_eq!(mime("module.mjs"), "application/javascript; charset=utf-8");
    }

    #[test]
    fn images() {
        assert_eq!(mime("logo.png"), "image/png");
        assert_eq!(mime("photo.jpg"), "image/jpeg");
        assert_eq!(mime("photo.jpeg"), "image/jpeg");
        assert_eq!(mime("icon.ico"), "image/x-icon");
        assert_eq!(mime("graphic.svg"), "image/svg+xml");
    }

    #[test]
    fn unknown_extension_falls_back_to_octet_stream() {
        assert_eq!(mime("archive.xyz"), "application/octet-stream");
        assert_eq!(mime("noextension"), "application/octet-stream");
    }

    #[test]
    fn json_and_wasm() {
        assert_eq!(mime("data.json"), "application/json");
        assert_eq!(mime("app.wasm"), "application/wasm");
    }
}
