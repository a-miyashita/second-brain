//! Microsoft sources for second-brain: OAuth against Entra ID (PKCE loopback),
//! a thin Microsoft Graph client and the `microsoft.file` source for SharePoint
//! and OneDrive files (ADR-0021, docs/specs/source-documents.md).

pub mod graph;
pub mod oauth;
pub mod source;

pub use graph::GraphApi;
pub use oauth::{Identity, MicrosoftClient, MicrosoftConfig, RefreshSink, TokenProvider};
pub use second_brain_oauth::OAuthError;
pub use source::MicrosoftSource;
