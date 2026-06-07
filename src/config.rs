//! Config file at `~/.config/mnml-msg-teams/config.toml`. First
//! run writes the scaffold + exits with instructions.
//!
//! Auth lives in `~/.config/mnml-msg-teams/token.json` (mode 0600),
//! NOT here. Run `mnml-msg-teams auth` to populate it via the OAuth
//! device-code flow.

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_refresh")]
    pub refresh_interval_secs: u64,
    #[serde(default)]
    pub tabs: Vec<Tab>,
}

fn default_refresh() -> u64 {
    60
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tab {
    pub name: String,
    /// Tab kind:
    ///   - `teams` — `GET /me/joinedTeams`; channels lazy-loaded on Enter
    ///   - `chats` — `GET /me/chats` ordered by last activity
    ///   - `search` — interactive query (`POST /search/query`)
    ///   - `threads` — v0.1 stub (placeholder)
    pub kind: String,
}

impl Config {
    pub const EXAMPLE: &'static str = r##"# mnml-msg-teams config. Edit and re-run.
#
# Auth is in `~/.config/mnml-msg-teams/token.json` (NOT here).
# First time? Run `mnml-msg-teams auth` to populate it via the OAuth
# device-code flow (the same one used by `az login`).

refresh_interval_secs = 60

# ── Tabs ─────────────────────────────────────────────────────────
# Kinds:
#   "teams"    — your joined Teams; Enter expands to channels
#   "chats"    — 1:1 + group chats, newest first
#   "search"   — message search (type `/` to enter a query)
#   "threads"  — v0.1 stub; threaded view of a focused channel/chat

[[tabs]]
name = "teams"
kind = "teams"

[[tabs]]
name = "chats"
kind = "chats"

[[tabs]]
name = "search"
kind = "search"

[[tabs]]
name = "threads"
kind = "threads"
"##;

    pub fn validate(&self) -> Result<()> {
        if self.tabs.is_empty() {
            return Err(anyhow!("config: at least one [[tabs]] entry required"));
        }
        for (i, t) in self.tabs.iter().enumerate() {
            match t.kind.as_str() {
                "teams" | "chats" | "search" | "threads" => {}
                other => {
                    return Err(anyhow!(
                        "tab #{i} ({}): unknown kind {other:?} (expected \"teams\", \"chats\", \"search\", or \"threads\")",
                        t.name
                    ));
                }
            }
        }
        Ok(())
    }
}

pub fn config_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
        .join("mnml-msg-teams")
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

pub fn load() -> Result<Config> {
    let path = config_path();
    if !path.exists() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, Config::EXAMPLE)?;
        return Err(anyhow!(
            "wrote config template to {} — edit it then re-run",
            path.display()
        ));
    }
    let text = std::fs::read_to_string(&path)?;
    let cfg: Config = toml::from_str(&text)?;
    cfg.validate()?;
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_config_parses_and_validates() {
        let cfg: Config = toml::from_str(Config::EXAMPLE).expect("example parses");
        cfg.validate().expect("example validates");
        assert!(!cfg.tabs.is_empty());
    }

    #[test]
    fn rejects_no_tabs() {
        let cfg = Config {
            refresh_interval_secs: 60,
            tabs: vec![],
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn rejects_unknown_kind() {
        let cfg = Config {
            refresh_interval_secs: 60,
            tabs: vec![Tab {
                name: "bad".into(),
                kind: "bogus".into(),
            }],
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn accepts_all_known_kinds() {
        for k in &["teams", "chats", "search", "threads"] {
            let cfg = Config {
                refresh_interval_secs: 60,
                tabs: vec![Tab {
                    name: "x".into(),
                    kind: (*k).into(),
                }],
            };
            assert!(cfg.validate().is_ok(), "kind {k} should validate");
        }
    }
}
