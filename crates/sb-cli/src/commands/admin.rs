//! `sb config`, `account`, `auth`, `index`, `version`.

use std::time::Duration;

use sb_google::oauth::{self, OAuthClient, Tokens};
use sb_store::settings::{default_value, is_known_key};
use sb_store::{Account, Catalog, EntryFilter, SecretScope};
use second_brain_kernel::{AccountId, AccountKind, AccountStatus, Secret};
use serde_json::{Value, json};

use crate::Ctx;
use crate::cli::{AccountAdd, AccountCmd, AuthCmd, ConfigCmd};
use crate::factory::{GOOGLE_CLIENT, GOOGLE_REFRESH, SLACK_TOKEN, google_client, google_tokens};
use crate::util::{self, exit, failure, usage};

// ---------- config ----------

/// Parse a CLI value: JSON if it parses, else a string.
fn parse_value(s: &str) -> Value {
    serde_json::from_str(s).unwrap_or_else(|_| Value::String(s.to_string()))
}

fn validate_setting(key: &str, value: &Value) -> anyhow::Result<()> {
    if !is_known_key(key) {
        return Err(usage(format!(
            "unknown setting {key:?} (see `sb config list`)"
        )));
    }
    if let Some(name) = key.strip_prefix("llm.profiles.") {
        sb_llm::Profile::from_value(name, value).map_err(|e| usage(e.to_string()))?;
    }
    if key.starts_with("summary.profile.") && !value.is_string() {
        return Err(usage(format!("{key} takes a profile name")));
    }
    if key == "display.language" && !matches!(value.as_str(), Some("ja" | "en")) {
        return Err(usage("display.language is \"ja\" or \"en\""));
    }
    if key == "summary.language" && !matches!(value.as_str(), Some("auto" | "ja" | "en")) {
        return Err(usage("summary.language is \"auto\", \"ja\" or \"en\""));
    }
    if matches!(
        key,
        "summary.budget.weekly_usd" | "summary.budget.monthly_usd"
    ) {
        // An amount in USD; `0` or `null` disables the cap (ADR-0013).
        return match value {
            Value::Null => Ok(()),
            Value::Number(n) if n.as_f64().is_some_and(|v| v.is_finite() && v >= 0.0) => Ok(()),
            _ => Err(usage(format!(
                "{key} is an amount in USD, like 2.0 (0 or null disables the cap)"
            ))),
        };
    }
    if key == "summary.budget.timezone" {
        return match value.as_str() {
            Some(name) if second_brain_kernel::budget::is_valid_tz(name) => Ok(()),
            _ => Err(usage(
                "summary.budget.timezone is an IANA time zone name, like \"Asia/Tokyo\"",
            )),
        };
    }
    if matches!(key, "sync.initial_days" | "sync.overlap_secs")
        && value.as_u64().is_none_or(|n| n == 0)
    {
        return Err(usage(format!("{key} is a positive whole number")));
    }
    if matches!(
        key,
        "ingest.max_file_bytes"
            | "ingest.max_text_chars"
            | "ingest.min_text_chars"
            | "ingest.extract_timeout_secs"
            | "ingest.web.timeout_secs"
    ) && value.as_u64().is_none_or(|n| n == 0)
    {
        return Err(usage(format!("{key} is a positive whole number")));
    }
    if key == "ingest.web.max_redirects" && value.as_u64().is_none() {
        return Err(usage(
            "ingest.web.max_redirects is a whole number (0 or more)",
        ));
    }
    if matches!(key, "ingest.keep_original" | "ingest.web.allow_private") && !value.is_boolean() {
        return Err(usage(format!("{key} is true or false")));
    }
    if key == "ingest.local.deny"
        && !value
            .as_array()
            .is_some_and(|a| a.iter().all(Value::is_string))
    {
        return Err(usage(
            "ingest.local.deny is a list of path patterns, like [\"**/*.pem\"]",
        ));
    }
    if let Some(d) = default_value(key)
        && std::mem::discriminant(&d) != std::mem::discriminant(value)
        && !(d.is_number() && value.is_number())
    {
        return Err(usage(format!("{key} expects a value like {d}")));
    }
    Ok(())
}

fn editor() -> String {
    std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| {
            if cfg!(windows) {
                "notepad".into()
            } else {
                "vi".into()
            }
        })
}

/// Open `content` in the editor and return the edited text.
fn edit_text(ctx: &Ctx, content: &str) -> anyhow::Result<String> {
    let dir = ctx.home.tmp_dir();
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("edit-{}.toml", std::process::id()));
    std::fs::write(&path, content)?;
    let ed = editor();
    let mut parts = ed.split_whitespace();
    let prog = parts.next().unwrap_or("vi");
    let status = std::process::Command::new(prog)
        .args(parts)
        .arg(&path)
        .status();
    let text = std::fs::read_to_string(&path);
    let _ = std::fs::remove_file(&path);
    match status {
        Ok(s) if s.success() => Ok(text?),
        Ok(s) => Err(failure("config.editor", format!("editor exited with {s}"))),
        Err(e) => Err(failure("config.editor", format!("cannot run {ed:?}: {e}"))),
    }
}

fn json_to_toml(v: &Value) -> anyhow::Result<toml::Value> {
    toml::Value::try_from(v).map_err(|e| failure("config.toml", e.to_string()))
}

pub fn config(ctx: &Ctx, c: ConfigCmd) -> anyhow::Result<i32> {
    let cat = ctx.catalog()?;
    match c {
        ConfigCmd::List => {
            let settings = cat.list_settings()?;
            let secrets = cat.list_secrets()?;
            if ctx.json {
                ctx.out_json(
                    "sb.config/v1",
                    json!({"settings": settings, "secrets": secrets}),
                );
            } else {
                for s in &settings {
                    println!(
                        "{} = {}{}",
                        s.key,
                        s.value,
                        if s.default { "  (default)" } else { "" }
                    );
                }
                if !secrets.is_empty() {
                    println!("\nSecrets:");
                    for s in &secrets {
                        println!("  {} {} = {}", s.scope, s.name, s.masked);
                    }
                }
            }
        }
        ConfigCmd::Get { key } => {
            let secret = cat.secret(&SecretScope::Global, &key)?.map(|s| s.masked());
            let value = match &secret {
                Some(m) => Some(json!(m)),
                None => cat.setting(&key)?,
            };
            let Some(v) = value else {
                return Err(failure("config.not_set", format!("{key} is not set")));
            };
            if ctx.json {
                ctx.out_json(
                    "sb.config/v1",
                    json!({"key": key, "value": v, "secret": secret.is_some()}),
                );
            } else {
                println!(
                    "{}",
                    if let Value::String(s) = &v {
                        s.clone()
                    } else {
                        v.to_string()
                    }
                );
            }
        }
        ConfigCmd::Set { key, value } => {
            let v = parse_value(&value);
            validate_setting(&key, &v)?;
            cat.set_setting(&key, &v)?;
            if ctx.json {
                ctx.out_json("sb.config/v1", json!({"key": key, "value": v}));
            }
        }
        ConfigCmd::Unset { key } => {
            let removed = cat.unset_setting(&key)?;
            if ctx.json {
                ctx.out_json("sb.config/v1", json!({"key": key, "removed": removed}));
            }
        }
        ConfigCmd::SetSecret { name, account } => {
            let scope = match &account {
                Some(a) => SecretScope::Account(
                    cat.account_by_str(a)?
                        .ok_or_else(|| failure("account.not_found", format!("no account {a}")))?
                        .id,
                ),
                None => SecretScope::Global,
            };
            let v = util::read_secret(&format!("Value for {name}"))?;
            cat.set_secret(&scope, &name, &Secret::new(v))?;
            if ctx.json {
                ctx.out_json(
                    "sb.config/v1",
                    json!({"secret": name, "scope": scope.as_key()}),
                );
            } else {
                eprintln!("Stored {name} ({}).", scope.as_key());
            }
        }
        ConfigCmd::Edit { account } => match account {
            Some(a) => {
                let acc = cat
                    .account_by_str(&a)?
                    .ok_or_else(|| failure("account.not_found", format!("no account {a}")))?;
                let text = toml::to_string_pretty(&json_to_toml(&acc.config)?)?;
                let edited = edit_text(ctx, &text)?;
                let parsed: toml::Value =
                    toml::from_str(&edited).map_err(|e| usage(format!("invalid TOML: {e}")))?;
                let v = serde_json::to_value(parsed)?;
                match acc.kind {
                    AccountKind::Slack => {
                        serde_json::from_value::<sb_slack::SlackConfig>(v.clone())
                            .map_err(|e| usage(format!("invalid Slack config: {e}")))?;
                    }
                    AccountKind::Google => {
                        serde_json::from_value::<sb_google::meet::MeetConfig>(v.clone())
                            .map_err(|e| usage(format!("invalid Google config: {e}")))?;
                    }
                    _ => {}
                }
                cat.update_account(&acc.id, None, None, Some(&v))?;
                eprintln!("Updated the config of {a}.");
            }
            None => {
                let mut table = toml::Table::new();
                for s in cat.list_settings()?.into_iter().filter(|s| !s.default) {
                    table.insert(s.key, json_to_toml(&s.value)?);
                }
                let text = format!(
                    "# second-brain settings. Keys are dotted setting names; see `sb config list`.\n{}",
                    toml::to_string_pretty(&table)?
                );
                let edited = edit_text(ctx, &text)?;
                let parsed: toml::Table =
                    toml::from_str(&edited).map_err(|e| usage(format!("invalid TOML: {e}")))?;
                let mut new: Vec<(String, Value)> = Vec::new();
                for (k, v) in parsed {
                    let v = serde_json::to_value(v)?;
                    validate_setting(&k, &v)?;
                    new.push((k, v));
                }
                for s in cat.list_settings()?.into_iter().filter(|s| !s.default) {
                    if !new.iter().any(|(k, _)| *k == s.key) {
                        cat.unset_setting(&s.key)?;
                    }
                }
                for (k, v) in new {
                    cat.set_setting(&k, &v)?;
                }
                eprintln!("Settings updated.");
            }
        },
    }
    Ok(exit::OK)
}

// ---------- account ----------

fn new_account_id(cat: &Catalog, id: &str) -> anyhow::Result<AccountId> {
    let id = AccountId::new(id).map_err(|e| usage(e.to_string()))?;
    if cat.account(&id)?.is_some() {
        return Err(usage(format!("account {id} already exists")));
    }
    Ok(id)
}

/// Run the Google consent flow and return the tokens.
pub(crate) async fn google_consent(
    client: &OAuthClient,
    features: &[String],
    no_browser: bool,
    login_hint: Option<&str>,
) -> anyhow::Result<Tokens> {
    let scopes = oauth::scopes_for(features, false);
    let pending = oauth::begin(client, &scopes, login_hint).await?;
    let headless = no_browser
        || (cfg!(target_os = "linux")
            && std::env::var_os("DISPLAY").is_none()
            && std::env::var_os("WAYLAND_DISPLAY").is_none());
    if headless || open::that(&pending.url).is_err() {
        eprintln!(
            "Open this URL in a browser on this machine and approve access:\n\n{}\n",
            pending.url
        );
    } else {
        eprintln!(
            "Opened the browser for Google consent. If nothing happened, open:\n\n{}\n",
            pending.url
        );
    }
    eprintln!("Waiting for the redirect (up to 5 minutes)...");
    Ok(pending.finish(Duration::from_secs(300)).await?)
}

async fn add_google(
    ctx: &Ctx,
    id: &str,
    client_secret: &str,
    label: Option<String>,
    features: Vec<String>,
    no_browser: bool,
) -> anyhow::Result<i32> {
    let cat = ctx.catalog()?;
    let aid = new_account_id(&cat, id)?;
    let (client_json, global) = if client_secret == "global" {
        let s = cat
            .secret(&SecretScope::Global, GOOGLE_CLIENT)?
            .ok_or_else(|| usage("no global google.oauth_client is stored"))?;
        (s.expose().to_string(), true)
    } else {
        (
            std::fs::read_to_string(client_secret)
                .map_err(|e| usage(format!("{client_secret}: {e}")))?,
            false,
        )
    };
    let client = OAuthClient::from_json(&client_json).map_err(|e| usage(e.to_string()))?;
    let features: Vec<String> = features
        .into_iter()
        .map(|f| f.trim().to_string())
        .filter(|f| !f.is_empty())
        .collect();
    for f in &features {
        if !matches!(f.as_str(), "meet" | "docs" | "gmail") {
            return Err(usage(format!("unknown feature {f:?} (meet, docs, gmail)")));
        }
    }
    let tokens = google_consent(&client, &features, no_browser, None).await?;
    let refresh = tokens.refresh_token.clone().ok_or_else(|| {
        failure(
            "auth.no_refresh_token",
            "Google returned no refresh token; revoke the app's access and try again",
        )
    })?;
    let email = tokens.email.clone().unwrap_or_default();
    let mut config = sb_google::default_config_json(&features);
    config["scopes"] = json!(tokens.scopes);
    let label = label.unwrap_or_else(|| {
        if email.is_empty() {
            id.to_string()
        } else {
            email.clone()
        }
    });
    cat.add_account(&aid, AccountKind::Google, &label, Some(&email), &config)?;
    let scope = SecretScope::Account(aid.clone());
    if !global {
        cat.set_secret(&scope, GOOGLE_CLIENT, &Secret::new(client_json))?;
    }
    cat.set_secret(&scope, GOOGLE_REFRESH, &refresh)?;
    if ctx.json {
        ctx.out_json(
            "sb.account/v1",
            json!({"added": id, "kind": "google", "identity": email}),
        );
    } else {
        eprintln!("Added Google account {id} ({email}).");
    }
    Ok(exit::OK)
}

/// Validate a Slack token, returning `(identity, team name, team url, missing scopes)`.
async fn check_slack_token(
    token: &Secret,
) -> anyhow::Result<(String, String, String, Vec<String>)> {
    let (info, missing) = sb_slack::validate_token(token, None)
        .await
        .map_err(|e| failure("auth.slack", e.to_string()))?;
    Ok((
        format!("{}:{}", info.team_id, info.user_id),
        info.team,
        info.url,
        missing,
    ))
}

async fn add_slack(
    ctx: &Ctx,
    id: &str,
    token: Option<String>,
    label: Option<String>,
) -> anyhow::Result<i32> {
    let cat = ctx.catalog()?;
    let aid = new_account_id(&cat, id)?;
    let token = match token {
        Some(t) => t,
        None => util::read_secret("Slack user token (xoxp-...)")?,
    };
    let token = Secret::new(token.trim());
    let (identity, team, url, missing) = check_slack_token(&token).await?;
    if !missing.is_empty() {
        eprintln!(
            "warning: the token lacks scopes: {} (see assets/slack-app-manifest.yaml)",
            missing.join(", ")
        );
    }
    let mut config = sb_slack::default_config_json();
    config["team_url"] = json!(url);
    cat.add_account(
        &aid,
        AccountKind::Slack,
        &label.unwrap_or(team.clone()),
        Some(&identity),
        &config,
    )?;
    cat.set_secret(&SecretScope::Account(aid), SLACK_TOKEN, &token)?;
    if ctx.json {
        ctx.out_json(
            "sb.account/v1",
            json!({"added": id, "kind": "slack", "identity": identity, "missing_scopes": missing}),
        );
    } else {
        eprintln!("Added Slack account {id} ({team}, {identity}).");
        eprintln!(
            "Tip: set channels to ingest fully with `sb config edit --account {id}` (full_channels)."
        );
    }
    Ok(exit::OK)
}

fn account_json(cat: &Catalog, a: &Account) -> anyhow::Result<Value> {
    let entries = cat.count_entries(&EntryFilter {
        accounts: vec![a.id.to_string()],
        ..Default::default()
    })?;
    Ok(json!({
        "id": a.id, "kind": a.kind, "label": a.label, "identity": a.identity,
        "status": a.status, "entries": entries,
        "created_at": second_brain_kernel::util::ts(a.created_at),
    }))
}

pub async fn account(ctx: &Ctx, c: AccountCmd) -> anyhow::Result<i32> {
    match c {
        AccountCmd::Add(AccountAdd::Google {
            id,
            client_secret,
            label,
            features,
            no_browser,
        }) => add_google(ctx, &id, &client_secret, label, features, no_browser).await,
        AccountCmd::Add(AccountAdd::Slack { id, token, label }) => {
            add_slack(ctx, &id, token, label).await
        }
        AccountCmd::List => {
            let cat = ctx.catalog()?;
            let rows: Vec<Value> = cat
                .accounts()?
                .iter()
                .map(|a| account_json(&cat, a))
                .collect::<anyhow::Result<_>>()?;
            if ctx.json {
                ctx.out_json("sb.account.list/v1", json!({"accounts": rows}));
            } else {
                for r in &rows {
                    println!(
                        "{:<16} {:<7} {:<13} {:>7} entries  {}",
                        r["id"].as_str().unwrap_or(""),
                        r["kind"].as_str().unwrap_or(""),
                        r["status"].as_str().unwrap_or(""),
                        r["entries"],
                        r["identity"].as_str().unwrap_or("")
                    );
                }
            }
            Ok(exit::OK)
        }
        AccountCmd::Show { id } => {
            let cat = ctx.catalog()?;
            let a = cat
                .account_by_str(&id)?
                .ok_or_else(|| failure("account.not_found", format!("no account {id}")))?;
            let mut v = account_json(&cat, &a)?;
            v["config"] = a.config.clone();
            let scope = format!("account:{}", a.id);
            v["secrets"] = json!(
                cat.list_secrets()?
                    .into_iter()
                    .filter(|s| s.scope == scope)
                    .map(|s| json!({"name": s.name, "masked": s.masked}))
                    .collect::<Vec<_>>()
            );
            if ctx.json {
                ctx.out_json("sb.account/v1", json!({"account": v}));
            } else {
                println!("{}", serde_json::to_string_pretty(&v)?);
            }
            Ok(exit::OK)
        }
        AccountCmd::Disable { id } => set_status(ctx, &id, AccountStatus::Disabled),
        AccountCmd::Enable { id } => set_status(ctx, &id, AccountStatus::Active),
        AccountCmd::Remove { id, purge, yes } => {
            let cat = ctx.catalog()?;
            let a = cat
                .account_by_str(&id)?
                .ok_or_else(|| failure("account.not_found", format!("no account {id}")))?;
            let entries = cat.list_entries(&EntryFilter {
                accounts: vec![id.clone()],
                ..Default::default()
            })?;
            if purge && !entries.is_empty() && !yes {
                if !util::interactive() {
                    return Err(usage(
                        "--purge deletes entries and raw files; add --yes to confirm",
                    ));
                }
                if !util::confirm(
                    &format!(
                        "Delete account {id} and its {} entries and raw files?",
                        entries.len()
                    ),
                    false,
                )? {
                    return Ok(exit::OK);
                }
            }
            let mut paths = Vec::new();
            for e in &entries {
                paths.extend(cat.raw_objects(e.id)?.into_iter().map(|r| r.path));
            }
            let n = cat
                .remove_account(&a.id, purge)
                .map_err(|e| usage(e.to_string()))?;
            sb_store::rawstore::delete_paths(cat.home(), &paths);
            let dir = cat.home().raw_dir().join(a.id.as_str());
            if dir.exists() {
                let _ = std::fs::remove_dir_all(dir);
            }
            if ctx.json {
                ctx.out_json(
                    "sb.account/v1",
                    json!({"removed": id, "entries_deleted": n}),
                );
            } else {
                eprintln!("Removed account {id} ({n} entries deleted).");
            }
            Ok(exit::OK)
        }
    }
}

fn set_status(ctx: &Ctx, id: &str, status: AccountStatus) -> anyhow::Result<i32> {
    let cat = ctx.catalog()?;
    let a = cat
        .account_by_str(id)?
        .ok_or_else(|| failure("account.not_found", format!("no account {id}")))?;
    cat.set_account_status(&a.id, status)?;
    if ctx.json {
        ctx.out_json("sb.account/v1", json!({"id": id, "status": status}));
    }
    Ok(exit::OK)
}

// ---------- auth ----------

/// Live credential check of one account. `Ok(None)` means not applicable.
pub(crate) async fn online_check(
    cat: &Catalog,
    a: &Account,
) -> anyhow::Result<Option<Result<String, String>>> {
    match a.kind {
        AccountKind::Google => {
            let Some(t) = google_tokens(cat, a)? else {
                return Ok(Some(Err("no stored refresh token".into())));
            };
            Ok(Some(match t.access_token().await {
                Ok(_) => Ok("token refresh OK".into()),
                Err(e) => Err(e.to_string()),
            }))
        }
        AccountKind::Slack => {
            let Some(token) =
                cat.secret_or_env(&SecretScope::Account(a.id.clone()), SLACK_TOKEN)?
            else {
                return Ok(Some(Err("no stored token".into())));
            };
            Ok(Some(match sb_slack::validate_token(&token, None).await {
                Ok((_, missing)) if missing.is_empty() => Ok("auth.test OK".into()),
                Ok((_, missing)) => Err(format!("missing scopes: {}", missing.join(", "))),
                Err(e) => Err(e.to_string()),
            }))
        }
        _ => Ok(None),
    }
}

pub async fn auth(ctx: &Ctx, c: AuthCmd) -> anyhow::Result<i32> {
    match c {
        AuthCmd::Status { online } => {
            let cat = ctx.catalog()?;
            let mut rows = Vec::new();
            let mut needs = false;
            for a in cat.accounts()? {
                if matches!(a.kind, AccountKind::Local | AccountKind::Web) {
                    continue;
                }
                let last_ok = cat
                    .cache_get_stale(a.id.as_str(), "auth.last_ok")?
                    .and_then(|v| v.as_str().map(str::to_string));
                let mut row = json!({
                    "id": a.id, "kind": a.kind, "identity": a.identity, "status": a.status,
                    "last_ok": last_ok, "scopes": a.config.get("scopes"),
                });
                let mut bad = a.status == AccountStatus::NeedsReauth;
                if online && a.status != AccountStatus::Disabled {
                    match online_check(&cat, &a).await? {
                        Some(Ok(m)) => row["online"] = json!({"ok": true, "message": m}),
                        Some(Err(m)) => {
                            bad = true;
                            row["online"] = json!({"ok": false, "message": m});
                        }
                        None => {}
                    }
                }
                needs |= bad;
                rows.push(row);
            }
            if ctx.json {
                ctx.out_json(
                    "sb.auth.status/v1",
                    json!({"accounts": rows, "needs_reauth": needs}),
                );
            } else {
                for r in &rows {
                    let online = r
                        .get("online")
                        .map(|o| format!("  [{}]", o["message"].as_str().unwrap_or("")))
                        .unwrap_or_default();
                    println!(
                        "{:<16} {:<7} {:<13} {}  last ok: {}{}",
                        r["id"].as_str().unwrap_or(""),
                        r["kind"].as_str().unwrap_or(""),
                        r["status"].as_str().unwrap_or(""),
                        r["identity"].as_str().unwrap_or(""),
                        r["last_ok"].as_str().unwrap_or("-"),
                        online
                    );
                    if r["status"] == "needs_reauth" {
                        println!("  -> sb auth login {}", r["id"].as_str().unwrap_or(""));
                    }
                }
            }
            Ok(if needs { exit::PROBLEMS } else { exit::OK })
        }
        AuthCmd::Login {
            id,
            allow_identity_change,
            no_browser,
        } => {
            let cat = ctx.catalog()?;
            let a = cat
                .account_by_str(&id)?
                .ok_or_else(|| failure("account.not_found", format!("no account {id}")))?;
            let scope = SecretScope::Account(a.id.clone());
            let identity = match a.kind {
                AccountKind::Google => {
                    let client = google_client(&cat, &a)?.ok_or_else(|| {
                        failure(
                            "auth.no_client",
                            "no OAuth client stored; use `sb account add google` again",
                        )
                    })?;
                    let features: Vec<String> = a
                        .config
                        .get("features")
                        .and_then(Value::as_array)
                        .map(|f| {
                            f.iter()
                                .filter_map(Value::as_str)
                                .map(str::to_string)
                                .collect()
                        })
                        .unwrap_or_else(|| vec!["meet".into()]);
                    let tokens =
                        google_consent(&client, &features, no_browser, a.identity.as_deref())
                            .await?;
                    let email = tokens.email.clone().unwrap_or_default();
                    if a.identity
                        .as_deref()
                        .is_some_and(|i| !i.is_empty() && i != email)
                        && !allow_identity_change
                    {
                        return Err(failure(
                            "auth.identity_changed",
                            format!(
                                "signed in as {email}, but the account is {}; use --allow-identity-change",
                                a.identity.clone().unwrap_or_default()
                            ),
                        ));
                    }
                    let refresh = tokens.refresh_token.ok_or_else(|| {
                        failure("auth.no_refresh_token", "Google returned no refresh token")
                    })?;
                    cat.set_secret(&scope, GOOGLE_REFRESH, &refresh)?;
                    let mut config = a.config.clone();
                    config["scopes"] = json!(tokens.scopes);
                    cat.update_account(&a.id, None, Some(&email), Some(&config))?;
                    email
                }
                AccountKind::Slack => {
                    let token = Secret::new(util::read_secret("New Slack user token (xoxp-...)")?);
                    let (ident, _, _, missing) = check_slack_token(&token).await?;
                    if a.identity.as_deref().is_some_and(|i| i != ident) && !allow_identity_change {
                        return Err(failure(
                            "auth.identity_changed",
                            format!(
                                "the token belongs to {ident}, but the account is {}; use --allow-identity-change",
                                a.identity.clone().unwrap_or_default()
                            ),
                        ));
                    }
                    if !missing.is_empty() {
                        eprintln!("warning: the token lacks scopes: {}", missing.join(", "));
                    }
                    cat.set_secret(&scope, SLACK_TOKEN, &token)?;
                    cat.update_account(&a.id, None, Some(&ident), None)?;
                    ident
                }
                k => return Err(usage(format!("{k} accounts have no credentials"))),
            };
            cat.set_account_status(&a.id, AccountStatus::Active)?;
            cat.resolve_issues("auth.", Some(a.id.as_str()))?;
            if ctx.json {
                ctx.out_json("sb.auth.login/v1", json!({"id": id, "identity": identity}));
            } else {
                eprintln!("Re-authenticated {id} ({identity}).");
            }
            Ok(exit::OK)
        }
    }
}

// ---------- index / version ----------

pub fn index_rebuild(ctx: &Ctx) -> anyhow::Result<i32> {
    let p = ctx.pipeline()?;
    let n = p.rebuild_index()?;
    if ctx.json {
        ctx.out_json(
            "sb.index/v1",
            json!({"backend": sb_store::fts::NAME, "rows": n}),
        );
    } else {
        eprintln!("Rebuilt {}: {n} rows.", sb_store::fts::NAME);
    }
    Ok(exit::OK)
}

pub fn version(ctx: &Ctx) -> anyhow::Result<i32> {
    let v = json!({
        "version": env!("CARGO_PKG_VERSION"),
        "target": format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS),
        "skill_version": sb_setup::skills::embedded_version(),
        "schema_version": sb_store::SCHEMA_VERSION,
    });
    if ctx.json {
        ctx.out_json("sb.version/v1", v);
    } else {
        println!(
            "second-brain {} ({}), skill version {}, schema {}",
            v["version"].as_str().unwrap_or(""),
            v["target"].as_str().unwrap_or(""),
            v["skill_version"].as_str().unwrap_or(""),
            v["schema_version"]
        );
    }
    Ok(exit::OK)
}
