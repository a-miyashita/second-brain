//! On-demand sources for `sb ingest` (docs/specs/source-documents.md): files on
//! the local disk and web pages. Neither has `sync`.

mod glob;
pub mod local;
pub mod web;

pub use local::LocalSource;
pub use web::WebSource;
