//! Microsoft Graph HTTP client for the Teams surface. Blocking
//! `reqwest` + `serde_json`. No SDK dep.
//!
//! Base URL: `https://graph.microsoft.com/v1.0/...`.
//! Auth: `Authorization: Bearer <access_token>` on every request.
//! Errors: `{"error": {"code": "...", "message": "..."}}` — surface as
//! `"graph: {code}: {msg}"`. 429 reads `Retry-After`; no auto-retry.

use crate::auth::{Token, refresh_token, save_token};
use anyhow::{Context, Result, anyhow};
use reqwest::blocking::{Client, RequestBuilder};
use reqwest::{Method, StatusCode};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Mutex, RwLock};
use std::time::Duration;

pub const GRAPH_BASE: &str = "https://graph.microsoft.com/v1.0";

/// Hard cap on items rendered per list. Mirrors `mnml-obs-datadog`'s
/// safety valve.
pub const LIST_CAP: usize = 500;

// ── Graph client ────────────────────────────────────────────────

/// Holds the current token + a lazy display-name cache for user ids.
pub struct GraphClient {
    pub token: RwLock<Token>,
    http: Client,
    #[allow(dead_code)] // exposed via `user_display_name` — UI wires in v0.2
    user_cache: Mutex<HashMap<String, String>>,
}

impl GraphClient {
    pub fn new(token: Token) -> Result<Self> {
        let http = Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent(concat!("mnml-msg-teams/", env!("CARGO_PKG_VERSION")))
            .build()
            .context("build HTTP client")?;
        Ok(Self {
            token: RwLock::new(token),
            http,
            user_cache: Mutex::new(HashMap::new()),
        })
    }

    fn ensure_fresh(&self) -> Result<()> {
        let needs_refresh = {
            let t = self.token.read().unwrap();
            t.is_near_expiry(60)
        };
        if !needs_refresh {
            return Ok(());
        }
        let rt = {
            let t = self.token.read().unwrap();
            t.refresh_token.clone()
        };
        let Some(rt) = rt else {
            return Err(anyhow!(
                "access token expired and no refresh token available — run `mnml-msg-teams auth`"
            ));
        };
        let fresh = refresh_token(&rt)?;
        // Persist the new token (best-effort — surfaces via context but
        // we still update memory).
        save_token(&fresh).ok();
        let mut t = self.token.write().unwrap();
        *t = fresh;
        Ok(())
    }

    fn bearer(&self) -> String {
        let t = self.token.read().unwrap();
        format!("Bearer {}", t.access_token)
    }

    fn req(&self, method: Method, url: &str) -> Result<RequestBuilder> {
        self.ensure_fresh()?;
        Ok(self
            .http
            .request(method, url)
            .header("Authorization", self.bearer())
            .header("Accept", "application/json"))
    }

    /// Run the request and parse JSON or surface a `graph: ...` error.
    /// Handles 401 → one-shot refresh + retry.
    fn send_json<T: for<'de> Deserialize<'de>>(&self, b: RequestBuilder) -> Result<T> {
        let req = b.try_clone().context("clone request for retry")?;
        let resp = req.send().context("graph request")?;
        let status = resp.status();
        let text = resp.text().context("read graph body")?;
        if status == StatusCode::UNAUTHORIZED {
            // Force refresh + try once more.
            let rt = {
                let t = self.token.read().unwrap();
                t.refresh_token.clone()
            };
            if let Some(rt) = rt
                && let Ok(fresh) = refresh_token(&rt)
            {
                save_token(&fresh).ok();
                *self.token.write().unwrap() = fresh;
                // Rebuild with new bearer (the cloned `b` carries
                // the old one). Caller re-issues if they want to.
                let resp2 = b
                    .header("Authorization", self.bearer())
                    .send()
                    .context("graph retry")?;
                let status2 = resp2.status();
                let text2 = resp2.text().context("read retry body")?;
                if status2.is_success() {
                    return serde_json::from_str(&text2).context("parse graph JSON");
                }
                return Err(graph_error(status2, &text2, None));
            }
            return Err(graph_error(status, &text, None));
        }
        if status == StatusCode::TOO_MANY_REQUESTS {
            return Err(anyhow!(
                "graph: 429 throttled — Retry-After unset in body (v0.1 doesn't auto-retry)"
            ));
        }
        if !status.is_success() {
            return Err(graph_error(status, &text, None));
        }
        serde_json::from_str(&text).context("parse graph JSON")
    }

    // ── Identity ────────────────────────────────────────────────

    /// `GET /me` — resolves the current user. Used by `--check`.
    pub fn me(&self) -> Result<MeResponse> {
        let url = format!("{GRAPH_BASE}/me");
        let req = self.req(Method::GET, &url)?;
        self.send_json(req)
    }

    /// Lookup display name by user id, cached for the session.
    #[allow(dead_code)] // wired by UI in v0.2 (mention resolution path)
    pub fn user_display_name(&self, id: &str) -> Result<String> {
        if let Ok(cache) = self.user_cache.lock()
            && let Some(v) = cache.get(id)
        {
            return Ok(v.clone());
        }
        let url = format!("{GRAPH_BASE}/users/{id}?$select=id,displayName");
        let req = self.req(Method::GET, &url)?;
        let user: UserStub = self.send_json(req)?;
        let name = user.display_name.unwrap_or_else(|| id.to_string());
        if let Ok(mut cache) = self.user_cache.lock() {
            cache.insert(id.to_string(), name.clone());
        }
        Ok(name)
    }

    // ── Teams ───────────────────────────────────────────────────

    pub fn joined_teams(&self) -> Result<Vec<Team>> {
        let url = format!("{GRAPH_BASE}/me/joinedTeams");
        let req = self.req(Method::GET, &url)?;
        let resp: ValueArray<Team> = self.send_json(req)?;
        let mut v = resp.value;
        if v.len() > LIST_CAP {
            v.truncate(LIST_CAP);
        }
        Ok(v)
    }

    pub fn team_channels(&self, team_id: &str) -> Result<Vec<Channel>> {
        let url = format!("{GRAPH_BASE}/teams/{team_id}/channels");
        let req = self.req(Method::GET, &url)?;
        let resp: ValueArray<Channel> = self.send_json(req)?;
        Ok(resp.value)
    }

    // ── Chats ───────────────────────────────────────────────────

    pub fn list_chats(&self) -> Result<Vec<Chat>> {
        let url = format!(
            "{GRAPH_BASE}/me/chats?$expand=members&$orderby=lastUpdatedDateTime%20desc&$top=50"
        );
        let req = self.req(Method::GET, &url)?;
        let resp: ValueArray<Chat> = self.send_json(req)?;
        Ok(resp.value)
    }

    // ── Messages ────────────────────────────────────────────────

    pub fn channel_messages(&self, team_id: &str, channel_id: &str) -> Result<Vec<Message>> {
        let url = format!("{GRAPH_BASE}/teams/{team_id}/channels/{channel_id}/messages?$top=30");
        let req = self.req(Method::GET, &url)?;
        let resp: ValueArray<Message> = self.send_json(req)?;
        Ok(resp.value)
    }

    pub fn chat_messages(&self, chat_id: &str) -> Result<Vec<Message>> {
        let url = format!("{GRAPH_BASE}/me/chats/{chat_id}/messages?$top=30");
        let req = self.req(Method::GET, &url)?;
        let resp: ValueArray<Message> = self.send_json(req)?;
        Ok(resp.value)
    }

    pub fn post_to_channel(&self, team_id: &str, channel_id: &str, text: &str) -> Result<Message> {
        let url = format!("{GRAPH_BASE}/teams/{team_id}/channels/{channel_id}/messages");
        let body = serde_json::json!({
            "body": { "content": text, "contentType": "text" }
        });
        let req = self.req(Method::POST, &url)?.json(&body);
        self.send_json(req)
    }

    pub fn post_to_chat(&self, chat_id: &str, text: &str) -> Result<Message> {
        let url = format!("{GRAPH_BASE}/chats/{chat_id}/messages");
        let body = serde_json::json!({
            "body": { "content": text, "contentType": "text" }
        });
        let req = self.req(Method::POST, &url)?.json(&body);
        self.send_json(req)
    }

    pub fn reply_in_channel(
        &self,
        team_id: &str,
        channel_id: &str,
        message_id: &str,
        text: &str,
    ) -> Result<Message> {
        let url = format!(
            "{GRAPH_BASE}/teams/{team_id}/channels/{channel_id}/messages/{message_id}/replies"
        );
        let body = serde_json::json!({
            "body": { "content": text, "contentType": "text" }
        });
        let req = self.req(Method::POST, &url)?.json(&body);
        self.send_json(req)
    }

    pub fn set_chat_reaction(&self, chat_id: &str, message_id: &str, reaction: &str) -> Result<()> {
        let url = format!("{GRAPH_BASE}/me/chats/{chat_id}/messages/{message_id}/setReaction");
        let body = serde_json::json!({ "reactionType": reaction });
        let resp = self
            .req(Method::POST, &url)?
            .json(&body)
            .send()
            .context("graph setReaction")?;
        let status = resp.status();
        let text = resp.text().context("read setReaction body")?;
        if !status.is_success() {
            return Err(graph_error(status, &text, None));
        }
        Ok(())
    }

    // ── Search ──────────────────────────────────────────────────

    pub fn search_messages(&self, query: &str) -> Result<Vec<Message>> {
        let url = format!("{GRAPH_BASE}/search/query");
        let body = serde_json::json!({
            "requests": [{
                "entityTypes": ["chatMessage"],
                "query": { "queryString": query },
                "from": 0,
                "size": 50
            }]
        });
        let req = self.req(Method::POST, &url)?.json(&body);
        let resp: SearchResponse = self.send_json(req)?;
        let mut out = Vec::new();
        for r in resp.value.into_iter() {
            for hc in r.hits_containers.into_iter() {
                for hit in hc.hits.into_iter() {
                    out.push(hit.resource);
                }
            }
        }
        Ok(out)
    }
}

// ── Error parsing ───────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct GraphErrorEnvelope {
    error: GraphErrorBody,
}

#[derive(Debug, Deserialize)]
struct GraphErrorBody {
    #[serde(default)]
    code: String,
    #[serde(default)]
    message: String,
}

fn graph_error(status: StatusCode, body: &str, retry_after: Option<&str>) -> anyhow::Error {
    if let Ok(env) = serde_json::from_str::<GraphErrorEnvelope>(body) {
        let suffix = retry_after
            .map(|r| format!(" (retry-after {r}s)"))
            .unwrap_or_default();
        return anyhow!("graph: {}: {}{}", env.error.code, env.error.message, suffix);
    }
    anyhow!(
        "HTTP {status}: {}",
        body.chars().take(200).collect::<String>()
    )
}

/// Public test seam — used by the unit tests.
#[cfg(test)]
fn parse_graph_error(status: StatusCode, body: &str) -> String {
    graph_error(status, body, None).to_string()
}

// ── Wire shapes ─────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ValueArray<T> {
    #[serde(default = "Vec::new")]
    pub value: Vec<T>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MeResponse {
    pub id: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub user_principal_name: Option<String>,
    #[serde(default)]
    pub mail: Option<String>,
}

impl MeResponse {
    pub fn label(&self) -> String {
        let name = self.display_name.as_deref().unwrap_or("(unknown)");
        let upn = self
            .user_principal_name
            .as_deref()
            .or(self.mail.as_deref())
            .unwrap_or("");
        if upn.is_empty() {
            name.to_string()
        } else {
            format!("{name} <{upn}>")
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct UserStub {
    pub id: String,
    pub display_name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Team {
    pub id: String,
    pub display_name: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Channel {
    pub id: String,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub web_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Chat {
    pub id: String,
    pub topic: Option<String>,
    #[allow(dead_code)] // present in the API; useful for v0.2 (group vs 1:1 chip)
    pub chat_type: Option<String>,
    pub last_updated_date_time: Option<String>,
    #[serde(default)]
    pub members: Vec<ChatMember>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatMember {
    #[serde(default)]
    pub display_name: Option<String>,
    #[allow(dead_code)] // used by `user_display_name` cache path in v0.2
    #[serde(default)]
    pub user_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub id: String,
    pub created_date_time: Option<String>,
    pub message_type: Option<String>,
    pub web_url: Option<String>,
    pub body: Option<MessageBody>,
    pub from: Option<MessageFrom>,
    #[serde(default)]
    pub reactions: Vec<Reaction>,
    #[allow(dead_code)] // present on the wire; available for v0.2 detail view
    pub chat_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageBody {
    #[serde(default)]
    pub content_type: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageFrom {
    pub user: Option<MessageFromUser>,
    pub application: Option<MessageFromApp>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageFromUser {
    #[allow(dead_code)] // available for the user_display_name cache path
    pub id: Option<String>,
    pub display_name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageFromApp {
    pub display_name: Option<String>,
}

impl Message {
    pub fn author(&self) -> String {
        if let Some(f) = &self.from {
            if let Some(u) = &f.user
                && let Some(n) = &u.display_name
            {
                return n.clone();
            }
            if let Some(a) = &f.application
                && let Some(n) = &a.display_name
            {
                return format!("[{n}]");
            }
        }
        "(unknown)".into()
    }

    pub fn is_system(&self) -> bool {
        self.message_type.as_deref() == Some("systemEventMessage")
    }

    pub fn body_text(&self) -> String {
        match &self.body {
            Some(b) => {
                let raw = b.content.clone().unwrap_or_default();
                if b.content_type.as_deref() == Some("html") {
                    strip_html(&raw)
                } else {
                    raw
                }
            }
            None => String::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Reaction {
    pub reaction_type: Option<String>,
    #[allow(dead_code)] // who-reacted; available for v0.2 tooltip
    pub user: Option<MessageFromUser>,
}

// ── Search response shape ──────────────────────────────────────

#[derive(Debug, Deserialize)]
struct SearchResponse {
    #[serde(default)]
    value: Vec<SearchResponseValue>,
}

#[derive(Debug, Deserialize)]
struct SearchResponseValue {
    #[serde(default, rename = "hitsContainers")]
    hits_containers: Vec<SearchHitsContainer>,
}

#[derive(Debug, Deserialize)]
struct SearchHitsContainer {
    #[serde(default)]
    hits: Vec<SearchHit>,
}

#[derive(Debug, Deserialize)]
struct SearchHit {
    resource: Message,
}

// ── HTML stripping helper ───────────────────────────────────────

/// Best-effort HTML → plain text. Teams returns small HTML snippets
/// (`<p>`, `<a>`, `<br>`, `<at>` mention spans). v0.1 strips tags
/// + decodes the five common entities. No full parser.
pub fn strip_html(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut chars = html.chars().peekable();
    let mut in_tag = false;
    while let Some(c) = chars.next() {
        match c {
            '<' => {
                in_tag = true;
                // Recognize `</p>`, `<br>`, `<br/>`, `<p>` → newline.
                let mut look = String::new();
                while let Some(&p) = chars.peek() {
                    if p == '>' {
                        chars.next();
                        in_tag = false;
                        break;
                    }
                    if look.len() < 8 {
                        look.push(p);
                    }
                    chars.next();
                }
                let lt = look.to_ascii_lowercase();
                if lt == "br" || lt == "br/" || lt.starts_with("br ") || lt == "/p" || lt == "/div"
                {
                    out.push('\n');
                }
            }
            '&' if !in_tag => {
                // Tiny entity decoder. Walk up to ';'.
                let mut entity = String::new();
                while let Some(&p) = chars.peek() {
                    if p == ';' {
                        chars.next();
                        break;
                    }
                    if entity.len() > 8 {
                        break;
                    }
                    entity.push(p);
                    chars.next();
                }
                match entity.as_str() {
                    "amp" => out.push('&'),
                    "lt" => out.push('<'),
                    "gt" => out.push('>'),
                    "quot" => out.push('"'),
                    "apos" | "#39" => out.push('\''),
                    "nbsp" => out.push(' '),
                    other => {
                        // Unknown — preserve the raw form so we don't
                        // silently drop content.
                        out.push('&');
                        out.push_str(other);
                        out.push(';');
                    }
                }
            }
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

// ── Permalink yank ──────────────────────────────────────────────

/// `y` payload for a focused message — Teams gives a `webUrl` field
/// on every message resource. Falls back to message id.
pub fn permalink_for(m: &Message) -> String {
    m.web_url.clone().unwrap_or_else(|| m.id.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_joined_teams() {
        let json = r#"{
            "value": [
                {"id": "t1", "displayName": "Engineering", "description": "the eng team"},
                {"id": "t2", "displayName": "Design", "description": null}
            ]
        }"#;
        let parsed: ValueArray<Team> = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.value.len(), 2);
        assert_eq!(parsed.value[0].id, "t1");
        assert_eq!(parsed.value[0].display_name.as_deref(), Some("Engineering"));
    }

    #[test]
    fn parses_channels() {
        let json = r#"{
            "value": [
                {"id":"c1","displayName":"General","description":"","webUrl":"https://teams.microsoft.com/l/channel/.../General"}
            ]
        }"#;
        let parsed: ValueArray<Channel> = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.value[0].display_name.as_deref(), Some("General"));
    }

    #[test]
    fn parses_chats_with_members() {
        let json = r#"{
            "value": [
                {
                    "id": "19:abc@thread.v2",
                    "topic": null,
                    "chatType": "oneOnOne",
                    "lastUpdatedDateTime": "2026-06-07T00:00:00Z",
                    "members": [
                        {"displayName": "Alice", "userId": "u1"},
                        {"displayName": "Bob",   "userId": "u2"}
                    ]
                }
            ]
        }"#;
        let parsed: ValueArray<Chat> = serde_json::from_str(json).unwrap();
        let c = &parsed.value[0];
        assert_eq!(c.chat_type.as_deref(), Some("oneOnOne"));
        assert_eq!(c.members.len(), 2);
    }

    #[test]
    fn parses_message_resource() {
        let json = r#"{
            "id": "m1",
            "createdDateTime": "2026-06-07T12:34:56.789Z",
            "messageType": "message",
            "webUrl": "https://teams.microsoft.com/l/message/.../m1",
            "body": {"contentType": "html", "content": "<p>hi &amp; bye</p>"},
            "from": {"user": {"id": "u1", "displayName": "Alice"}},
            "reactions": [{"reactionType": "like", "user": {"id": "u2", "displayName": "Bob"}}]
        }"#;
        let m: Message = serde_json::from_str(json).unwrap();
        assert_eq!(m.id, "m1");
        assert_eq!(m.author(), "Alice");
        assert_eq!(m.body_text(), "hi & bye\n");
        assert_eq!(m.reactions.len(), 1);
        assert!(!m.is_system());
    }

    #[test]
    fn parses_search_query_response() {
        let json = r#"{
            "value": [{
                "hitsContainers": [{
                    "hits": [
                        {"resource": {
                            "id": "m1",
                            "body": {"contentType":"text","content":"hello"},
                            "messageType": "message"
                        }}
                    ]
                }]
            }]
        }"#;
        let parsed: SearchResponse = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.value.len(), 1);
        assert_eq!(parsed.value[0].hits_containers[0].hits.len(), 1);
        assert_eq!(parsed.value[0].hits_containers[0].hits[0].resource.id, "m1");
    }

    #[test]
    fn parses_graph_error_envelope() {
        let body = r#"{"error":{"code":"InvalidAuthenticationToken","message":"Access token has expired."}}"#;
        let msg = parse_graph_error(StatusCode::UNAUTHORIZED, body);
        assert!(msg.contains("graph: InvalidAuthenticationToken"));
        assert!(msg.contains("expired"));
    }

    #[test]
    fn parses_429_falls_through_to_graph_error() {
        // 429 without an `error` envelope falls back to the HTTP form.
        let body = "{}";
        let msg = parse_graph_error(StatusCode::TOO_MANY_REQUESTS, body);
        assert!(msg.contains("429"));
    }

    #[test]
    fn html_strip_basic_tags() {
        let input = "<p>hello <b>world</b></p>";
        assert_eq!(strip_html(input), "hello world\n");
    }

    #[test]
    fn html_strip_decodes_entities() {
        assert_eq!(strip_html("&amp; &lt; &gt; &nbsp;"), "& < >  ");
    }

    #[test]
    fn html_strip_handles_br_and_newlines() {
        assert_eq!(strip_html("a<br>b<br/>c"), "a\nb\nc");
    }

    #[test]
    fn permalink_yank_uses_web_url() {
        let m = Message {
            id: "m1".into(),
            created_date_time: None,
            message_type: None,
            web_url: Some("https://teams.microsoft.com/l/message/.../m1".into()),
            body: None,
            from: None,
            reactions: vec![],
            chat_id: None,
        };
        assert_eq!(
            permalink_for(&m),
            "https://teams.microsoft.com/l/message/.../m1"
        );
    }

    #[test]
    fn permalink_yank_falls_back_to_id() {
        let m = Message {
            id: "m-no-url".into(),
            created_date_time: None,
            message_type: None,
            web_url: None,
            body: None,
            from: None,
            reactions: vec![],
            chat_id: None,
        };
        assert_eq!(permalink_for(&m), "m-no-url");
    }
}
