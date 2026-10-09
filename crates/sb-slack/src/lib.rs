//! Slack source for second-brain: `slack.thread` and `slack.day`
//! (docs/specs/source-slack.md).

pub mod client;
pub mod render;
pub mod source;

pub use client::{AuthInfo, REQUIRED_SCOPES, SlackClient};
pub use source::{SlackConfig, SlackSource, default_config_json};

use second_brain_kernel::Secret;
use second_brain_kernel::source::SourceError;

/// Validate a user token with `auth.test` and report missing scopes
/// (`sb account add slack`, `sb auth login`).
pub async fn validate_token(
    token: &Secret,
    api_base: Option<String>,
) -> Result<(AuthInfo, Vec<String>), SourceError> {
    if !token.expose().starts_with("xoxp-") {
        return Err(SourceError::Auth(
            "expected a Slack user token (xoxp-...); bot tokens cannot read DMs and private channels".into(),
        ));
    }
    let info = SlackClient::new(token.clone(), api_base)?
        .auth_test()
        .await?;
    let missing = if info.scopes.is_empty() {
        Vec::new()
    } else {
        REQUIRED_SCOPES
            .iter()
            .filter(|s| !info.scopes.iter().any(|g| g == *s))
            .map(|s| s.to_string())
            .collect()
    };
    Ok((info, missing))
}
