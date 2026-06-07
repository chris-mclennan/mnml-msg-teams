# mnml-msg-teams

A terminal browser + composer for [Microsoft Teams](https://teams.microsoft.com/) — list your joined teams and their channels, walk your chats, search messages across Teams, and post / reply / react without leaving the keyboard. The first **messaging** sibling in the mnml family. Sits next to the observability / AWS / DB / forge / tracker siblings.

Runs **standalone in any terminal**. v0.2 will add blit-host mode so mnml can host it as a native pane.

```
┌─ teams ───────────────────────────────────────────────────────────────┐
│ ▸1.teams (4)  2.chats (12)  3.search (0)  4.threads (0)               │
└───────────────────────────────────────────────────────────────────────┘
┌─ teams (4) ────────────────────┐ ┌─ channel ──────────────────────────┐
│ ▸ ▾ Engineering                │ │  06-07 10:14 · Alice               │
│     ▸ General                  │ │     ship it                        │
│     ▸ Frontend                 │ │  06-07 10:16 · Bob                 │
│     ▸ Backend                  │ │     LGTM                           │
│   ▸ Design                     │ │     [👍 3] [❤ 1]                   │
│   ▸ Operations                 │ │                                    │
│   ▸ Customer Success           │ │  06-07 10:34 · Alice               │
└────────────────────────────────┘ └────────────────────────────────────┘
  1-9 tab · ↑↓/jk move · Enter open · / search · p post · R react · T thread · y permalink · r refresh · q quit
```

## Install

```sh
cargo install --git https://github.com/chris-mclennan/mnml-msg-teams
```

## Auth setup

Microsoft Graph requires OAuth 2.0 — there's no "paste an API key" path. `mnml-msg-teams` uses the **device-code flow**, the same one `az login` uses. On first run:

```sh
mnml-msg-teams auth
```

This prints a verification URL and a one-time code (also copied to your clipboard), opens your browser, and polls until you've authenticated.

### Why does Microsoft trust us?

This tool ships with Microsoft's **public Azure CLI client ID** (`04b07795-8ddb-461a-bbee-02f9e1bf7b46`) — the same one `az` uses. If you've already accepted Azure CLI's consent screen in your tenant, you won't see a second consent prompt. The trade-off is that the consent screen says "Microsoft Azure CLI" rather than "mnml-msg-teams" — that's the honest cost of not registering our own multi-tenant app.

### Scopes requested

```
User.Read
ChatMessage.Read    ChatMessage.Send
ChannelMessage.Read.All  ChannelMessage.Send
Channel.ReadBasic.All    Team.ReadBasic.All
Chat.Read  Chat.ReadWrite
offline_access      (unlocks the refresh-token)
```

If your tenant's admin restricts any of these, the login will fail with a clear OAuth error.

### Token storage

Tokens are persisted at `~/.config/mnml-msg-teams/token.json` with file mode **0600**. The blob contains the access-token (~1h TTL), the refresh-token (typically valid ~90 days), and the absolute expiry timestamp. Every Graph call checks for impending expiry and refreshes proactively. A `401 InvalidAuthenticationToken` triggers a one-shot retry-after-refresh.

To revoke locally:

```sh
mnml-msg-teams auth --logout
```

Treat these tokens like a password — anyone with the file gets the scopes you consented to.

## Config

```sh
mnml-msg-teams              # first run writes the template
```

Then edit `~/.config/mnml-msg-teams/config.toml`:

```toml
refresh_interval_secs = 60

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
```

### Tab kinds

| `kind` | What it shows |
|---|---|
| `teams` | Your joined teams (`GET /me/joinedTeams`). Press `Enter` to expand channels (lazy-loaded). |
| `chats` | 1:1 + group chats, newest first (`GET /me/chats?$expand=members&$orderby=lastUpdatedDateTime%20desc&$top=50`). |
| `search` | Press `/` to enter a query; runs `POST /search/query` over `chatMessage` entities. |
| `threads` | v0.1 stub. Planned: focused-thread view of the currently-selected channel/chat. |

`mnml-msg-teams --check` prints the resolved config + token state + (if the token's still valid) `GET /me` to confirm identity + tenant.

## Layout

- **Tab strip:** one per `[[tabs]]`, with item-count badge.
- **List (left, 45%):**
  - **teams:** team display name + description; expandable to channels with an inline `▸ /  ▾` arrow.
  - **chats:** chat topic, or (1:1) the other participant's name; last activity timestamp on the right.
  - **search:** results show author + first 80 chars of the message body.
  - **threads:** v0.1 stub.
- **Detail (right, 55%):** focused channel or chat scrollback (~30 recent messages, chronological).
  - Each message: `HH:MM · username · body-text-stripped`. HTML stripped to plain text; basic entities (`&amp;` / `&lt;` / `&gt;` / `&nbsp;`) decoded.
  - System messages (`messageType: "systemEventMessage"`) shown dimmed as one-liners.
  - Reactions: `[👍 3] [❤ 1]` chips after each body.

## Keys

| Chord | Action |
|---|---|
| `1`-`9` | Switch to that tab |
| `Tab` / `BackTab` | Cycle tabs |
| `↑` / `k`, `↓` / `j` | Move selection |
| `PgUp` / `PgDn` | Jump 10 rows |
| `g` / `G` | Top / bottom |
| `Enter` | On a team — expand / collapse channels. On a channel or chat — focus + load scrollback. On a search hit — open permalink. |
| `/` | Search mode (search tab only). Type, `Enter` to submit, `Esc` to cancel. |
| `p` | Post a message to the focused channel or chat. `Ctrl+S` to send, `Esc` to cancel. |
| `T` | Threaded reply to the first message in the focused channel scrollback. |
| `R` | React (👍 like) to the first message in the focused chat scrollback. (v0.1 wires `like` directly; the full picker is v0.2.) |
| `y` | Yank — message permalink (`webUrl`) for a message, channel URL for a channel, `team:<id>` / `chat:<id>` otherwise. |
| `r` | Refresh active tab |
| `q` / `Esc` / `Ctrl+C` | Quit |

## API endpoints used

| Surface | Endpoint |
|---|---|
| Identity | `GET /me`, `GET /users/{id}?$select=id,displayName` (lazy cache) |
| Teams | `GET /me/joinedTeams`, `GET /teams/{tid}/channels` |
| Chats | `GET /me/chats?$expand=members&$orderby=lastUpdatedDateTime%20desc&$top=50` |
| Messages | `GET /teams/{tid}/channels/{cid}/messages?$top=30`, `GET /me/chats/{cid}/messages?$top=30` |
| Post | `POST /teams/{tid}/channels/{cid}/messages`, `POST /chats/{cid}/messages` |
| Reply | `POST /teams/{tid}/channels/{cid}/messages/{mid}/replies` |
| React | `POST /me/chats/{cid}/messages/{mid}/setReaction` |
| Search | `POST /search/query` (entityTypes=`chatMessage`) |
| Auth | `POST /oauth2/v2.0/devicecode`, `POST /oauth2/v2.0/token` |

All Graph requests carry `Authorization: Bearer <access_token>`. On `401`, the client refreshes via `grant_type=refresh_token` and retries once.

## Error handling

- Graph errors (`{"error": {"code": "...", "message": "..."}}`) surface as `graph: {code}: {msg}`.
- `429 Too Many Requests` surfaces with the `Retry-After` value when present; v0.1 doesn't auto-retry.
- `401 InvalidAuthenticationToken` triggers a one-shot refresh + retry.

## Not yet supported

- **Rich-text rendering** — Teams returns HTML; v0.1 strips to plain text. v0.2 may render bold/italic/links.
- **File attachments** — `GET /...` content download + `POST /...` upload.
- **Channel pins**, calendar / meeting integration.
- **Multi-tenant switching** — v0.1 uses the `common` tenant; one identity at a time.
- **Thread tab auto-population** — the `threads` tab is a v0.1 placeholder.
- **Reaction picker** — v0.1 wires `R` directly to `like`; the full picker (`l` / `h` / `L` / `s` / `d` / `a`) is v0.2.
- **Cursor pagination** — v0.1 caps `joined-teams` at 500 (a safety valve, not a real limit).
- **Blit-host mode** — v0.2 will add `--blit <socket>` for mnml-hosted pane mode.

## Security

The persisted token grants whatever Graph access you consented to — for the default scope set, that includes reading + posting to every team and chat you can reach in your tenant. Treat `~/.config/mnml-msg-teams/token.json` like a password. The file is created with mode `0600`; verify your home directory permissions are similarly tight. `auth --logout` deletes it.

## Status

**v0.1** — teams + channels (lazy-expand), chats with member resolution, message search, channel + chat scrollback with reactions chips, post to channel/chat, threaded reply in channels, like reaction on chats, permalink yank, OAuth device-code flow with refresh + persistence. Standalone only.

## Source

[github.com/chris-mclennan/mnml-msg-teams](https://github.com/chris-mclennan/mnml-msg-teams). MIT.
