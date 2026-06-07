mod app;
mod auth;
mod clipboard;
mod config;
mod keys;
mod teams;
mod ui;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::io::Write;
use std::thread;
use std::time::{Duration, Instant};

#[derive(Parser, Debug)]
#[command(
    name = "mnml-msg-teams",
    version,
    about = "Microsoft Teams browse + post for mnml"
)]
struct Cli {
    /// Print the resolved config + auth state and exit.
    #[arg(long)]
    check: bool,

    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Run the OAuth device-code flow + persist tokens.
    Auth {
        /// Delete the persisted token instead of logging in.
        #[arg(long)]
        logout: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    if let Some(Cmd::Auth { logout }) = cli.cmd {
        return if logout { cmd_logout() } else { cmd_login() };
    }

    if cli.check {
        return cmd_check();
    }

    // Bare → launch TUI.
    let cfg = config::load()?;
    let token = match auth::load_token()? {
        Some(t) => t,
        None => {
            eprintln!("not authenticated — run `mnml-msg-teams auth` first.");
            std::process::exit(2);
        }
    };
    let graph = teams::GraphClient::new(token)?;
    let mut app = app::App::new(cfg, graph)?;
    ui::run(&mut app)
}

// ── auth ────────────────────────────────────────────────────────

fn cmd_login() -> Result<()> {
    println!("requesting device code from Microsoft…");
    let dc = auth::request_device_code()?;
    println!();
    println!("┌─────────────────────────────────────────────────────────────┐");
    println!("│  Open {:<50}  │", dc.verification_uri);
    println!("│  and enter the code:                                        │");
    println!("│                                                             │");
    println!("│                {:^29}                │", dc.user_code);
    println!("│                                                             │");
    println!("│  (code copied to clipboard)                                 │");
    println!("└─────────────────────────────────────────────────────────────┘");
    println!();
    if let Err(e) = clipboard::copy(&dc.user_code) {
        eprintln!("warn: clipboard copy failed — copy manually: {e}");
    }
    // Best-effort browser open.
    if let Err(e) = webbrowser::open(&dc.verification_uri) {
        eprintln!("warn: couldn't open browser — paste the URL manually: {e}");
    }

    println!("polling for completion (every {}s)…", dc.interval);
    let mut interval = dc.interval.max(1);
    let started = Instant::now();
    let max = Duration::from_secs(dc.expires_in.max(120));

    loop {
        if started.elapsed() > max {
            anyhow::bail!(
                "device-code expired after {}s — re-run `mnml-msg-teams auth`",
                max.as_secs()
            );
        }
        thread::sleep(Duration::from_secs(interval));
        match auth::poll_token(&dc.device_code)? {
            auth::PollResult::Done(token) => {
                auth::save_token(&token)?;
                println!();
                println!(
                    "✓ authenticated. Token saved to {}",
                    auth::token_path().display()
                );
                if let Some(rt) = &token.refresh_token {
                    let _ = rt;
                    println!("  (refresh-token persisted — typical TTL ~90 days)");
                }
                return Ok(());
            }
            auth::PollResult::Pending => {
                print!(".");
                let _ = std::io::stdout().flush();
            }
            auth::PollResult::SlowDown => {
                interval = (interval + 5).min(30);
                println!(" (slow_down → next poll in {interval}s)");
            }
        }
    }
}

fn cmd_logout() -> Result<()> {
    let path = auth::token_path();
    if path.exists() {
        auth::delete_token()?;
        println!("✓ logged out. Removed {}", path.display());
    } else {
        println!("(no token at {} — nothing to do)", path.display());
    }
    Ok(())
}

// ── --check ─────────────────────────────────────────────────────

fn cmd_check() -> Result<()> {
    let cfg = config::load();
    println!("config: {}", config::config_path().display());
    match &cfg {
        Ok(cfg) => {
            println!("tabs:");
            for (i, t) in cfg.tabs.iter().enumerate() {
                println!("  {} ({}): kind={}", i + 1, t.name, t.kind);
            }
        }
        Err(e) => println!("config: ERROR — {e}"),
    }

    println!();
    println!("token: {}", auth::token_path().display());
    let token = auth::load_token().ok().flatten();
    match token {
        None => {
            println!("token: (not present — run `mnml-msg-teams auth`)");
            std::process::exit(2);
        }
        Some(t) => {
            let now = chrono::Utc::now();
            let secs_left = (t.expires_at - now).num_seconds();
            if t.is_expired() {
                println!("token: EXPIRED ({}s ago)", -secs_left);
            } else {
                println!("token: ok ({}s until expiry)", secs_left);
            }
            if t.refresh_token.is_some() {
                println!("refresh_token: present");
            } else {
                println!("refresh_token: ABSENT (re-auth required when token expires)");
            }
            if let Some(s) = &t.scope {
                println!("scopes:  {s}");
            }

            // Try to resolve identity via GET /me when the token's
            // usable. If the token's stale, just stop here (we don't
            // hit the refresh endpoint from --check; that's the TUI's
            // job).
            if !t.is_expired() {
                let graph = teams::GraphClient::new(t)?;
                match graph.me() {
                    Ok(me) => {
                        println!();
                        println!("me: {} (id {})", me.label(), me.id);
                    }
                    Err(e) => {
                        println!();
                        println!("me: ERROR — {e}");
                        std::process::exit(2);
                    }
                }
            } else {
                println!();
                println!(
                    "(skipping `/me` lookup — token is stale; the TUI will refresh on launch)"
                );
            }
        }
    }

    if cfg.is_err() {
        std::process::exit(2);
    }
    Ok(())
}
