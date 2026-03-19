//! HTML template rendering via [`Tera`].
//!
//! [`TemplateEngine`] wraps a pre-compiled [`Tera`] instance that loads all
//! `*.html` files from the `templates/` directory at startup.  Templates use
//! the [Jinja2-like Tera syntax].
//!
//! [Jinja2-like Tera syntax]: https://keats.github.io/tera/docs/
//!
//! # Error handling
//! [`render`][TemplateEngine::render] returns a pre-formatted 500 HTML page
//! on template errors so callers do not need to handle `Result` explicitly.

use std::sync::Arc;

use tera::{Context, Tera};
use tracing::error;

use crate::http::response::{HttpResponse, ResponseBuilder};

/// Compiled template engine.
///
/// Create once at server startup via [`TemplateEngine::new`] and share via
/// `Arc`.  Template files are compiled once; rendering is lock-free.
#[derive(Clone)]
pub struct TemplateEngine {
    tera: Arc<Tera>,
}

impl TemplateEngine {
    /// Load and compile all `*.html` templates from `templates/`.
    ///
    /// Returns `None` if the directory does not exist or no templates are
    /// found (the server falls back gracefully without HTML routes).
    pub fn new(template_dir: &str) -> Option<Self> {
        let pattern = format!("{template_dir}/**/*.html");
        match Tera::new(&pattern) {
            Ok(tera) => {
                tracing::info!(dir = %template_dir, "Tera templates loaded");
                Some(Self {
                    tera: Arc::new(tera),
                })
            }
            Err(e) => {
                // Non-fatal: server runs without HTML routes.
                tracing::warn!(err = %e, dir = %template_dir, "Failed to load templates — /ui routes unavailable");
                None
            }
        }
    }

    /// Render `template_name` with the given [`Context`].
    ///
    /// On error, returns a `500 Internal Server Error` HTML response with
    /// the error message embedded so developers can diagnose render failures
    /// during development.
    pub fn render(&self, template_name: &str, ctx: &Context) -> HttpResponse {
        match self.tera.render(template_name, ctx) {
            Ok(html) => ResponseBuilder::ok()
                .header("content-type", "text/html; charset=utf-8")
                .text(html),
            Err(e) => {
                error!(template = template_name, err = %e, "Template render error");
                ResponseBuilder::internal_error()
                    .header("content-type", "text/html; charset=utf-8")
                    .text(format!("<h1>500 Internal Server Error</h1><pre>{e}</pre>"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_templates_from_existing_dir() {
        // The project's `templates/` directory exists and contains HTML files.
        let engine = TemplateEngine::new("templates");
        assert!(engine.is_some());
    }

    #[test]
    fn renders_known_template() {
        let engine = TemplateEngine::new("templates").expect("templates dir must exist");
        let ctx = Context::new();
        let resp = engine.render("index.html", &ctx);
        // Should succeed — status 200 with HTML body.
        assert_eq!(resp.status().as_u16(), 200);
    }
}
