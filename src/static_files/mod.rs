//! Static file serving with async I/O.
//!
//! [`serve_file`] reads a file from `base_dir` relative to the URL path
//! component captured by the `*filepath` route wildcard.  It guards against
//! path-traversal attacks by allowing only [`Normal`](std::path::Component::Normal)
//! path components and rejecting any `..`, `.`, or absolute segments.

pub mod mime;

use std::path::{Component, Path, PathBuf};

use bytes::Bytes;
use hyper::StatusCode;
use tokio::fs;
use tracing::error;

use crate::http::response::{HttpResponse, ResponseBuilder};

/// Serve a file from `base_dir` given the URL-decoded `req_path`.
///
/// `req_path` is the path portion captured from the request URL (e.g.
/// `"css/style.css"` from `/static/css/style.css`).
///
/// # Security
/// Only [`Component::Normal`] path components are allowed.  Any segment that
/// is `..`, `.`, or an absolute root causes an immediate `403 Forbidden`
/// response, preventing path-traversal attacks.
///
/// # Directory handling
/// If `req_path` resolves to a directory, `serve_file` tries to serve
/// `index.html` inside it.  If that file is missing, a `404` is returned.
///
/// # Errors
/// - `403 Forbidden` — rejected path component detected
/// - `404 Not Found` — file (or its parent directory) does not exist
/// - `500 Internal Server Error` — unexpected I/O failure
pub async fn serve_file(base_dir: &Path, req_path: &str) -> HttpResponse {
    // Reject any path that contains traversal components.
    let safe_path = match sanitize_path(req_path) {
        Some(p) => p,
        None => {
            return ResponseBuilder::new(StatusCode::FORBIDDEN).text("403 Forbidden\n");
        }
    };

    let target = base_dir.join(&safe_path);

    // Async metadata read — non-blocking, uses the blocking thread pool.
    let metadata = match fs::metadata(&target).await {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return ResponseBuilder::not_found().text("404 Not Found\n");
        }
        Err(e) => {
            error!(err = %e, path = ?target, "Static file metadata error");
            return ResponseBuilder::internal_error().text("500 Internal Server Error\n");
        }
    };

    // For directories, fall through to index.html.
    let file_path = if metadata.is_dir() {
        target.join("index.html")
    } else {
        target
    };

    // Async file read — non-blocking.
    match fs::read(&file_path).await {
        Ok(bytes) => {
            let content_type = mime::from_path(&file_path);
            ResponseBuilder::ok().bytes_body(content_type, Bytes::from(bytes))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            ResponseBuilder::not_found().text("404 Not Found\n")
        }
        Err(e) => {
            error!(err = %e, path = ?file_path, "Failed to read static file");
            ResponseBuilder::internal_error().text("500 Internal Server Error\n")
        }
    }
}

/// Strip a URL path to only its [`Normal`](Component::Normal) components,
/// returning `None` if any non-normal component (e.g. `..`) is found.
fn sanitize_path(path: &str) -> Option<PathBuf> {
    let mut result = PathBuf::new();
    for component in Path::new(path).components() {
        match component {
            Component::Normal(seg) => result.push(seg),
            Component::CurDir => {} // silently skip "."
            _ => return None,       // reject "..", root "/", and prefix
        }
    }
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_simple_path() {
        let p = sanitize_path("css/style.css").unwrap();
        assert_eq!(p, PathBuf::from("css/style.css"));
    }

    #[test]
    fn sanitize_rejects_traversal() {
        assert!(sanitize_path("../secret.txt").is_none());
        assert!(sanitize_path("css/../../etc/passwd").is_none());
    }

    #[test]
    fn sanitize_skips_cur_dir() {
        let p = sanitize_path("./img/logo.png").unwrap();
        assert_eq!(p, PathBuf::from("img/logo.png"));
    }

    #[test]
    fn sanitize_absolute_path_rejected() {
        assert!(sanitize_path("/etc/passwd").is_none());
    }

    #[test]
    fn sanitize_empty_path() {
        let p = sanitize_path("").unwrap();
        assert_eq!(p, PathBuf::new());
    }
}
