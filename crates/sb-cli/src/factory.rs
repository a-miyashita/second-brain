//! Builds source adapters for accounts from stored credentials.

use std::sync::Arc;

use sb_core::source::Source;
use sb_core::{AccountKind, Secret};
use sb_google::{GoogleApi, MeetSource, OAuthClient, TokenProvider};
use sb_pipeline::{PipelineError, SourceFactory};
use sb_slack::SlackSource;
use sb_store::{Account, Catalog, SecretScope};

/// The CLI's source factory.
pub struct Factory;

pub const SLACK_TOKEN: &str = "slack.user_token";
pub const GOOGLE_CLIENT: &str = "google.oauth_client";
pub const GOOGLE_REFRESH: &str = "google.refresh_token";

/// The Google OAuth client of an account (account scope, then global).
pub fn google_client(
    cat: &Catalog,
    account: &Account,
) -> Result<Option<OAuthClient>, PipelineError> {
    let scope = SecretScope::Account(account.id.clone());
    let json = match cat.secret(&scope, GOOGLE_CLIENT)? {
        Some(s) => Some(s),
        None => cat.secret(&SecretScope::Global, GOOGLE_CLIENT)?,
    };
    json.map(|j| {
        OAuthClient::from_json(j.expose()).map_err(|e| PipelineError::Invalid(e.to_string()))
    })
    .transpose()
}

/// Token provider of a Google account, if credentials are stored.
pub fn google_tokens(
    cat: &Catalog,
    account: &Account,
) -> Result<Option<Arc<TokenProvider>>, PipelineError> {
    let Some(client) = google_client(cat, account)? else {
        return Ok(None);
    };
    let refresh: Option<Secret> =
        cat.secret(&SecretScope::Account(account.id.clone()), GOOGLE_REFRESH)?;
    Ok(refresh.map(|r| Arc::new(TokenProvider::new(client, r))))
}

impl SourceFactory for Factory {
    fn source(
        &self,
        account: &Account,
        cat: &Catalog,
    ) -> Result<Option<Arc<dyn Source>>, PipelineError> {
        match account.kind {
            AccountKind::Slack => {
                let token =
                    cat.secret_or_env(&SecretScope::Account(account.id.clone()), SLACK_TOKEN)?;
                let tz: String = cat.setting_or("slack.day_timezone", "UTC".to_string())?;
                Ok(Some(Arc::new(SlackSource::new(
                    account.ctx(),
                    token,
                    None,
                    tz,
                )?)))
            }
            AccountKind::Google => {
                let api = match google_tokens(cat, account)? {
                    Some(t) => Some(GoogleApi::new(t)?),
                    None => None,
                };
                Ok(Some(Arc::new(MeetSource::new(account.ctx(), api)?)))
            }
            _ => Ok(None),
        }
    }
}
