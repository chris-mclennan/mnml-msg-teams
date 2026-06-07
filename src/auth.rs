//! OAuth 2.0 device-code flow for Microsoft Graph + token persistence.
//!
//! Uses Microsoft's public Azure CLI client ID
//! (`04b07795-8ddb-461a-bbee-02f9e1bf7b46`) — same client `az login` uses,
//! so users who've already consented for Azure CLI never see another
//! consent screen. Token + refresh-token persisted at
//! `~/.config/mnml-msg-teams/token.json` with mode 0600 on Unix.

use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

/// Azure CLI's public client ID. Documented Microsoft-blessed
/// client; appears in countless community OAuth tools. Users who have
/// previously consented for `az login` will skip the consent screen.
pub const AZURE_CLI_CLIENT_ID: &str = "04b07795-8ddb-461a-bbee-02f9e1bf7b46";

/// Space-separated scopes for read + post. `offline_access` is what
/// unlocks the refresh-token.
pub const SCOPES: &str = "User.Read ChatMessage.Read ChatMessage.Send ChannelMessage.Read.All ChannelMessage.Send Channel.ReadBasic.All Team.ReadBasic.All Chat.Read Chat.ReadWrite offline_access";

const DEVICE_CODE_URL: &str = "https://login.microsoftonline.com/common/oauth2/v2.0/devicecode";
const TOKEN_URL: &str = "https://login.microsoftonline.com/common/oauth2/v2.0/token";

/// Persisted token blob.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Token {
    pub access_token: String,
    pub refresh_token: Option<String>,
    /// When `access_token` expires (UTC). Refresh proactively before
    /// hitting Graph if the token is within ~60s of expiry.
    pub expires_at: DateTime<Utc>,
    pub token_type: String,
    /// Scopes the server actually granted (may be a subset of
    /// requested).
    #[serde(default)]
    pub scope: Option<String>,
}

impl Token {
    pub fn is_expired(&self) -> bool {
        Utc::now() >= self.expires_at
    }

    /// Expired or within `secs` seconds of expiry.
    pub fn is_near_expiry(&self, secs: i64) -> bool {
        Utc::now() + ChronoDuration::seconds(secs) >= self.expires_at
    }
}

// ── Device-code flow ────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct DeviceCodeResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    pub interval: u64,
    /// Microsoft's human-readable instruction string. We print our
    /// own UI so this is informational — kept on the wire so callers
    /// can fall back to MS's wording if they want.
    #[allow(dead_code)]
    #[serde(default)]
    pub message: Option<String>,
}

/// `POST /devicecode` — kick off the flow.
pub fn request_device_code() -> Result<DeviceCodeResponse> {
    let client = build_client()?;
    let resp = client
        .post(DEVICE_CODE_URL)
        .form(&[("client_id", AZURE_CLI_CLIENT_ID), ("scope", SCOPES)])
        .send()
        .context("POST devicecode")?;
    let status = resp.status();
    let text = resp.text().context("read devicecode body")?;
    if !status.is_success() {
        return Err(anyhow!("devicecode HTTP {status}: {}", trim(&text, 300)));
    }
    let parsed: DeviceCodeResponse =
        serde_json::from_str(&text).context("parse devicecode JSON")?;
    Ok(parsed)
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    expires_in: i64,
    token_type: String,
    #[serde(default)]
    scope: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OAuthError {
    error: String,
    #[serde(default)]
    error_description: Option<String>,
}

/// Poll `POST /token` with `grant_type=urn:ietf:params:oauth:grant-type:device_code`
/// once. Returns `Ok(Some(token))` on success, `Ok(None)` on a benign
/// pending state (`authorization_pending` or `slow_down`), or `Err` on
/// terminal failure (`expired_token`, `authorization_declined`, etc.).
///
/// Callers loop, sleeping `interval` seconds (and bumping it on
/// `slow_down`) between calls.
pub fn poll_token(device_code: &str) -> Result<PollResult> {
    let client = build_client()?;
    let resp = client
        .post(TOKEN_URL)
        .form(&[
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ("client_id", AZURE_CLI_CLIENT_ID),
            ("device_code", device_code),
        ])
        .send()
        .context("POST token (device-code)")?;
    let status = resp.status();
    let text = resp.text().context("read token body")?;

    if status.is_success() {
        let parsed: TokenResponse = serde_json::from_str(&text).context("parse token JSON")?;
        return Ok(PollResult::Done(token_from_response(parsed)));
    }

    // OAuth error envelope — { "error": "...", "error_description": "..." }
    let err: OAuthError = serde_json::from_str(&text).with_context(|| {
        format!(
            "parse oauth error (HTTP {status}, body: {})",
            trim(&text, 200)
        )
    })?;
    match err.error.as_str() {
        "authorization_pending" => Ok(PollResult::Pending),
        "slow_down" => Ok(PollResult::SlowDown),
        other => Err(anyhow!(
            "oauth error {other}: {}",
            err.error_description.unwrap_or_default()
        )),
    }
}

pub enum PollResult {
    Done(Token),
    Pending,
    SlowDown,
}

/// Exchange a refresh-token for a fresh access-token.
pub fn refresh_token(refresh_token: &str) -> Result<Token> {
    let client = build_client()?;
    let resp = client
        .post(TOKEN_URL)
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", AZURE_CLI_CLIENT_ID),
            ("refresh_token", refresh_token),
            ("scope", SCOPES),
        ])
        .send()
        .context("POST token (refresh_token)")?;
    let status = resp.status();
    let text = resp.text().context("read refresh body")?;
    if !status.is_success() {
        // Try to pull out the OAuth error code for a cleaner message.
        if let Ok(err) = serde_json::from_str::<OAuthError>(&text) {
            return Err(anyhow!(
                "refresh failed ({}): {}",
                err.error,
                err.error_description.unwrap_or_default()
            ));
        }
        return Err(anyhow!("refresh HTTP {status}: {}", trim(&text, 200)));
    }
    let parsed: TokenResponse = serde_json::from_str(&text).context("parse refresh JSON")?;
    Ok(token_from_response(parsed))
}

fn token_from_response(r: TokenResponse) -> Token {
    let expires_at = Utc::now() + ChronoDuration::seconds(r.expires_in);
    Token {
        access_token: r.access_token,
        refresh_token: r.refresh_token,
        expires_at,
        token_type: r.token_type,
        scope: r.scope,
    }
}

// ── Persistence ─────────────────────────────────────────────────

pub fn token_path() -> PathBuf {
    crate::config::config_dir().join("token.json")
}

/// Read + parse the token at `~/.config/mnml-msg-teams/token.json`.
/// Returns `Ok(None)` when the file doesn't exist.
pub fn load_token() -> Result<Option<Token>> {
    load_token_at(&token_path())
}

/// Test seam — `load_token()` calls this with the canonical path.
pub fn load_token_at(path: &std::path::Path) -> Result<Option<Token>> {
    if !path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("read token file {}", path.display()))?;
    let t: Token = serde_json::from_str(&text).context("parse token JSON")?;
    Ok(Some(t))
}

/// Persist with mode 0600 on Unix.
pub fn save_token(t: &Token) -> Result<()> {
    save_token_at(&token_path(), t)
}

pub fn save_token_at(path: &std::path::Path, t: &Token) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
    }
    let text = serde_json::to_string_pretty(t).context("serialize token")?;
    std::fs::write(path, text).with_context(|| format!("write token {}", path.display()))?;
    set_mode_0600(path)?;
    Ok(())
}

#[cfg(unix)]
fn set_mode_0600(path: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(0o600);
    std::fs::set_permissions(path, perms).context("chmod 0600")?;
    Ok(())
}

#[cfg(not(unix))]
fn set_mode_0600(_path: &std::path::Path) -> Result<()> {
    // On Windows we rely on ACLs from `dirs::home_dir()` — no chmod.
    Ok(())
}

/// Delete the persisted token. No-op if it isn't there.
pub fn delete_token() -> Result<()> {
    let path = token_path();
    if path.exists() {
        std::fs::remove_file(&path).with_context(|| format!("remove {}", path.display()))?;
    }
    Ok(())
}

// ── Helpers ─────────────────────────────────────────────────────

fn build_client() -> Result<Client> {
    Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent(concat!("mnml-msg-teams/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("build HTTP client")
}

fn trim(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn fake_token(expires_in_secs: i64) -> Token {
        Token {
            access_token: "AT.fake".into(),
            refresh_token: Some("RT.fake".into()),
            expires_at: Utc::now() + ChronoDuration::seconds(expires_in_secs),
            token_type: "Bearer".into(),
            scope: Some("User.Read".into()),
        }
    }

    #[test]
    fn token_persistence_round_trip() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("token.json");
        let t = fake_token(3600);
        save_token_at(&path, &t).expect("save");
        let loaded = load_token_at(&path).expect("load").expect("present");
        assert_eq!(loaded.access_token, t.access_token);
        assert_eq!(loaded.refresh_token, t.refresh_token);
        assert_eq!(loaded.token_type, t.token_type);
    }

    #[test]
    fn load_token_missing_is_ok_none() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("nope.json");
        let res = load_token_at(&path).expect("ok");
        assert!(res.is_none());
    }

    #[test]
    fn expired_token_detected() {
        let t = fake_token(-60);
        assert!(t.is_expired(), "expired_at < now");
        assert!(t.is_near_expiry(0));
    }

    #[test]
    fn near_expiry_window() {
        let t = fake_token(30);
        // 30s from now, 60s window → near expiry
        assert!(t.is_near_expiry(60));
        // 30s from now, 5s window → not near
        assert!(!t.is_near_expiry(5));
    }

    #[test]
    fn fresh_token_not_expired() {
        let t = fake_token(3600);
        assert!(!t.is_expired());
        assert!(!t.is_near_expiry(60));
    }

    #[cfg(unix)]
    #[test]
    fn token_file_is_mode_0600() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("token.json");
        let t = fake_token(3600);
        save_token_at(&path, &t).expect("save");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "expected mode 0600, got {mode:o}");
    }

    #[test]
    fn refresh_failure_surfaces_oauth_error() {
        // We can't hit the live endpoint in tests, so the failure
        // mode we want to cover is `parse oauth error`. Confirm the
        // OAuthError shape parses cleanly.
        let body =
            r#"{"error":"invalid_grant","error_description":"AADSTS70008: refresh token expired"}"#;
        let err: OAuthError = serde_json::from_str(body).unwrap();
        assert_eq!(err.error, "invalid_grant");
        assert!(err.error_description.unwrap().contains("expired"));
    }

    #[test]
    fn token_from_response_sets_expires_at() {
        let r = TokenResponse {
            access_token: "AT".into(),
            refresh_token: Some("RT".into()),
            expires_in: 3600,
            token_type: "Bearer".into(),
            scope: None,
        };
        let t = token_from_response(r);
        // expires_at should be ~1h from now (within 5s of slack).
        let diff = (t.expires_at - Utc::now()).num_seconds();
        assert!(
            (3595..=3605).contains(&diff),
            "expires_at offset {diff}s out of range"
        );
    }

    #[test]
    fn delete_token_idempotent_when_missing() {
        // delete_token() uses the canonical path which we can't move,
        // so we exercise the equivalent shape directly.
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("nope.json");
        // Mimic delete_token's body
        if path.exists() {
            std::fs::remove_file(&path).unwrap();
        }
        assert!(!path.exists());
    }
}
