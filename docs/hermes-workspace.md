# Hermes fork: the Duty Manager Workspace (phases 0 and 1)

The "Duty Manager Workspace" design (the approved mockup, §5 "Feasibility and plan"). **Phase 0**
made what already exists visible inside Campfire, read-only. **Phase 1** ("Work incidents without
leaving the app") works the incident cards from Campfire: a room's cards panel, the board, the
card sheet, and "Create a card from this message". Both are described here; phase 1 is
[below](#phase-1-working-incidents).

## Phase 0: seeing incidents

- **Card chips**: a Fizzy card URL (`…/<account>/cards/<n>`) of an incident-board card in a message
  renders as a small Fizzy-style card (number, title, severity, column), with the card's *current*
  state. A link to a card on any other board stays a plain link.
- **Draft buttons**: a Hermes message that shows an incident draft and asks for a confirmation gets
  **File / Edit / Dismiss**. File and Dismiss post `confirm` / `cancel` in the room as the user;
  Edit puts "@Hermes change: " in the composer.
- **Tab bar**: on phones a bottom bar **Home · Chats · Report · Boards** (Report = the live voice
  report of the room on screen; Boards since phase 1); a slim rail on wide screens.
- **Home** (`/workspace`): *To confirm* (Hermes drafts nobody answered), *Open incidents* (the
  Incident Log's cards that aren't closed, by column, most severe first), *Mentions* (Fizzy comments
  on incident-board cards that @mention you), *Handover* (incident cards changed in the last 12 h).
  Every card links to Fizzy.

This is a fork-only feature. **It is entirely off unless `FIZZY_URL` and `FIZZY_TOKEN` are set**:
no routes (404), no hooks installed, so every page renders byte for byte what upstream renders.

## Configuration

| Variable | Default | Meaning |
|---|---|---|
| `FIZZY_URL` | unset (off) | Fizzy as the Campfire server reaches it, e.g. `http://fizzy` on the compose network |
| `FIZZY_TOKEN` | unset (off) | A Fizzy personal access token. **Phase 1 writes with it: it needs `write` permission** (a `read` token only reads; writes then fail with "the workspace's Fizzy token can't write", Fizzy answering 401). Server-side only: never logged, never sent to browsers, never in an error message (`Secret`'s `Debug` is redacted) |
| `FIZZY_PUBLIC_URL` | `FIZZY_URL` | Fizzy as *browsers* reach it, for every card link, e.g. `https://192.168.0.114:8444` |
| `FIZZY_ACCOUNT` | the token's first account | The account slug (digits), e.g. `897362094` |
| `FIZZY_POLL_S` | `30` | Seconds between polls (minimum 5) |
| `WORKSPACE_INCIDENT_BOARD` | `Incident Log` | The incident board, by name (case-insensitive) or id |

A malformed `FIZZY_URL`, `FIZZY_PUBLIC_URL`, `FIZZY_ACCOUNT` or `FIZZY_POLL_S` fails the boot
(the message never quotes the token). Only one of `FIZZY_URL`/`FIZZY_TOKEN` set = off. Phase 1 adds
no variable: its settings are edited in the app and kept in `<CAMPFIRE_STORAGE_PATH>/hermes/
workspace.json` ([Settings](#settings)).

Whose token: the token decides what the workspace can see (Fizzy scopes everything to the token's
user: `Current.user.boards`) and, in phase 1, who Fizzy shows as the author of every change. Use a
`write` token of the Hermes Fizzy user created by `deploy/fizzy-agent.sh` (it can see the incident
board). **Everything the
token sees on the incident board is shown to every signed-in user** (chips, Home). Nothing from its
other boards is: their cards and comments are dropped as the poll reads them (the activity feed is
account-wide, and a chip can ask for any card number), so they never become chips or Home entries.

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
  recently closed cards, its columns, page 1 of `/activities` (3 on the first poll: card changes and
  `comment_created` @mentions; those of other boards are dropped), the users those mentions name
  (their email address, once each), and at most 10 cards per poll that chips asked for and nothing
  else brought in (404s and cards on other boards aren't asked again for 30 min). A single card or
  user lookup that fails doesn't fail the poll: the lists still refresh, the lookup is retried next
  poll, and the log says it once (`some Fizzy lookups failed`, never with the token). Requests: `Accept: application/json`, `Authorization: Bearer`,
  pagination by `?page=N` while `Link` says `rel="next"` (the header's own URL carries Fizzy's
  `BASE_URL`, possibly unreachable, so only its presence is used). The HTTP client is the app's
  `integrations::net` (HTTP/1.1, rustls; 10 s connect, 20 s per read, 30 s per request).
- **Fizzy down**: a failed poll keeps the previous picture and records the error. Chips keep their
  last state; Home says "Boards unavailable… retrying every 30 s". The log says it once when Fizzy
  goes down and once when it's back. A missing board (renamed, deleted) is looked up again.
- **Chips** (`campfire_workspace::chips`): at render time, anchors whose `href` is
  `<base>/<account>/cards/<n>` with `<base>` = `FIZZY_URL`, `FIZZY_PUBLIC_URL`, or the base Fizzy
  writes into card URLs (its `BASE_URL`, learned from the cards it returns, e.g.
  `http://localhost:8484`) become `<a class="ws-chip" data-ws-card="n" href="FIZZY_PUBLIC_URL/…">`
  when the card is on the incident board. A card the workspace doesn't know yet, or that is on
  another board, keeps its plain link, marked `data-ws-card`. The stored
  message is never changed. Message HTML is cached (the shared fragment cache, per message version),
  so the chip in it is the state at first render: `hermes/workspace.js` asks
  `GET /workspace/cards.json?numbers=…` on load, when messages arrive, and every 60 s, and swaps in
  the current chips (and fills in the ones that were unknown). A reply that was redirected (signed
  out) is ignored.
- **Drafts** (`campfire_workspace::drafts`): a message from an *active bot* is a draft when it has
  the incident-report skill's step-4 shape: a preview of the card (a `Title:` line, the skill's
  "<type> — <summary> — <location>" title, or two of the template's header fields such as
  `Type:`/`Severity:`/`Location:`), then, in the last 600 characters, an invitation to reply with a
  confirmation word (reply/type/say/réponds… + confirm/yes/oui/ok/valide…) whose sentence says it's
  *to file it* (to file/create/log…, and I'll log…, pour créer/enregistrer…). "Card #12 filed … Reply
  ok if you also want me to notify maintenance", "Say yes and I'll create it" with no card shown, or
  "just reply yes" aren't drafts. Its title is
  a `Title:` line, else the first heading, else the first line with " — " (the skill's
  "<type> — <summary> — <location>"), else the first bold text. Severity from `sev-*` or
  `Severity:`/`Gravité :`. Text matching, as planned for phase 0; a structured marker in the bot's
  message would be more reliable (later). In the room the buttons are appended after the body; the
  page marks a draft "answered" once the same bot posted after it. On Home a draft is *to confirm*
  while it's younger than 24 h, the bot hasn't posted in that room since, and nobody answered
  yes/no: a message that is exactly one decision word (confirm/yes/oui/ok/cancel/no…, optionally
  after a mention), or that mentions the bot (the composer's mention or "@Name") in at most six
  words with one. "ok thanks", "no worries" or "go ahead with lunch" aren't answers.
- **Answering**: `POST /workspace/drafts/:message_id/reply` creates the user's message
  `<mention of the bot> confirm` (or `cancel`) through `MessagesController#create`'s path
  (`create_message` → `broadcast_create` → `deliver_webhooks_to_bots`), exactly like the live voice
  report does. The mention is what makes Campfire deliver it to the bot in a shared room (in a
  shared room only mentioned bots get the webhook), so the incident-report skill sees `confirm`.
  The page says "Sent" only on a `201` that wasn't redirected; an expired session (the chain
  redirects to the sign-in page, which `fetch` follows) says to sign in again.
- **Tab bar** (`campfire_workspace::overlay`): rendered by the layout seam for signed-in, non-bot
  users; it links `hermes/workspace.css` and the `hermes/workspace.js` module itself. Report is
  `/rooms/:id/voice` for the room on screen, else the last room visited, and only when the live
  voice report is on (`GEMINI_API_KEY`). Hidden on the voice page (it has its own bar) and while
  typing in the composer (soft keyboard).
- **Home**: `GET /workspace`, the application layout, a nav with "back to chats" and a "Home"
  title pill. Mentions are comments on incident-board cards only, matched by email address (the
  Fizzy user's `email_address` = the Campfire user's), last 7 days, not your own comments. Times
  are `<time>` elements (date and time) the layout's `local-time` controller formats in the
  browser's time zone. While Fizzy hasn't answered yet the page says "Boards unavailable" only; the
  error itself is in the server log.
- **Known limitation (mentions)**: Campfire lets users change their email address without
  verification, so someone who sets a colleague's address sees that colleague's mentions. That is
  why mentions are limited to the incident board, whose content this feature shows every signed-in
  user anyway; it still exposes which incident-board comments name that colleague, and their
  excerpts. Matching on a verified identity (a Fizzy ↔ Campfire user link) is for a later phase.

## Phase 1: working incidents

Decided by the owner: **one incident board** (`WORKSPACE_INCIDENT_BOARD`, currently "Incidents");
**departments are Fizzy tags** on its cards, not boards. Fizzy's board settings (auto-postpone…)
are never changed.

- **Room cards panel**: a room linked to one or more departments (settings) gets a "N cards"
  button in its nav; it opens a panel (a column beside the conversation on wide screens, a sheet
  on phones) listing the incident board's open cards tagged with those departments, most severe
  first, with "New card" and "Open on the board". A room not linked shows nothing. Open or closed
  is remembered per browser (`localStorage`). The panel comes with the layout's overlay (from the
  cache, no Fizzy request) and is refreshed after each change (`GET /workspace/rooms/:id/panel`).
- **Board** (`/workspace/board`): "New" (Fizzy's "Maybe?"), the board's columns, "Monitoring"
  ("Not Now") and "Closed" (the 30 most recently closed), each most severe first. Filters: one
  department tag and any severities (`?dept=engineering&sev[]=critical&sev[]=high`). A Move menu
  on each card (New, a column, Monitoring, Closed). On phones one column at a time, with a column
  switcher; side by side on wide screens. The Boards tab of the tab bar.
- **Card sheet** (`/workspace/cards/:n`, and the overlay a chip, a panel card or a board card
  opens; a modified click on a chip still opens Fizzy): read fresh from Fizzy. Title, severity
  (a single-choice select), column (select), owners (read-only), department tags (toggles),
  other tags, the description as text ("Report", collapsed), steps (tickable), the comment thread
  (Fizzy comments, oldest first, newest last; the first ~195, then "older comments are in
  Fizzy"… Fizzy pages oldest first), a comment box, "Open in Fizzy". Each control saves at once;
  the sheet, the card's chips and the room panel are replaced from the reply.
- **Create a card from this message**: an entry in every message's action menu (added by the
  script to `.message__actions-grid`), and "New card" in the panel and on the board. The form is
  prefilled: title = the message's first line (120 characters at most), details = its text,
  department = the room's (first) linked department, severity to pick. It creates the card on the
  incident board, adds the severity and department tags, comments "Karim: Created from Maya's
  message in #room: <link to the message>", then posts "New card: <card URL>" in the room as the
  person (it renders as a chip). That room message goes through `create_message` and
  `broadcast_create` but not the bot webhooks: a link to a card isn't something to ask Hermes.
- **Cache**: every write reads the card again and puts it in the snapshot at once
  (`Workspace::remember`, which also moves it between the open and closed lists), so chips, the
  panel, Home and the board show it before the next poll. (A poll that started before the write
  can still put the older state back for one interval.)

### Rules every write follows

In `campfire_workspace::actions` (what) and `::writes` (how):

1. **Policy first** (`Workspace::authorize`, the only place): refused → `403 forbidden`, before
   anything is read or written.
2. **Input validated** (`Change::parse`, `NewCard::parse`): `422` with a code (`invalid_target`,
   `invalid_severity`, `unknown_department`, `unknown_column`, `unknown_step`, `blank_title`,
   `title_too_long` (255), `description_too_long` (10 000), `blank_comment`, `comment_too_long`
   (5 000)…). Department tags must be configured departments'.
3. **Incident board only**: the card is read fresh first; a card on another board (or none) is
   `404`, and nothing is written.
4. **Toggles handled**: Fizzy's taggings are toggles (posting a tag the card has removes it;
   titles are lowercased). Severity and departments are computed against the fresh card: only the
   differences are posted (old severity off, new one on; a card with two `sev-*` tags ends with
   one), then the card is read again and checked. Nothing is ever retried blindly. Moves use
   `POST /triage` (`column_id`), `DELETE /triage` (New), `POST /not_now` (a closed card is
   reopened first: Fizzy postpones open cards), `POST /closure`; steps `PUT /steps/:id`
   (`{"step": {"completed"}}`); comments `POST /comments`; cards `POST /boards/:id/cards` (tags
   can't be set there, so they follow). A move that didn't take is an error. **One write
   sequence per card at a time**: `Workspace::lock_card` (a `tokio` mutex per card number, dropped
   from the map once nobody holds or waits for it) is held around a change (and its read-back
   after an error) and around a new card's tags and comment, so two people changing the severity
   of one card at once can't interleave their toggles (both would remove the old tag, the second
   putting it back). It serializes this server's writes only: Hermes or someone in Fizzy can still
   change the card meanwhile, which the final read and check catch.
5. **Fizzy down or refusing** → `502 fizzy_unavailable` with a clear message; after a failed write
   the cache gets the card's actual state if Fizzy still answers (and the sheet reloads to show
   it). No partial state is silent: a card that was created but whose tags or comment failed is
   still `201`, with `"warning": "Card #14 was created, but its tags couldn't be set (…). Open it
   to finish."`, which the form shows.
6. **Logged**: every write, success or not, is a `WriteRecord` (Campfire user id, card, action
   such as `tag +sev-high` or `move column:<id>`, whose token, outcome) that the adapter logs
   ("workspace wrote to Fizzy"). Never the token.

### Acting identity

`writes::TokenSource` gives the token each person's writes use (`ActingIdentity { token,
on_behalf, label }`). Today it's `SharedToken` (Hermes's `FIZZY_TOKEN`) for everyone, so every
comment written for someone starts with their Campfire name ("Karim: …") and Fizzy shows Hermes
as the author. Per-person Fizzy tokens later = another `TokenSource`
(`Workspace::with_tokens`), with `on_behalf: false`; nothing else changes.

### Policy

`settings::Policy`, read from the settings, applied only by `Workspace::authorize`:

| | `anyone` (now) | `author_or_duty_manager` | `duty_managers_only` |
|---|---|---|---|
| Confirm a Hermes draft (File / Dismiss) | everyone | the reporter or a duty manager | duty managers |
| Create a card | everyone | everyone | duty managers |
| Comment | everyone | everyone | duty managers |
| Change a card (move, close, not now, severity, departments, steps) | everyone | duty managers | duty managers |

Duty managers: those listed in the settings, else Campfire's administrators. A draft's reporter
is the last person (not a bot) who wrote in the room before the draft. The page disables what the
viewer can't do; the draft buttons can't (they're in the shared message cache), so a refused File
says why.

### Settings

`<CAMPFIRE_STORAGE_PATH>/hermes/workspace.json` (so `storage/hermes/workspace.json` by default,
next to `db/` and `files/`), not Campfire's database (upstream's schema stays untouched). Written
atomically (temporary file in the same directory, `fsync`, rename, `fsync` of the directory), one
save at a time (a mutex around write-then-use, so the file and memory end as the same save).
Missing = the defaults (`anyone`). A file that exists but can't be read or doesn't validate **fails
closed** (`Settings::fail_closed`): no departments, `duty_managers_only`, the administrators as
duty managers, so a damaged file can't loosen a policy the owner tightened; a warning in the log and
on the settings page, and the next save replaces it. A save the server can't write is `500
settings_not_saved` (nothing changed), not `422`.

```json
{
  "version": 1,
  "departments": [{"name": "Engineering", "tag": "engineering", "rooms": [3, 7]}],
  "duty_managers": [1, 5],
  "confirm_policy": "anyone"
}
```

- `departments`: at most 50; a name (unique, 60 characters), a tag (a Fizzy tag title: `#` and
  case dropped as Fizzy does, letters/digits/`-_.`, 40 characters, unique, not `sev-*`; empty =
  from the name), linked rooms (optional; a room may be linked to several departments).
- `duty_managers`: Campfire user ids; absent or `null` = the administrators.
- `confirm_policy`: `anyone`, `author_or_duty_manager` or `duty_managers_only`.

Edited at `/workspace/settings` (Campfire administrators only; linked from Home and the board):
add / rename / remove departments, their rooms (checkboxes), duty managers (administrators, or a
list), the policy. Saving drops room and user ids that aren't rooms or active people. There's no
Fizzy API to create, rename or delete tags: a tag exists once a card has it, and renaming a
department's tag here leaves the old tag on existing cards.

## Routes and contract

All answer **404 while the workspace is off**, run `ApplicationController`'s chain (session cookie
only, bots denied — except the bot route —, `Sec-Fetch-Site` forgery protection on the POSTs) and
live in the fork's route table (`controllers::HERMES_ROUTES`, tried after the Rails one). JSON
errors are `{"error": code, "message"}`; the script shows the message. An expired session
redirects to the sign-in page (fetch follows it): the script says to sign in again.

| Route | Answer |
|---|---|
| `GET /workspace` | The Home page (HTML) |
| `GET /workspace/cards.json?numbers=12,13` | `200 {"cards": {"12": "<a class=\"ws-chip\" …>…</a>"}}`: the chips of the incident-board cards the workspace knows (at most 50 asked); unknown numbers are omitted and fetched at the next poll, cards on other boards are always omitted |
| `POST /workspace/drafts/:message_id/reply` `{"decision": "confirm" \| "dismiss"}` | `201 {"message_id", "reply": "confirm" \| "cancel"}`; `404` when the message isn't in one of the user's rooms; `422 invalid_decision`; `422 not_a_draft` (not from an active bot, or not a draft); `403 forbidden` (policy) |
| `GET /workspace/board?dept=&sev[]=` | The board (HTML) |
| `GET /workspace/cards/:number[?fragment=1]` | The card's sheet (page, or fragment for the overlay); `404` not on the incident board; `502` + a notice when Fizzy doesn't answer |
| `GET /workspace/cards/new?message_id=…\|room_id=…[&fragment=1]` | The new-card form; `404` for a message or room that isn't the user's |
| `POST /workspace/cards` `{"title", "description", "severity", "department" \| "departments", "message_id" \| "room_id"}` | `201 {"number", "url", "chip", "message_id", "warning"}` (`message_id` = the room message with the link); `403`, `404`, `422`, `502` |
| `POST /workspace/cards/:number/:change` — `move {"to": "new"\|"column:<id>"\|"not_now"\|"closed"}`, `severity {"severity": "high"\|""}`, `departments {"tags": […]}`, `step {"step_id", "completed"}`, `comment {"body"}` | `200 {"number", "sheet", "chip"}` (the fresh sheet and chip); unknown change `404`; `403`, `404`, `422`, `502` |
| `GET /workspace/rooms/:room_id/panel` | The panel (fragment); `204` room not linked; `404` not the user's room |
| `GET /workspace/settings` | The settings page; `403` unless administrator |
| `POST /workspace/settings` (JSON above) | `200 {"ok": true}`; `422 invalid_settings` with the reason; `500 settings_not_saved` when the file can't be written (nothing changed); `403` unless administrator |
| `GET /hermes/:bot_key/workspace/settings.json` | Bots only (`403` for people): `{"incident_board", "severity_tags", "departments": [{"name", "tag", "rooms": [{"id", "name"}]}], "duty_managers": [{"id", "name"}], "confirm_policy"}`, for Hermes's skill to tag cards by department. No bot write endpoint |

## Crate layout

| Path | What |
|---|---|
| `crates/workspace` (`campfire_workspace`) | The feature. Depends on no `campfire_*` crate (askama, base64, jiff, regex, serde, serde_json, and tokio for its `sync::Mutex` only), so `cargo test -p campfire_workspace --offline` runs anywhere, without libvips or the parity seed |
| `…/src/config.rs` | `WorkspaceConfig::from_lookup`, `Secret` |
| `…/src/fizzy.rs` | Fizzy types (lenient decoding), `HttpClient` trait (the app implements it), `Client` (paths, pagination), `@mention` reading from rich text |
| `…/src/cache.rs` | `Snapshot` and the poll |
| `…/src/chips.rs` | Card URL matching, chip HTML, message decoration |
| `…/src/drafts.rs` | Draft detection, pending drafts, `Decision`, the buttons |
| `…/src/home.rs`, `templates/workspace/home.html`, `_card.html` | Home view-model and template |
| `…/src/overlay.rs`, `templates/workspace/_tab_bar.html`, `_nav.html` | Tab bar (and the room panel it carries), pages' nav, page location |
| `…/src/settings.rs` | `Settings`, `Department`, `Policy`/`Act` (the policy point), `SettingsStore` (the JSON file, atomic serialized writes, fail-closed reads) |
| `…/src/writes.rs` | `TokenSource`/`SharedToken`/`ActingIdentity`, `Writer` (every Fizzy write, audited), `ActionError`, `WriteRecord`, toggle diffing |
| `…/src/actions.rs` | `Change`, `Target`, `NewCard` (input parsing and validation); `Workspace::{authorize, card_sheet, change_card, create_card}` |
| `…/src/pages.rs`, `templates/workspace/{board,sheet,_panel,_list_card,new_card,settings}.html` | Board, card sheet, room panel, new-card form, settings page, the bot's settings JSON |
| `…/src/lib.rs` | `Workspace` (state shared by poll, hooks and routes: snapshot, settings, token source, audit sink), `ChatSource` trait (the app implements it) |
| `crates/campfire/src/controllers/workspace.rs` | **The adapter**: boot (`build`, `start`: bots, hooks, poll task, the write log), the actions, `ChatSource` over campfire_db (one SQL query: the user's rooms' messages of the last day), a draft's reporter (one SQL query), rooms and people for the settings, `HttpClient` over `integrations::net` (GET, POST, PUT, DELETE) |
| `crates/views/src/hermes.rs` (end) | `WorkspaceHooks`, `install_workspace_hooks`, the two seam functions (fork-owned file) |
| `crates/assets/overrides/hermes/workspace.css`, `workspace.js`, `home.svg`, `board.svg` | Frontend. Under `hermes/`, which `build.rs` leaves out of `stylesheet_link_tag :all`; the script isn't pinned in the import map (only `pin_all_from` directories are), so upstream pages keep their exact asset tags |

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
| `crates/campfire/src/controllers.rs:329-342` | Twelve rows in `HERMES_ROUTES` (three for phase 0, nine for phase 1) | The fork's own route table (the Rails table stays identical to `bin/rails routes`) |
| `crates/views/src/messages/presentation.rs:19-20` | `MessageContent::Text { html } => crate::hermes::workspace_message_html(message, html)` | The one hook in message rendering: chips and draft buttons, at render time |
| `crates/views/templates/layouts/application.html:55` | `{{ crate::hermes::workspace_overlay(ctx)\|safe }}` after the lightbox include, same line | The one include in the layout: stylesheet, script, tab bar. Renders `""` while off, and being on the same line adds no whitespace |

Phase 1 added no seam: only rows in the existing `HERMES_ROUTES` block. Everything else is in
fork-owned files.

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
5. If upstream changed any of what `controllers/workspace.rs` borrows, fix that file (the compiler
   points at it):
   - views: `message_presentation`, `MessageView` (`id`, `creator.id`), `ViewContext`
     (`current_user`, `request_url`, `base_url`, `last_room_visited_id`, `asset_path`/`asset`), the
     `layouts::Application` fields (`ctx`, `page_title`, `body_class`, `head`, `nav`, `content`,
     `footer`, `sidebar`) and `helpers::{empty, raw}`;
   - controllers: `MessagesController#create`'s helpers (`create_message`, `broadcast_create`,
     `deliver_webhooks_to_bots`, `MessageParams`), `before_actions`/`Before` (and
     `allow_bot_access`), `require_current_user`, `concerns::{ensure_can_administer, head}`,
     `presenters::Presenter::new` and `Presenter::room_view` (its `display_name`),
     `presenters::accounts::attachable_sgid`, `presenters::page::db_error`,
     `presenters::view_context::{find_template, page_in_any_format}`, `Ctx::{render_html, head,
     url_for}`, `ParamMap::to_json` / `Param::Number`, `BOT_DEFAULTS`;
   - db: `Message::find_reachable`, `Message::find`/`room`/`creator`/`body_html`/`created_at`,
     `Room::find`/`find_for_user`/`all`/`direct`/`name`, `User::active_bots_ordered`,
     `User::active_ordered_without_bots`, `User::is_administrator`/`is_bot`, `User::email_address`,
     and the `messages`/`memberships`/`users.role` columns used by `recent_messages` and
     `reporter_of`;
   - HTTP: `campfire_richtext::uri::parse` (`host`, `port`, `scheme`, `is_http`),
     `integrations::net::Network::system`, `integrations::net::http::{Request::net_http(…).transport(…),
     Request::body, request_uri, exchange, Endpoint, Timeouts, Body}` and the response's
     `status`/`header`/`read_body`.
6. If upstream changed the room page's DOM (`.message[data-user-id][data-message-id]` siblings,
   `.message__actions-grid` in the message menu, `form#composer`, `[data-composer-target=text]`,
   the composer controller's `replaceMessageContent`, `#nav .room--current`,
   `meta[name=current-room-id]`) or the layout grid (`body`'s `grid-template-areas`/`-columns`
   with `--sidebar-width`, `#main-content`, `#sidebar` z-index, the `100ch` breakpoint): check
   `hermes/workspace.js` and `workspace.css` in a browser (phone and desktop, light and dark): the
   room panel, the message menu's card entry, the overlay, the board's column switcher.
7. `cargo clippy --workspace --all-targets` and the parity gate as usual; with the workspace **off**
   the parity screenshots must not move.
8. If Fizzy changed (a new image): re-check the write endpoints against
   `docs/api/fizzy-rest-api.md` in the Hermes repo (taggings still toggles, `triage`/`not_now`/
   `closure`, `PUT /steps/:id`, `POST /boards/:id/cards` still ignoring tags), then create a test
   card from Campfire and change its severity twice: exactly one `sev-*` tag must remain.

## Tests

- `crates/workspace` (`cargo test -p campfire_workspace`): config (off without both variables,
  defaults, errors never quote the token); Fizzy decoding (lenient cards, column colour objects,
  severity tags, `Link` pagination, @mention sgids incl. nested and avatar fallback); card URL
  matching (known bases, account, terminators, look-alike hosts); chips (escaping, unknown cards
  marked, idempotent); draft detection (the skill's English draft as the bridge posts it, French,
  headed and field-only drafts; filed messages, invitations without a card or without a filing
  purpose ignored), decisions (single words, bot mentions; "ok thanks"/"no worries" aren't) and
  their reply HTML, pending drafts (answered, superseded, expired, chatter); Home (grouping and
  ordering, mentions by email, handover window, drafts need a known bot, rendering and escaping,
  Fizzy down/waiting); the tab bar; and a full poll against a fake Fizzy (both pages, activity
  cards, other boards' cards and mentions dropped, learned origins, mention emails, token only in
  the `Authorization` header, cards asked by chips fetched next poll, other boards' cards never
  chips nor re-asked at once, one failed card or user lookup doesn't fail the poll and is retried,
  Fizzy down keeps the picture, missing board looked up again).
- `crates/views/tests/workspace_hooks.rs`: hooks off = upstream bytes; on = overlay once in place
  and message hook applied, nothing else changed; removed = upstream bytes again.
- `crates/assets/tests/reference.rs`: workspace assets served, not in `stylesheet_link_tag :all`,
  not in the import map.
- Phase 1, in `crates/workspace` against a **stateful** fake Fizzy (writes change its cards the
  way Fizzy does: taggings toggle, triage reopens…): severity as a single choice over toggles
  (old off, new on; nothing posted when already right; two severities end as one; two concurrent
  changes of one card serialized, ending with one severity, no lock left behind), departments
  (exactly the requested tags, other tags untouched, unknown ones refused), moves (column,
  unknown column refused before any write, close, not now on a closed card reopens first, New),
  steps (and no write when already in that state), comments prefixed and escaped, only
  incident-board cards (404, nothing written), the policy refusing before any request (each
  policy, draft reporters, administrators as default duty managers), the audit records (never the
  token), a read-only token (401, stopped at the first write, no retry), Fizzy down, a
  half-applied tag change reported and cached as Fizzy has it, no writes before the board is
  known, card creation (tags, the comment linking back, the cache and the room panel updated at
  once), creation with failing tags (created, with a warning), input validation, the sheet
  (fields, escaping, controls disabled by the policy), the board (columns, filters, Move menus,
  settings link), room panels, the prefilled form, the settings page and the bot JSON; settings
  (validation, normalization, atomic save and reload, a broken, invalid or unreadable file failing
  closed while a missing one is the defaults, the settings page's warning, a failed write being an
  I/O error that changes nothing, concurrent saves ending with file and memory agreeing), the
  policy table.
- `crates/campfire/src/controllers/workspace.rs` (needs the app to link, i.e. libvips; runs in CI):
  route recognition (all routes), `FizzyHttp` against a fake server (headers, query, `Link`, 404,
  errors without the token, a POST's method and body), settings ids; request tests against the
  parity seed (skip without it): phase 1 routes 404 while off, errors before Fizzy answers (502,
  422, 404, the form, the panel's 204), settings saved by an administrator and read by a bot.

Not covered by automated tests: the actions end to end against a real Fizzy (writes are only
tested against the fake, and request tests only reach Fizzy-down paths), `hermes/workspace.js` (no
JS test harness in the repo), and the look of the CSS. Check in a browser after deploying: a chip
in a room (a click opens the sheet), File/Dismiss/Edit on a real Hermes draft (the Edit prefill
relies on Lexxy accepting an `<action-text-attachment … content="…">` in `replaceMessageContent`),
the tab bar on a phone (composer not covered, hidden while typing), Home; for phase 1: the
settings page (add a department linked to a room), the room's "N cards" button and panel (wide:
a column between the conversation and the sidebar; phone: a sheet), the message menu's card
entry and the form, the board (phone: one column and the switcher; wide: columns side by side),
Move, the sheet (severity twice, a department, a step, a comment, "Open in Fizzy"), light and
dark.

## Deliberately deferred or different from the mockup

Phase 1:

- **One board**: no Boards list, no "linked to: Engineering board" (the owner chose one incident
  board with department tags).
- **Owners are shown, not changed**: no "+ owner" / "Assign to me". With one shared token,
  "assign to me" would assign Hermes (Fizzy's `self_assignment`), and assigning someone needs
  their Fizzy user (no Campfire ↔ Fizzy user link yet). Comes with per-person tokens.
- **No drag and drop** on the board: Move is a menu, on desktop too.
- **No auto-postpone countdown** (the owner hasn't decided the board settings; they aren't read).
- **The panel lists open department cards only**, not also "cards mentioned here recently" nor
  Hermes drafts (those are on Home).
- **Message menu**: one entry ("Create a card", an icon in the menu's grid), not also "Add to an
  existing card…" / "Ask Hermes to draft the report".
- **Step ticks don't post to the thread** (the mockup's "Karim ticked …" line): Fizzy records the
  change itself, and a comment per tick would be noise.
- **Comments aren't dictated** (no Whisper in the sheet), and comments are shown as text, not
  Fizzy's rich HTML (no attachments or mentions rendered).
- **The draft buttons stay visible** to everyone even when the policy wouldn't let them confirm
  (they live in the shared message cache); the server refuses and says why.

Phase 0:

- **No badge counts** on the tab bar (they'd need a per-user query on every page); Home shows the
  counts. **Three tabs**, not five: Boards and Hermes are phases 1 and 2.
- **Report from Home** goes to the last room visited instead of asking "Where?".
- **Open incidents** = every open card of the incident board, not "cards I own + unowned" (per the
  task); owners are shown on each card.
- **Mentions** come from Fizzy comments only; Campfire @mentions already notify through Campfire.
- **In-memory cache**, not SQLite: the first poll after a restart rebuilds it (activity pages 1–3,
  so mentions older than that aren't recovered).
- **Chips on every message**, not only Hermes's; unknown card numbers are fetched lazily.
- **Anyone in the room** can answer a draft; since phase 1 that's the settings' policy (`anyone`
  by default; `author_or_duty_manager` is "only the author or the Duty Manager").
