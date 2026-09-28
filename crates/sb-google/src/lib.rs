//! Google sources for second-brain: OAuth (PKCE loopback), Drive and Calendar
//! clients, and the `google.meet` source (ADR-0006,
//! docs/specs/source-google-meet.md).

pub mod api;
pub mod gemini;
pub mod meet;
pub mod oauth;

pub use api::GoogleApi;
pub use meet::{MeetSource, default_config_json};
pub use oauth::{OAuthClient, OAuthError, TokenProvider};
