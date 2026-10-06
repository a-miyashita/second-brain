//! Builds source adapters for accounts from stored credentials.

use std::sync::Arc;

use sb_core::document::IngestSettings;
use sb_core::source::Source;
use sb_core::{AccountKind, Secret};
use sb_google::{GoogleApi, GoogleSource, OAuthClient, TokenProvider};
use sb_ondemand::{LocalSource, WebSource};
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

/// The `ingest.*` settings.
pub fn ingest_settings(cat: &Catalog) -> Result<IngestSettings, PipelineError> {
    let d = IngestSettings::default();
    Ok(IngestSettings {
        max_file_bytes: cat.setting_or("ingest.max_file_bytes", d.max_file_bytes)?,
        max_text_chars: cat.setting_or("ingest.max_text_chars", d.max_text_chars)?,
        min_text_chars: cat.setting_or("ingest.min_text_chars", d.min_text_chars)?,
        extract_timeout_secs: cat
            .setting_or("ingest.extract_timeout_secs", d.extract_timeout_secs)?,
        keep_original: cat.setting_or("ingest.keep_original", d.keep_original)?,
        web_timeout_secs: cat.setting_or("ingest.web.timeout_secs", d.web_timeout_secs)?,
        web_max_redirects: cat.setting_or("ingest.web.max_redirects", d.web_max_redirects)?,
        web_allow_private: cat.setting_or("ingest.web.allow_private", d.web_allow_private)?,
        local_deny: cat.setting_or("ingest.local.deny", d.local_deny)?,
    })
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
                Ok(Some(Arc::new(GoogleSource::new(
                    account.ctx(),
                    api,
                    ingest_settings(cat)?,
                )?)))
            }
            AccountKind::Local => Ok(Some(Arc::new(LocalSource::new(
                account.ctx(),
                ingest_settings(cat)?,
                cat.home().root().to_path_buf(),
            )))),
            AccountKind::Web => Ok(Some(Arc::new(WebSource::new(
                account.ctx(),
                ingest_settings(cat)?,
            )?))),
            _ => Ok(None),
        }
    }
}
