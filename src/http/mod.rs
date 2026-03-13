//! HTTP module: request parsing, response building, and route matching.
//!
//! Public API re-exported from the sub-modules:
//! - [`HttpRequest`] — parsed request wrapper
//! - [`HttpResponse`] / [`ResponseBuilder`] — response construction
//! - [`Router`] — method + path dispatch

pub mod request;
pub mod response;
pub mod router;

pub use request::HttpRequest;
pub use response::{HttpResponse, ResponseBuilder, SERVER_HEADER};
pub use router::Router;
