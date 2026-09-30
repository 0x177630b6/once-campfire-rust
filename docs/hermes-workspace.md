# Hermes fork: the Duty Manager Workspace (phase 0)

Phase 0 of the "Duty Manager Workspace" design (the approved mockup, §5 "Feasibility and plan"):
make what already exists visible inside Campfire, **read-only on the Fizzy side** (this phase never
writes to Fizzy).

- **Card chips**: a Fizzy card URL (`…/<account>/cards/<n>`) in a message renders as a small
  Fizzy-style card (number, title, severity, column), with the card's *current* state.
- **Draft buttons**: a Hermes message that shows an incident draft and asks for a confirmation gets
  **File / Edit / Dismiss**. File and Dismiss post `confirm` / `cancel` in the room as the user;
  Edit puts "@Hermes change: " in the composer.
- **Tab bar**: on phones a bottom bar **Home · Report · Chats** (Report = the live voice report of
  the room on screen); a slim rail on wide screens.
- **Home** (`/workspace`): *To confirm* (Hermes drafts nobody answered), *Open incidents* (the
  Incident Log's cards that aren't closed, by column, most severe first), *Mentions* (Fizzy comments
  that @mention you), *Handover* (incident cards changed in the last 12 h). Every card links to Fizzy.

This is a fork-only feature. **It is entirely off unless `FIZZY_URL` and `FIZZY_TOKEN` are set**:
no routes (404), no hooks installed, so every page renders byte for byte what upstream renders.

## Configuration

| Variable | Default | Meaning |
|---|---|---|
| `FIZZY_URL` | unset (off) | Fizzy as the Campfire server reaches it, e.g. `http://fizzy` on the compose network |
| `FIZZY_TOKEN` | unset (off) | A Fizzy personal access token. `read` is enough (phase 0 only GETs). Server-side only: never logged, never sent to browsers, never in an error message (`Secret`'s `Debug` is redacted) |
| `FIZZY_PUBLIC_URL` | `FIZZY_URL` | Fizzy as *browsers* reach it, for every card link, e.g. `https://192.168.0.114:8444` |
| `FIZZY_ACCOUNT` | the token's first account | The account slug (digits), e.g. `897362094` |
| `FIZZY_POLL_S` | `30` | Seconds between polls (minimum 5) |
| `WORKSPACE_INCIDENT_BOARD` | `Incident Log` | The incident board, by name (case-insensitive) or id |

A malformed `FIZZY_URL`, `FIZZY_PUBLIC_URL`, `FIZZY_ACCOUNT` or `FIZZY_POLL_S` fails the boot
(the message never quotes the token). Only one of `FIZZY_URL`/`FIZZY_TOKEN` set = off.

Whose token: the token decides what the workspace can see (Fizzy scopes everything to the token's
user: `Current.user.boards`). Use a token of a user that can see the Incident Log (the Hermes Fizzy
user created by `deploy/fizzy-agent.sh` works; a dedicated `read` token is better). **Everything
that user can see on those boards is shown to every signed-in Campfire user** (chips, Home).

## How it works

```
                 every FIZZY_POLL_S                      render (message partial, layout)
Fizzy JSON API ◀── poll task ──▶ Snapshot (memory) ──▶ WorkspaceHooks ──▶ chips, draft buttons, tab bar
  GET /:acct/boards.json (once), /boards/:id/columns.json,       │
      /cards.json?board_ids[]=… (+ indexed_by=not_now|closed),   ├──▶ GET /workspace (Home)
      /activities.json, /users/:id.json, /cards/:n.json          └──▶ GET /workspace/cards.json (chip refresh)
```

- **Polling** (`campfire_workspace::cache`): Fizzy refuses webhooks to private addresses, so the app
  polls: the incident board's open cards (all pages, at most 10), its "Not Now" and 2 pages of
  recently closed cards, its columns, page 1 of `/activities` (3 on the first poll: card changes on
  every board, and `comment_created` @mentions), the users those mentions name (their email address,
  once each), and at most 10 cards per poll that chips asked for and nothing else brought in (404s
  aren't asked again for 30 min). Requests: `Accept: application/json`, `Authorization: Bearer`,
  pagination by `?page=N` while `Link` says `rel="next"` (the header's own URL carries Fizzy's
  `BASE_URL`, possibly unreachable, so only its presence is used). The HTTP client is the app's
  `integrations::net` (HTTP/1.1, rustls; 10 s connect, 20 s per read, 30 s per request).
- **Fizzy down**: a failed poll keeps the previous picture and records the error. Chips keep their
  last state; Home says "Boards unavailable… retrying every 30 s". The log says it once when Fizzy
  goes down and once when it's back. A missing board (renamed, deleted) is looked up again.
- **Chips** (`campfire_workspace::chips`): at render time, anchors whose `href` is
  `<base>/<account>/cards/<n>` with `<base>` = `FIZZY_URL`, `FIZZY_PUBLIC_URL`, or the base Fizzy
  writes into card URLs (its `BASE_URL`, learned from the cards it returns, e.g.
  `http://localhost:8484`) become `<a class="ws-chip" data-ws-card="n" href="FIZZY_PUBLIC_URL/…">`.
  A card the workspace doesn't know yet keeps its plain link, marked `data-ws-card`. The stored
  message is never changed. Message HTML is cached (the shared fragment cache, per message version),
  so the chip in it is the state at first render: `hermes/workspace.js` asks
  `GET /workspace/cards.json?numbers=…` on load, when messages arrive, and every 60 s, and swaps in
  the current chips (and fills in the ones that were unknown).
- **Drafts** (`campfire_workspace::drafts`): a message from an *active bot* whose text asks to reply
  with a confirmation word (reply/type/say/réponds… + confirm/yes/oui/ok/valide…, in the last 600
  characters) about filing something (Fizzy/card/incident/report/log/file…) is a draft. Its title is
  a `Title:` line, else the first heading, else the first line with " — " (the skill's
  "<type> — <summary> — <location>"), else the first bold text. Severity from `sev-*` or
  `Severity:`/`Gravité :`. Text matching, as planned for phase 0; a structured marker in the bot's
  message would be more reliable (later). In the room the buttons are appended after the body; the
  page marks a draft "answered" once the same bot posted after it. On Home a draft is *to confirm*
  while it's younger than 24 h, the bot hasn't posted in that room since, and nobody answered
  yes/no (a short message with confirm/yes/oui/ok/cancel/no…).
- **Answering**: `POST /workspace/drafts/:message_id/reply` creates the user's message
  `<mention of the bot> confirm` (or `cancel`) through `MessagesController#create`'s path
  (`create_message` → `broadcast_create` → `deliver_webhooks_to_bots`), exactly like the live voice
  report does. The mention is what makes Campfire deliver it to the bot in a shared room (in a
  shared room only mentioned bots get the webhook), so the incident-report skill sees `confirm`.
- **Tab bar** (`campfire_workspace::overlay`): rendered by the layout seam for signed-in, non-bot
  users; it links `hermes/workspace.css` and the `hermes/workspace.js` module itself. Report is
  `/rooms/:id/voice` for the room on screen, else the last room visited, and only when the live
  voice report is on (`GEMINI_API_KEY`). Hidden on the voice page (it has its own bar) and while
  typing in the composer (soft keyboard).
- **Home**: `GET /workspace`, the application layout, a nav with "back to chats" and a "Home"
  title pill. Mentions are matched by email address (the Fizzy user's `email_address` = the
  Campfire user's), last 7 days, not your own comments. Times are `<time>` elements the layout's
  `local-time` controller formats in the browser's time zone.

## Routes and contract

All answer **404 while the workspace is off**, run `ApplicationController`'s chain (session cookie
only, bots denied, `Sec-Fetch-Site` forgery protection on the POST) and live in the fork's route
table (`controllers::HERMES_ROUTES`, tried after the Rails one).

| Route | Answer |
|---|---|
| `GET /workspace` | The Home page (HTML) |
| `GET /workspace/cards.json?numbers=12,13` | `200 {"cards": {"12": "<a class=\"ws-chip\" …>…</a>"}}`: the chips the workspace knows (at most 50 asked); unknown numbers are omitted and fetched at the next poll |
| `POST /workspace/drafts/:message_id/reply` `{"decision": "confirm" \| "dismiss"}` | `201 {"message_id", "reply": "confirm" \| "cancel"}`; `404` when the message isn't in one of the user's rooms; `422 invalid_decision`; `422 not_a_draft` (not from an active bot, or not a draft) |

Phase 0 lets **any member of the room** answer a draft (open question 2 of the mockup).

## Crate layout

| Path | What |
|---|---|
| `crates/workspace` (`campfire_workspace`) | The feature. Depends on no `campfire_*` crate (askama, base64, jiff, regex, serde, serde_json only), so `cargo test -p campfire_workspace --offline` runs anywhere, without libvips or the parity seed |
| `…/src/config.rs` | `WorkspaceConfig::from_lookup`, `Secret` |
| `…/src/fizzy.rs` | Fizzy types (lenient decoding), `HttpClient` trait (the app implements it), `Client` (paths, pagination), `@mention` reading from rich text |
| `…/src/cache.rs` | `Snapshot` and the poll |
| `…/src/chips.rs` | Card URL matching, chip HTML, message decoration |
| `…/src/drafts.rs` | Draft detection, pending drafts, `Decision`, the buttons |
| `…/src/home.rs`, `templates/workspace/home.html`, `_card.html` | Home view-model and template |
| `…/src/overlay.rs`, `templates/workspace/_tab_bar.html`, `_nav.html` | Tab bar, Home nav, page location |
| `…/src/lib.rs` | `Workspace` (state shared by poll, hooks and routes), `ChatSource` trait (the app implements it) |
| `crates/campfire/src/controllers/workspace.rs` | **The adapter**: boot (`build`, `start`: bots, hooks, poll task), the three actions, `ChatSource` over campfire_db (one SQL query: the user's rooms' messages of the last day), `HttpClient` over `integrations::net` |
| `crates/views/src/hermes.rs` (end) | `WorkspaceHooks`, `install_workspace_hooks`, the two seam functions (fork-owned file) |
| `crates/assets/overrides/hermes/workspace.css`, `workspace.js`, `home.svg` | Frontend. Under `hermes/`, which `build.rs` leaves out of `stylesheet_link_tag :all`; the script isn't pinned in the import map (only `pin_all_from` directories are), so upstream pages keep their exact asset tags |

## Seams in upstream-owned files

Every one is marked `Hermes fork:` in the file. Line numbers as of this commit.

| File:line | What | Why |
|---|---|---|
| `Cargo.toml:29-30` | `campfire_workspace = { path = "crates/workspace" }` in `[workspace.dependencies]` | Shared dependency versions live in the root (AGENTS.md). The crate itself is a member through `members = ["crates/*"]` |
| `crates/campfire/Cargo.toml:21-22` | `campfire_workspace.workspace = true` | The adapter uses it |
| `crates/campfire/src/config.rs:67-69` | `Config::workspace: Option<WorkspaceConfig>` | The app's config carries the parsed env |
| `crates/campfire/src/config.rs:213-214` | `workspace: WorkspaceConfig::from_lookup(&get)?` | Parsing lives in the workspace crate |
| `crates/campfire/src/app.rs:47-48` | `AppState::workspace: Option<Arc<Workspace>>` | Actions reach it with `c.app()` |
| `crates/campfire/src/app.rs:112-113, 126` | `let workspace = controllers::workspace::build(&config);` and the field in the `AppState` literal | Built before `config` moves into the state |
| `crates/campfire/src/app.rs:129-130` | `controllers::workspace::start(&app).await;` | Bots, hooks and the poll task need the booted app; while off it only makes sure no hooks are installed |
| `crates/campfire/src/controllers.rs:48-49` | `pub mod workspace;` | The adapter module |
| `crates/campfire/src/controllers.rs:329-332` | Three rows in `HERMES_ROUTES` | The fork's own route table (the Rails table stays identical to `bin/rails routes`) |
| `crates/views/src/messages/presentation.rs:19-20` | `MessageContent::Text { html } => crate::hermes::workspace_message_html(message, html)` | The one hook in message rendering: chips and draft buttons, at render time |
| `crates/views/templates/layouts/application.html:55` | `{{ crate::hermes::workspace_overlay(ctx)\|safe }}` after the lightbox include, same line | The one include in the layout: stylesheet, script, tab bar. Renders `""` while off, and being on the same line adds no whitespace |

Not seams (fork-owned or new files): `crates/workspace/**`, `controllers/workspace.rs`,
`crates/views/src/hermes.rs`, `crates/views/tests/workspace_hooks.rs`,
`crates/assets/overrides/hermes/*`, a test in `crates/assets/tests/reference.rs`, a row in
`crates/assets/OVERRIDES.md`, this document.

## After each upstream merge

1. `git grep -n "Hermes fork" -- ':!docs' ':!crates/workspace'` and compare with the table above:
   every seam still there, once.
2. `cargo test -p campfire_workspace --offline` (the feature, anywhere).
3. `cargo test -p campfire_views --offline`: the goldens (hooks not installed = upstream's bytes)
   and `tests/workspace_hooks.rs` (the overlay lands once after the lightbox; removing the hooks
   gives upstream's bytes back). If upstream moved the lightbox include or the `app-logo` link, move
   the seam and fix that test.
4. `cargo test -p campfire_assets --offline`: the import map and `stylesheet_link_tag :all` still
   equal the reference's; the workspace assets are served, not linked, not pinned.
5. If upstream changed `message_presentation`, `MessageView` (`id`, `creator.id`), `ViewContext`
   (`current_user`, `request_url`, `base_url`, `last_room_visited_id`, `asset_path`),
   `MessagesController#create`'s helpers (`create_message`, `broadcast_create`,
   `deliver_webhooks_to_bots`, `MessageParams`), `Message::find_reachable`, `User::active_bots_ordered`,
   the `messages`/`memberships` columns used by `recent_messages`, or `integrations::net::http`:
   fix `controllers/workspace.rs` (the compiler points at it).
6. If upstream changed the room page's DOM (`.message[data-user-id]` siblings, `form#composer`,
   `[data-composer-target=text]`, the composer controller's `replaceMessageContent`) or the layout
   grid (`body`, `#main-content`, `#sidebar` z-index, the `100ch` breakpoint): check
   `hermes/workspace.js` and `workspace.css` in a browser (phone and desktop, light and dark).
7. `cargo clippy --workspace --all-targets` and the parity gate as usual; with the workspace **off**
   the parity screenshots must not move.

## Tests

- `crates/workspace` (`cargo test -p campfire_workspace`): config (off without both variables,
  defaults, errors never quote the token); Fizzy decoding (lenient cards, column colour objects,
  severity tags, `Link` pagination, @mention sgids incl. nested and avatar fallback); card URL
  matching (known bases, account, terminators, look-alike hosts); chips (escaping, unknown cards
  marked, idempotent); draft detection (the skill's English draft as the bridge posts it, French,
  headed drafts; filed/other messages ignored), decisions and their reply HTML, pending drafts
  (answered, superseded, expired); Home (grouping and ordering, mentions by email, handover window,
  drafts need a known bot, rendering and escaping, Fizzy down/waiting); the tab bar; and a full poll
  against a fake Fizzy (both pages, activity cards, learned origins, mention emails, token only in
  the `Authorization` header, cards asked by chips fetched next poll, Fizzy down keeps the picture,
  missing board looked up again).
- `crates/views/tests/workspace_hooks.rs`: hooks off = upstream bytes; on = overlay once in place
  and message hook applied, nothing else changed; removed = upstream bytes again.
- `crates/assets/tests/reference.rs`: workspace assets served, not in `stylesheet_link_tag :all`,
  not in the import map.
- `crates/campfire/src/controllers/workspace.rs` (needs the app to link, i.e. libvips; runs in CI):
  route recognition, and `FizzyHttp` against a fake server (headers, query, `Link`, 404, errors
  without the token).

Not covered by automated tests: the three actions end to end (they need the parity seed, like the
other request tests), `hermes/workspace.js` (no JS test harness in the repo), and the look of the
CSS. Check in a browser after deploying: a chip in a room, File/Dismiss/Edit on a real Hermes draft
(the Edit prefill relies on Lexxy accepting an `<action-text-attachment … content="…">` in
`replaceMessageContent`), the tab bar on a phone (composer not covered, hidden while typing), Home.

## Deliberately deferred or different from the mockup

- **No badge counts** on the tab bar (they'd need a per-user query on every page); Home shows the
  counts. **Three tabs**, not five: Boards and Hermes are phases 1 and 2.
- **Report from Home** goes to the last room visited instead of asking "Where?".
- **Open incidents** = every open card of the incident board, not "cards I own + unowned" (per the
  task); owners are shown on each card.
- **Mentions** come from Fizzy comments only; Campfire @mentions already notify through Campfire.
- **In-memory cache**, not SQLite: the first poll after a restart rebuilds it (activity pages 1–3,
  so mentions older than that aren't recovered).
- **Chips on every message**, not only Hermes's; unknown card numbers are fetched lazily.
- **Anyone in the room** can answer a draft; "only the author or the Duty Manager" is an open
  question for phase 1.
