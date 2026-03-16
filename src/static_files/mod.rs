//! Static file serving with async streaming I/O.
//!
//! [`serve_file`] streams a file from `base_dir` using [`tokio::fs::File`] +
//! [`ReaderStream`] instead of reading the entire file into memory.  The
//! `Content-Length` header is set from file metadata so clients know the size
//! up front.
//!
//! ## Path-traversal protection
//! Only [`Component::Normal`] segments are allowed.  Any `..`, `.`, absolute
//! root, or prefix segment triggers an immediate `403 Forbidden`.

pub mod mime;

use std::convert::Infallible;
use std::path::{Component, Path, PathBuf};

use bytes::Bytes;
use futures_util::StreamExt;
use http_body_util::{BodyExt, StreamBody};
use hyper::body::Frame;
use hyper::StatusCode;
use tokio::fs::File;
use tokio_util::io::ReaderStream;
use tracing::error;

use crate::http::response::{HttpResponse, ResponseBuilder};

/// Serve a file from `base_dir` given the URL-decoded `req_path`.
///
/// `req_path` is the path portion captured from the request URL (e.g.
/// `"css/style.css"` from `/static/css/style.css`).
///
/// The file is streamed in chunks — it is never fully buffered in memory.
/// `Content-Length` is taken from filesystem metadata before streaming begins.
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

    // Async metadata read — discovers size and whether target is a directory.
    let metadata = match tokio::fs::metadata(&target).await {
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

    // Open the file asynchronously.  Getting the size from the open file
    // handle (fstat) instead of a second fs::metadata() call avoids the
    // TOCTOU window where the file could change between two separate stat calls.
    let file = match File::open(&file_path).await {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return ResponseBuilder::not_found().text("404 Not Found\n");
        }
        Err(e) => {
            error!(err = %e, path = ?file_path, "Failed to open static file");
            return ResponseBuilder::internal_error().text("500 Internal Server Error\n");
        }
    };

    // fstat the already-open file descriptor — atomic with the open above.
    let file_size = match file.metadata().await {
        Ok(m) => m.len(),
        Err(e) => {
            error!(err = %e, path = ?file_path, "Failed to stat open static file");
            return ResponseBuilder::internal_error().text("500 Internal Server Error\n");
        }
    };

    let content_type = mime::from_path(&file_path);

    // Stream the file in chunks.  Any mid-stream I/O error is logged and the
    // stream is terminated; the client will detect the truncation via the
    // content-length mismatch.
    let fp_for_log = file_path.clone();
    let stream = ReaderStream::new(file).filter_map(move |result| {
        let path = fp_for_log.clone();
        async move {
            match result {
                Ok(chunk) => Some(Ok::<Frame<Bytes>, Infallible>(Frame::data(chunk))),
                Err(e) => {
                    error!(err = %e, path = ?path, "File stream read error");
                    None
                }
            }
        }
    });

    let body = BodyExt::boxed(StreamBody::new(stream));

    ResponseBuilder::ok().stream_body(content_type, body, file_size)
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
