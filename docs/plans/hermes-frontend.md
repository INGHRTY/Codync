# Hermes frontend: cloud + local bots in one room

Status: **Phases 1–2 done and verified live** against the user's VPS (2026-10-30). Phases 3–7 planned.
Decisions taken with the user on 2026-10-30.

Codync becomes the front end for Hermes: bots that run on the user's own VPS (Hermes `api_server`) and bots
that run on this computer (`hermes acp`) sit in the same roster, the same group chats, and the same rooms.

## Decisions

| Question | Answer |
| --- | --- |
| What is "cloud Hermes"? | The user's own VPS running Hermes with `API_SERVER_ENABLED`. Not Nous-hosted Hermes Cloud, not Hermes Desktop's `hermes serve` (`/api/ws`). |
| Which transport? | Hermes' HTTP API: `POST /api/sessions`, `POST /api/sessions/{id}/chat/stream` (SSE), `POST /v1/runs/{id}/approval|steer|stop`, `GET /v1/models`, `GET /api/model/options`. ACP over SSH is **not** used: it needs `codync-host` on the VPS and can't reach Nous-hosted instances. |
| Profiles | One Hermes profile per bot, **local and remote**. Local: `hermes -p <profile> acp`. Remote: `/p/<profile>/…` URL prefix with that profile's own `API_SERVER_KEY`. |
| Appearance | Layout and interaction follow the installed Grok Bot 0.78.1 (measured, see [Grok Bot deltas](#grok-bot-deltas)). |

Hermes facts this rests on (verified against Hermes 0.22.5+2884 and its source):

- `hermes acp` takes `-p <profile>` (pre-parsed before imports) and reads `HERMES_PROFILE`; an unknown profile
  fails loudly, so the host must create it first.
- With `gateway.multiplex_profiles` on (the default), one listener serves `/p/<profile>/…`, **and each profile's
  prefix needs that profile's own `API_SERVER_KEY`** (a default-profile key is rejected). A profile with no key
  fails closed.
- The API server's sessions do **not** take session-level MCP servers
  (`_get_platform_tools(config, "api_server", include_default_mcp_servers=False)`). ACP does
  (`new_session {mcpServers}` → `_register_session_mcp_servers`).
- `system_message` (alias `instructions`) is accepted per request and becomes the ephemeral system prompt.
- Approval choices are `once | session | always | deny` (`api_server_runs._approval_event_choices`).
- Session chat events: `run.started`, `message.started`, `tool.started`, `tool.completed`, `assistant.delta`,
  `assistant.commentary`, `assistant.completed`, `approval.request`, `run.completed|failed|cancelled`, `done`.

## Phase 0 — probe results (2026-10-30, live against the user's VPS)

The VPS (`contabo` → ing-vps, Tailscale `100.101.122.109`) runs Hermes **0.22.2** with
`gateway.multiplex_profiles: true`. Its api_server was **off**; it was enabled in `~/.hermes/.env`
(`API_SERVER_ENABLED=true`, `API_SERVER_KEY`, `API_SERVER_HOST=100.101.122.109`, port `8642`, a backup
saved as `.env.bak-codync-*`) and the gateway restarted. It listens on the **Tailscale address only**,
so it is not exposed to the internet.

What the live probe established:

| Item | Result |
| --- | --- |
| Reachability | `GET /v1/models` answers `401` without the key from the Mac over Tailscale, `200` with it. |
| Capabilities | `session_chat_streaming`, `run_approval_response`, `approval_events`, `tool_progress_events`, `session_fork`, `session_model_lock`, `session_continuity_header: X-Hermes-Session-Id`. `memory_write_api: false`, `admin_config_rw: false`. |
| Session create | `POST /api/sessions {title}` → `{session:{id}}`. **Titles must be unique** (`invalid_title` on reuse). |
| Streaming | `POST /api/sessions/{id}/chat/stream` → `text/event-stream`, `event:` line carries the type. Observed order: `run.started`, `message.started`, `tool.started`, `tool.completed`, `assistant.delta` (many), `tool.progress` (`_thinking`), `assistant.completed`, `run.completed`, `done`. |
| Tools | `terminal` runs on the VPS; `tool.started` carries `tool_name`, `preview`, `args`. |
| Approvals | Fires `approval.request` with `choices` (`once`/`session`/`always`/`deny`), `pattern_key`, `description`, `request_id`. `POST /v1/runs/{run_id}/approval {choice:"deny"}` → `{resolved: 1}` and the turn continues. Smart mode auto-approves low-risk commands (`approvals.mode: smart`), so a card only appears for genuinely flagged ones (e.g. `curl … | bash`). |
| Profile isolation | Created `codync-a` (`--clone`), gave it its own `API_SERVER_KEY`: `/p/codync-a/v1/models` → `200` with that key, **`401` with the default key**. A session under the prefix ran and the agent reported its profile name as `codync-a`. |
| Profile creation | `hermes profile create <name> --clone` copies config/skills; it does **not** copy an API key, so Codync must write `API_SERVER_KEY` into `~/.hermes/profiles/<name>/.env` itself. |

Consequences for the plan: nothing in Phases 1–4 changes, except that a remote bot's profile must exist
**and hold its own key** before `/p/<profile>/` works (fail closed otherwise). Profile creation and key
writing belong to the host's connection setup, not to the user.

## Consequences

- **Remote bots get no Codync MCP tools in v1.** `chat`, `team`, `memory`, `manage`, `routines`, `computer`
  are loopback stdio servers on this computer; the VPS can't reach them. Remote bots therefore don't call
  `send_message`, share files or publish artifacts on their own. Room replies, @-mentions, approvals, stop and
  push all still work, because the host drives those. Parity comes in Phase 6 through an MCP bridge over
  `codync-network`.
- **Remote bots have no fallback chain in v1** (`fallbacks` must stay empty; the editor hides the section).
- **A remote bot can't be the lead bot** in v1.
- **Group attachments:** images travel as `image_url` data parts; other files are named in the room prompt as
  unavailable, with a notice, until an upload endpoint is verified.

## Model

```rust
// BotConfig, additive
/// A Hermes connection on another computer; `None` = this computer's ACP agent.
connection: Option<String>,
/// The Hermes profile this bot runs as (local and remote). `None` = host picks `codync-<slug>`.
hermes_profile: Option<String>,
```

```rust
// kv `connections.<id>`, key in the vault (never in the bot JSON)
struct Connection {
    id: String,
    name: String,
    base_url: String,      // https://vps.example.com (or /p/<profile> base, resolved per bot)
    key_slot: String,      // market::vault slot
    verified_at: Option<i64>,
    profiles: Vec<String>, // what the gateway reports it serves, when it can
}
```

## Phases

### Phase 1 — Host skeleton

- `host/src/store/connections.rs`: list/save/delete, `rev` + `emit {"type":"connection"}`; key through
  `market::vault`.
- `host/src/remote/hermes.rs`: `reqwest` client + hand-rolled SSE reader (same style as `agent/acp.rs`: keep
  payloads as `serde_json::Value`), `capabilities()`, `models()`, `model_options()`, `create_session()`,
  `chat_stream()`, `approval()`, `steer()`, `stop()`; errors as an enum (`Unreachable | Unauthorized |
  NoModel | Gateway(String)`), not strings.
- `host/src/agent/bot/engine/mod.rs`: `enum Engine { Acp(Conn), Hermes(remote::Conn) }` with
  `ensure_session / prompt / cancel / respond_permission / stop / set_model`. `turn.rs`, `updates.rs`,
  `session.rs`, `lifecycle.rs` dispatch through it; the ACP branch keeps its code.
- API: `listConnections`, `saveConnection`, `testConnection`, `deleteConnection`. Saving, testing and deleting
  are loopback only (a key is entered at the computer); `listConnections` also answers a phone, so it can show
  where a bot runs. `hello` gains `connections` (names and addresses, never keys).
- `Hub::create_bot` / `update_bot`: accept `connection`; a remote bot is `backend: hermes`, skips local
  install checks, and gets only its attachment/artifact folders.

### Phase 2 — Remote engine

Landed as `host/src/remote/hermes/` (client, connections, events, provision) plus
`agent/bot/conn.rs` (`Conn` = ACP process or gateway) and `agent/bot/gateway.rs` (the actor's gateway side).
What the live run established, beyond the design above:

| Finding | Handling |
| --- | --- |
| Frame order is `assistant.completed` **then** `run.completed` | Only `run.completed\|failed\|cancelled` answers the turn; `assistant.completed` just marks the reply delivered, so a stream cut right after it still ends the turn cleanly. |
| `session/update`s need a `sessionId` | The actor drops updates for other sessions; `events.rs` stamps every one with the turn's gateway session. |
| `tool.progress` on `_thinking` **replays the whole reply** | Not reasoning; ignored, so the trace doesn't show a duplicate thought bubble. |
| Replies open with blank lines | Trimmed before the first word only. |
| `approvals.mode` is `smart` by default on that VPS | Low-risk commands are auto-approved, so cards appear only for flagged ones (`rm -rf /…`, `curl … \| bash`). `manual` makes every dangerous command ask. |
| Cards carry Hermes' own choices | `once`, `session`, `always`, `deny`, rendered as the four options; the answer goes to `POST /v1/runs/{run}/approval`. |
| A bot's `permission: auto` auto-answers cards | `ask` is needed to see one. |

Verified end to end on the VPS: streamed reply → `agent` entry, `terminal` tool entry completed, a permission
card raised and answered (`selected: once`) with the run continuing, and a clean `final: true` reply.


| Hermes | Codync |
| --- | --- |
| `run.started` | runtime `working`, remember `run_id` |
| `assistant.delta` | `EntryKind::Agent` streamed update |
| `assistant.commentary` | `EntryKind::Thought` |
| `tool.started` / `tool.completed` | `EntryKind::Tool` |
| `approval.request` | permission card; answer → `POST /v1/runs/{run_id}/approval` |
| `run.completed` | `final: true`, done push |
| `run.failed` / `cancelled` | error notice, failed push |
| `done` | close the stream, clear inflight |

- Each turn sends Codync's rendered instructions as `system_message` (a gateway has no session-level system
  prompt, so `compose` returns them and `Conn::send` puts them in the request).

### Phase 3 — Rooms with cloud and local members

- `chat/group.rs::valid_members`: accept agent bots that carry a `connection`; still no nested groups, no
  duplicates.
- Room prompt unchanged (it is plain text plus the member list).
- `chat::lead`, `fallbacks`, `manage_team` refuse remote bots.
- Tests: one local + one remote member, a round with an @-mention, a pass, and a late answer.

### Phase 4 — Four clients

- Settings → **Hermes connections** page: list, name, base URL, key (system keychain on iOS, main-process
  safe storage on desktop), `testConnection` in three states.
- Bot editor: with `backend: hermes`, a Local / Cloud switch, then connection + profile + model
  (`/v1/models`); `fastMode` hidden for remote.
- Cloud / local badge on avatars and roster rows; the room shows which side answered.
- Group member picker offers remote bots with the badge.
- TUI: a **Hermes connections** sheet (`H`, or the palette) — the list with each gateway's address, profile,
  reach, and its bots with the profile each runs as; a gateway's form (name, address, profile, key, SSH host)
  with `^t` test in three states, `^s` save, `x` remove. Bot settings gain **Runs on** (this computer, or a
  gateway) and **Profile**; a cloud bot hides Agent, Fast mode, Fallbacks, Options and Workspace, and saves as
  Hermes with no local workspace. `[cloud]` in roster previews and chat headers, `☁` beside authors in a room,
  and the group picker marks a gateway's bots.

### Phase 5 — Appearance (Grok Bot)

See [Grok Bot deltas](#grok-bot-deltas). Every item lands in iOS, Android, desktop and TUI together.

### Phase 6 — Hermes adaptation

- Local profile per bot: `hermes profile create codync-<slug> --clone --no-alias` once
  (`spawn_blocking`), then `hermes -p codync-<slug> acp`; fall back to the default profile with a visible
  notice, never silently.
- `own_memory::shared("hermes")` becomes per-bot once profiles are isolated.
- Approval policy: `Permission::Auto` → `edit_approval_policy` automatic, `Ask` → per request.
- Model via `set_config_option` (`model`, `reasoning_effort`), never `session/set_model`.
- Never call `session/close` (Hermes advertises no `close` capability); absolute paths only (no
  `additionalDirectories`).
- Usage: show "not supported" when Hermes reports no rate-limit meta.
- MCP bridge over `codync-network` so remote bots get `chat`/`memory`/`team`.

Landed against the local Hermes 0.22.5 (2026-10-31): the per-bot profile (`agent/hermes_profile.rs`, verified by
a bot starting as `hermes -p codync-… acp` with its own `memories` folder), `shared` dropping Hermes, the
approval policy (`edit_approval_policy` → `session` for a bot that never asks), the model through
`session/set_config_option` (Hermes lists `model` in `configOptions`), no `session/close` or
`additionalDirectories` (Hermes advertises neither, and the host already follows the capabilities), and the
Usage list saying Hermes reports none. The MCP bridge is still open.

### Phase 7 — Release

`2.31.0` (minor: new methods + additive bot fields; no `minApp` bump). `after-change` workflow, merge into
`dev`, e2e on macOS desktop with the user's VPS.

## Grok Bot deltas

Measured from `/Applications/Grok Bot.app` (0.78.1, `app.asar` → `dist/renderer/assets/index-*.css`) against
`apps/desktop/src/renderer/styles/theme.css` and `views/thread/thread.css`. The app's own window could not be
captured (it is a menu-bar app and the display was locked), so these are the bundle's declarations; a live
side-by-side pass is part of Phase 5. Light-mode values only where marked: Grok injects its dark tokens at
runtime and the bundle carries no dark block.

| Token | Grok Bot | Codync today | Change |
| --- | --- | --- | --- |
| Sidebar width | declares `180px`, `48px` collapsed (surface to confirm live) | `266px` default, `76px` rail | Confirm, then narrow: the chat gets the room. |
| Chat min width | `--sand-chat-min-width: 424px` | `760px` window min | Lower the window minimum. |
| Conversation column | bubble max `min(640px, 100%)` | `820px` column | Narrow to 640. |
| Conversation text | `14px / 22px` | `12px` base, scaled | Raise the default text size. |
| Bubble radius | prose `18px`, one-line `36px` height | `16px` (`--radius-l`) | `18px`, one-line bubbles at 36px. |
| **Bubble fills** | user `#1c1c1c` with white text; agent `#1c1c1c0f` (a 6% tint) | user `#e2e2e2`, agent `#f0f0f0` (both tints) | **The user's own messages become the only solid bubbles; agent replies become quiet tints.** The single most visible change; it touches `Palette` (iOS/Android) and `theme.css` together. Grok's dark-mode values are injected at runtime, so match the *relationship* (solid user, tinted agent) and keep Codync's own light/dark pairs. |
| Radius scale | 2 / 4 / 6 / 8 / 12 / 14 / 16 / 18 | 6 / 10 / 12 / 16 / 22 | Adopt 4 / 8 / 12 / 16 / 18. |
| Spacing scale | 4 / 8 / 12 / 16 / 24 / 32 | ad hoc | Adopt as `--space-*`. |
| Plan/todo tray | floating tray inset by the corner rule over the composer | plan only inside the trace sheet | **New:** the running turn's plan floats above the composer, dismissible, and opens the trace. |
| Opposed inset | `clamp(0px, (100% - 480px) * .4, 20%)` | fixed 56px / 88px reserves | Let wide windows breathe. |

Interaction follow-ups that come with it: a hover/focus action row per message (Codync has it), the composer's
attachment tray and `/` command panel kept (Codync has them), and the plan tray wired to the existing
`EntryKind::Plan` entries so the four clients render the same thing.

## Open items

1. ~~The VPS base URL and its `API_SERVER_KEY`~~ — done in Phase 0; the connection is
   `http://100.101.122.109:8642` (Tailscale) and the key lives in `~/.hermes/.env` on the VPS.
2. ~~Whether the VPS gateway has `gateway.multiplex_profiles` on~~ — yes; `/p/<profile>/` works, and each
   profile needs its own `API_SERVER_KEY`.
3. Whether the VPS gateway accepts file uploads. The capability list shows no upload feature
   (`artifact_upload` is browser-extension only: 10 MiB, allowlisted MIME types); until one is confirmed,
   images travel as `image_url` parts and other files are named in the prompt.
