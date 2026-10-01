# Hermes fork: the Duty Manager Workspace (phases 0, 1 and 2)

The "Duty Manager Workspace" design (the approved mockup, §5 "Feasibility and plan"). **Phase 0**
made what already exists visible inside Campfire, read-only. **Phase 1** ("Work incidents without
leaving the app") works the incident cards from Campfire: a room's cards panel, the board, the
card sheet, and "Create a card from this message". **Phase 2** supervises Hermes; its slices 2.0
(groundwork), 2.2 (the Hermes tab), 2.3 (proposals and the autonomy dial) and 2.4 (undo) are
[below](#phase-2-supervising-hermes), and its second batch, 2.5 ([alerts](#25-alerts)), 2.6 (the
[handover](#26-end-of-shift-handover)) and 2.7 ([visibility by department room](#27-visibility-by-department-room)),
after them.

**Names people see** (owner decision, 1 Oct 2026; docs/hermes-theme.md, "Branding"): the product is
**MeshDuty** and the assistant (Hermes) is **Sky**. This document keeps the internal names: the
"Hermes tab" is labelled **Sky** in the tab bar, its lines read "via MeshDuty" (`Via::Campfire`),
new drafts start "Sky proposes:", the undo comment reads "Undone from MeshDuty: Sky created this
card…", and so on. Routes (`/workspace/hermes`), classes, `data-ws-*`, the `hermes-proposal:`
marker, environment variables and log lines are unchanged. The bot's display name is data (the
account's bot user); messages already posted keep the wording they were posted with.

## Phase 0: seeing incidents

- **Card chips**: a Fizzy card URL (`…/<account>/cards/<n>`) of an incident-board card in a message
  renders as a small Fizzy-style card (number, title, severity, column), with the card's *current*
  state. A link to a card on any other board stays a plain link. The chip links to the card's
  sheet in the app (`/workspace/cards/<n>`), not to Fizzy.
- **Draft buttons**: a Hermes message that shows an incident draft and asks for a confirmation gets
  **File / Edit / Dismiss**. File and Dismiss post `confirm` / `cancel` in the room as the user;
  Edit puts "@Hermes change: " in the composer.
- **Tab bar**: on phones a bottom bar **Home · Chats · Report · Boards · Hermes** (Report = the live
  voice report of the room on screen; Boards since phase 1, Hermes since phase 2); a slim rail on
  wide screens.
- **Home** (`/workspace`): *To confirm* (Hermes drafts nobody answered), *Open tickets* (the
  incident board's cards that aren't closed — any ticket: requests, faults, complaints, incidents — by column, most severe first), *Mentions* (Fizzy comments
  on incident-board cards that @mention you), *Handover* (incident cards changed in the last 12 h).
  Every card, mention and Hermes-log line opens the card's sheet in the app (see "Tickets stay in
  Campfire" below).

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
| `CAMPFIRE_PUBLIC_URL` | unset | Campfire as *browsers* reach it, e.g. `https://192.168.0.114:8443`, for the link from a card created from a message back to that message. Unset: the comment gives the message's path as text (see below). When its host isn't `FIZZY_PUBLIC_URL`'s (Campfire published on a public hostname, e.g. through the `public` profile's Cloudflare Tunnel), requests on that host are **public**: no Fizzy links on the pages, even for duty managers (see "Tickets stay in Campfire") |
| `HERMES_BOT` | unset | Phase 2: Hermes's Campfire bot, by user id or exact name: the **only** bot whose proposals Campfire takes (the two `/hermes/:bot_key/workspace/proposals` routes; any other bot gets `403 {"status": "refused", "error": "not_hermes_bot"}`, logged). Unset: `GEMINI_LIVE_VOICE_BOT` (the bot live voice reports go to) when set, else the instance's only active bot; with several active bots and neither set, every proposal is refused until it's set. Compose passes `HERMES_BOT` from `deploy/.env` |
| `HERMES_FIZZY_TOKEN` | unset | Phase 2: Hermes's **own** Fizzy token (`write`; compose passes `FIZZY_AGENT_TOKEN`). What Campfire runs for Hermes (its proposals, the undo of its comments) is written with it, so Fizzy shows Hermes as the author, and the Hermes log learns Hermes's Fizzy user from it (`GET /my/identity`). Unset: those writes use `FIZZY_TOKEN`, their comments start with "Hermes: ", Hermes's comments can't be deleted by undo, and Hermes's Fizzy user must be set in the settings for its direct actions to be logged. Never logged or shown |

A malformed `FIZZY_URL`, `FIZZY_PUBLIC_URL`, `CAMPFIRE_PUBLIC_URL`, `FIZZY_ACCOUNT` or
`FIZZY_POLL_S` fails the boot (the message never quotes the token). Only one of
`FIZZY_URL`/`FIZZY_TOKEN` set = off. Phase 1's only variable is the optional `CAMPFIRE_PUBLIC_URL`
(read by the workspace crate's own `WorkspaceConfig::from_lookup`, so no new seam); its settings
are edited in the app and kept in `<CAMPFIRE_STORAGE_PATH>/hermes/workspace.json`
([Settings](#settings)).

Whose token: the token decides what the workspace can see (Fizzy scopes everything to the token's
user: `Current.user.boards`) and who Fizzy shows as the author of every change people make here.
Since phase 2 (decision D1) that's the Fizzy user **"Campfire"** created by `deploy/fizzy-agent.sh`
(`WORKSPACE_FIZZY_TOKEN`, which compose prefers over Hermes's `FIZZY_AGENT_TOKEN`), so Fizzy tells
people's changes from the AI's; until that user exists the workspace can still run on Hermes's token,
and the Hermes log then tells them apart with the write log ([below](#hermes-log)). **Everything the
token sees on the incident board is shown to every signed-in user** (chips, Home), unless an
administrator restricts departments ([2.7](#27-visibility-by-department-room)). Nothing from its
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
  `BASE_URL`, possibly unreachable, so only its presence is used; the card sheet's comments also
  read `X-Total-Count`). The HTTP client is the app's
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
  `Severity:`/`Gravité :`. Text matching, as planned for phase 0; since phase 2 the drafts of Hermes's
  proposals carry a structured marker instead ([below](#proposals-and-the-autonomy-dial)), and
  text matching stays for the others. In the room the buttons are appended after the body; the
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
  users; it loads the `hermes/workspace.js` module itself, while `hermes/workspace.css` is linked
  for the same users from the layout's head (`WorkspaceHooks::stylesheets`, through the head seam
  of docs/hermes-theme.md, before the theme). Report is
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
  on phones; on wide screens the room's fixed top bar is shortened to end where the panel starts,
  so it doesn't cover the panel's head and close button) listing the incident board's open cards tagged with those departments, most severe
  first, with "New card" and "Open on the board". A room not linked shows nothing. Open or closed
  is remembered per browser (`localStorage`). The panel comes with the layout's overlay (from the
  cache, no Fizzy request) and is refreshed after each change (`GET /workspace/rooms/:id/panel`).
- **Board** (`/workspace/board`): "New" (Fizzy's "Maybe?"), the board's columns, "Monitoring"
  ("Not Now") and "Closed" (the 30 most recently closed), each most severe first. Filters: one
  department tag and any severities (`?dept=engineering&sev[]=critical&sev[]=high`). A Move menu
  on each card (New, a column, Monitoring, Closed). On phones one column at a time, with a column
  switcher; side by side on wide screens. The Boards tab of the tab bar.
- **Card sheet** (`/workspace/cards/:n`, and the overlay a chip, a panel card, a board card, a
  Home tile, a mention or a Hermes-log line opens; a modified click opens the sheet's page):
  read fresh from Fizzy. Title, severity
  (a single-choice select), column (select), owners (read-only), department tags (toggles),
  other tags, the description as text ("Report", collapsed), steps (tickable), the comment thread
  (the **newest 100** Fizzy comments, oldest first, newest last, under **"Show earlier comments"**
  when there are more), a comment box, and for duty managers on the LAN only a "⋯" menu with
  "Open in Fizzy (browser) ↗". "Show earlier comments" is a link to `?comments=all`, which the
  script loads into the sheet in place (keeping what was typed): the same read, visibility check
  included, keeping up to 1,000 comments (`actions::SHEET_ALL_COMMENTS`; beyond that the sheet says
  "Only the last 1000 comments are shown"). Such a read walks up to a dozen Fizzy pages and the app
  is on the internet, so at most 3 run at once (`ALL_COMMENTS_AT_ONCE`, a semaphore in `Workspace`):
  one more is refused before any Fizzy request, `429 busy` with "try again in a moment", shown as
  the sheet's status. An expanded sheet stays expanded after a change (it carries
  `data-ws-comments="all"`; the script posts to `…/<change>?comments=all`, and the reply falls back
  to the newest 100 while those reads are busy). Fizzy lists comments
  oldest first in geared pages (15, 30, 50, then 100) with only a `rel="next"` link, so the sheet
  reads page 1, takes the total from its `X-Total-Count` header (set by `geared_pagination` on
  JSON lists), and jumps to the page holding the 100th newest comment and the ones after it: three
  requests at most. Without that header it walks the pages (20 at most), keeping the last 100; when
  pages are left after the cap, the comments shown aren't the newest: the sheet says "the newest
  comments may be missing" and the adapter logs a warning (`LatestComments::truncated`).
  Each control saves at once;
  the sheet, the card's chips and the room panel are replaced from the reply. A change that fails
  shows its error and reloads the sheet (the card as it really is), keeping what was typed in the
  comment box.
- **Create a card from this message**: an entry in every message's action menu (added by the
  script to `.message__actions-grid`, only when the policy lets the viewer create cards: the
  layout's overlay says so in `<template data-ws-viewer data-ws-can-create>`), and "New card" in
  the panel and on the board (same condition, rendered by the server). The form is
  prefilled: title = the message's first line (120 characters at most), details = its text,
  department = the room's (first) linked department, severity to pick. It creates the card on the
  incident board, adds the severity and department tags, comments "Karim: Created from Maya's
  message in #room: <link to the message>" (the link is `CAMPFIRE_PUBLIC_URL` + the message's
  path; without that variable the comment says "… in #room (in Campfire at /rooms/3/@55)." as
  text. It is never built from the request's `Host` header, which the client controls and which
  would put a link of their choosing in front of everyone reading the card in Fizzy; the app has
  no other setting for its own public URL, and `TLS_DOMAIN` doesn't say the port browsers use),
  then posts "New card: <card URL>" in the room as the
  person (it renders as a chip). That room message goes through `create_message` and
  `broadcast_create` but not the bot webhooks: a link to a card isn't something to ask Hermes.
- **Cache**: every write reads the card again and puts it in the snapshot at once
  (`Workspace::remember`, which also moves it between the open and closed lists), so chips, the
  panel, Home and the board show it before the next poll. (A poll that started before the write
  can still put the older state back for one interval.)

### Rules every write follows

In `campfire_workspace::actions` (what) and `::writes` (how):

The checks run in this order, each answering before the next one runs and before anything is
written to Fizzy:

1. **Request shape and input** (in the adapter, before the policy): an unknown change or a
   non-numeric card number is `404`; bad input is `422` with a code (`Change::parse`,
   `NewCard::parse`: `invalid_target`, `invalid_severity`, `unknown_department`, `blank_title`,
   `title_too_long` (255), `description_too_long` (10 000), `blank_comment`, `comment_too_long`
   (5 000)…; department tags must be configured departments'); a card's source message or room
   that isn't the user's is `404`; for drafts, `422 invalid_decision`, `404` outside the user's
   rooms, `422 not_a_draft`. So someone the policy refuses can still learn that their input was
   malformed or that a message isn't theirs, never anything about a card.
2. **Policy** (`Workspace::authorize`, the only place): refused → `403 forbidden`, before Fizzy
   is asked anything.
3. **Incident board only**: the card is read fresh (`502` if the board isn't known yet or Fizzy
   doesn't answer); a card on another board (or none) is `404`, and nothing is written. Checks
   that need the fresh card come after it: `422 unknown_column` (the board's columns are read
   again) and `422 unknown_step`.
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
6. **Logged**: every write, success or not, is a `WriteRecord` (time, Campfire user id, card,
   action such as `tag +sev-high` or `move column:<id>`, whose token, for what, outcome), appended to
   the durable write log `actions.jsonl` (phase 2) and logged by the adapter ("workspace wrote to
   Fizzy"). Never the token.

### Acting identity

`writes::TokenSource` gives the token each person's writes use (`ActingIdentity { token,
on_behalf, label }`). Today it's `SharedToken` (the workspace's `FIZZY_TOKEN`, label `workspace`) for
everyone, so every comment written for someone starts with their Campfire name ("Karim: …") and
Fizzy shows the token's user (the "Campfire" user) as the author. Per-person Fizzy tokens later =
another `TokenSource` (`Workspace::with_tokens`), with `on_behalf: false`; nothing else changes.
What Campfire runs for Hermes (phase 2) uses `HERMES_FIZZY_TOKEN` (label `hermes`, `on_behalf:
false`) when it's set, else the shared token with Hermes as the actor ("Hermes: …").

### Policy

`settings::Policy`, read from the settings, applied only by `Workspace::authorize`:

| | `anyone` (now) | `author_or_duty_manager` | `duty_managers_only` |
|---|---|---|---|
| Confirm a Hermes draft (File / Dismiss) | everyone | the reporter or a duty manager | duty managers |
| Create a card | everyone | everyone | duty managers |
| Comment | everyone | everyone | duty managers |
| Change a card (move, close, not now, severity, departments, steps) | everyone | duty managers | duty managers |

Duty managers: those listed in the settings, else Campfire's administrators. A draft's reporter
(`reporter_of` in the adapter) is a heuristic: the last person (not a bot) who wrote in the room
before the draft. In a busy room that can be a bystander who spoke in between, who then counts as
the reporter under `author_or_duty_manager`, while the real reporter doesn't; a draft with no
earlier human message has no reporter (duty managers only). A structured marker in Hermes's
draft naming the reporter would fix this (later). The page disables what the viewer can't do;
the draft buttons can't (they're in the shared message cache), so a refused File says why.

**What the policy does not cover.** It gates the workspace's own buttons and routes (File /
Dismiss, including those of Hermes's proposals since phase 2, the sheet's controls, the panel's and
board's actions, card creation). It is not a permission on Fizzy: anyone with a Fizzy account on
the incident board can change cards there. Hermes's proposals go through it (a proposal is
confirmed per this policy); Hermes's **direct** writes with its own Fizzy token don't (decision D7:
Hermes keeps that token), see [phase 2](#phase-2-supervising-hermes). A text draft (an older skill,
or the skill's fallback) is still answered by typing "@Hermes confirm", which the policy doesn't
see either.

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
  "version": 3,
  "departments": [{"name": "Engineering", "tag": "engineering", "rooms": [3, 7], "restricted": false},
                  {"name": "Security", "tag": "security", "rooms": [4], "restricted": true}],
  "duty_managers": [1, 5],
  "confirm_policy": "anyone",
  "autonomy": {"create": "ask_first", "comment": "alone", "tag": "alone", "move": "ask_first",
               "close": "ask_first", "step": "ask_first"},
  "hermes_fizzy_user_id": null,
  "visibility": {"mode": "everyone", "untagged": "everyone"},
  "notifications": {"enabled": true, "severities": ["critical", "high"], "department_rooms": true,
                    "new_reminder_min": 15, "draft_reminder_min": 10},
  "handover": {"room_id": null, "shift_ends": ["07:00", "15:00", "23:00"], "time_zone": "Europe/Paris",
               "reminder": true}
}
```

- `departments`: at most 50; a name (unique, 60 characters), a tag (a Fizzy tag title: `#` and
  case dropped as Fizzy does, letters/digits/`-_.`, 40 characters, unique, not `sev-*`; empty =
  from the name), linked rooms (optional; a room may be linked to several departments).
- `duty_managers`: Campfire user ids; absent or `null` = the administrators.
- `confirm_policy`: `anyone`, `author_or_duty_manager` or `duty_managers_only`.
- `autonomy` (version 2): per kind of action Hermes proposes, `alone`, `ask_first` or `never`
  ([phase 2](#proposals-and-the-autonomy-dial)); a kind left out keeps its default. Deleting and
  reassigning cards aren't kinds: Campfire never does them for Hermes.
- `hermes_fizzy_user_id` (version 2): Hermes's Fizzy user id, for the Hermes log; `null` = learned
  from `HERMES_FIZZY_TOKEN`.
- `restricted` (version 3, per department): with `visibility.mode` = `by_department_room`, only the
  members of its linked rooms, the duty managers and the administrators see its cards
  ([2.7](#27-visibility-by-department-room)). Default `false`.
- `visibility` (version 3): `mode` = `everyone` (default: what phases 0 to 2a did) or
  `by_department_room`; `untagged` = `everyone` (default, decision D3) or `duty_managers`.
- `notifications` (version 3, [2.5](#25-alerts)): `enabled`, the `severities` that alert (default
  critical and high, D9), `department_rooms` (a notice in the card's department rooms),
  `new_reminder_min` and `draft_reminder_min` (0 = no reminder; at most 1 440).
- `handover` (version 3, [2.6](#26-end-of-shift-handover)): `room_id` (none by default: the handover
  can be prepared, not posted), `shift_ends` (`HH:MM`, 1 to 6, sorted on save), `time_zone` (an IANA
  name, or a POSIX TZ rule; see below), `reminder` (a direct message to the duty managers at each
  shift end once a room is set).
- A version 1 or 2 file loads with the defaults of what it lacks (it isn't rewritten), so nothing
  changes on deploy (visibility `everyone`, no handover room); the next save writes version 3. A
  damaged or invalid file fails closed: every dial at `ask_first`, and visibility `by_department_room`
  with `untagged: duty_managers`, which, with no department known any more, shows the cards to the
  duty managers (the administrators) only, rather than letting a restricted department's cards leak.
  One exception: a handover `time_zone` this system can't resolve is read as UTC, with a warning in
  the log and on the settings page until the next save (a save still refuses it), since it only
  moves the shift ends.
- Saving refuses (`422`) a **restricted department linked to an open room** (Campfire's
  `Rooms::Open`, which every user is a member of): everyone would see its cards
  (`Settings::check_open_rooms`, checked by the adapter, which knows the rooms).

Edited at `/workspace/settings` (Campfire administrators only, decision D6; linked from Home, the
board and the Hermes tab): add / rename / remove departments, their rooms (checkboxes) and whether
they're restricted, duty managers (administrators, or a list), the policy, what Hermes may do alone,
Hermes's Fizzy user, who sees which cards, the alerts, the handover (room, shift ends, time zone,
reminder). Saving drops room and user ids that aren't rooms or active people (the handover room
too). Saving a different visibility mode empties the message fragment cache (see 2.7). There's no
Fizzy API to create, rename or delete tags: a tag exists once a card has it, and renaming a
department's tag here leaves the old tag on existing cards.

## Phase 2: supervising Hermes

The phase 2 plan (slices 2.0 to 2.8, decisions D1 to D14) is in the Hermes repo. This fork has
slices **2.0** (fork part), **2.2**, **2.3** and **2.4**, then **2.5**, **2.6** and **2.7**
([second batch](#phase-2-second-batch-alerts-handover-visibility)); 2.1 (departments in the bridge's prefix) is
bridge and skill only. Decisions applied: D1 (a separate "Campfire" Fizzy user for the workspace),
D4 (everyone reads the Hermes tab; a proposal's details only for its room, see below), D5 (default dial), D6 (admins set the dial), D8 (duty managers
and the person it was for undo), and **D7 = no: Hermes keeps its own Fizzy write token**. So the
dial binds what Hermes *proposes* through Campfire, which its incident-report skill does by
default; Hermes can still write to Fizzy directly with its `fizzy` CLI (the skill's fallback when
Campfire can't be reached, and anything a confused or manipulated Hermes decides to do), and its
replies to `@Hermes` mentions in Fizzy comments are posted by the bridge with Hermes's own token:
neither goes through the dial (not even "Comment"). The Hermes
tab shows those direct actions too, marked **direct**, and the settings page says so next to the
dial. Draft confirmation stays `anyone` (the policy's default).

All new data is in files next to `workspace.json`, under `<CAMPFIRE_STORAGE_PATH>/hermes/`:

| File | What | Kept |
|---|---|---|
| `actions.jsonl` | The durable write log: every write the workspace makes to Fizzy, one JSON line (`at`, Campfire `user_id`, `card`, `action`, `identity` = `workspace`/`hermes`, `via` = `workspace`/`proposal`/`undo`, `reference` = proposal or log entry, `outcome`). Never a token | Moves to `actions.jsonl.1` past 5 MiB (so ~10 MiB at most); the last 2 000 are read back at boot |
| `hermes-log.jsonl` | The Hermes log, one entry per line, deduplicated by id | 90 days (compacted at boot and daily) |
| `proposals.json` | Pending proposals and those decided in the last 7 days (written atomically, like the settings) | Pending ones expire after 24 h; past 500, the decided ones decided longest ago are forgotten first, never a pending one |
| `notified.json` | Phase 2.5–2.6: the alerts and reminders sent (by key: `sev:<card>:<severity>`, `new:<card>`, `proposal:<id>:person\|managers`, `handover:<shift end>`), and when the last handover was posted and by whom (written atomically) | Keys 14 days. Unreadable: starts empty (logged); the first poll only records, so nothing old is sent again |

A file that can't be written doesn't stop anything: the entry stays in memory and the server log
says so once ("a workspace file couldn't be read or written").

### 2.0 Groundwork

- **Durable write log** (`campfire_workspace::journal`): `Workspace::record` appends each
  `WriteRecord` to `actions.jsonl`, then gives it to the app's audit sink (the server log, as
  before). The Hermes log reads it back to tell Campfire's writes from Hermes's own, and undo to
  see whether anyone changed a card since.
- **Chip staleness**: every card in the picture has the time it was last read
  (`Snapshot::refreshed`). One nothing refreshed for 15 minutes (a card a chip asked for once, an
  old closed card beyond the lists) is read again, the longest-unread first, within the same budget
  of 10 card reads per poll as the cards chips ask for. A card Fizzy now answers 404 for (deleted,
  or moved to another board) leaves the picture.
- **JS harness**: the page script's logic that doesn't touch the page (chip versions, the numbers
  and proposal ids to ask for, reply handling and the signed-out case, the drafts' status lines, a
  proposal's state, card sheet change bodies, the settings body) moved to
  `hermes/workspace_logic.js`, an ES module the tab bar loads before `hermes/workspace.js`, which
  reads it from `globalThis.HermesWorkspace` (asset URLs are digested, so no relative import). It is
  tested with Node's own runner, no npm package: `node --test crates/assets/tests/js/*.test.mjs`
  (Node 22). DOM behaviour stays untested (phase 3). If `workspace_logic.js` didn't load,
  `workspace.js` does nothing but say so (a console warning, and "Reload the page to use these
  buttons." under drafts): links keep working, nothing throws.
- The "Campfire" Fizzy user and the bridge's trimmed mention DMs (2.0's other parts) are in the
  Hermes repo (`deploy/fizzy-agent.sh`, the bridge). Nothing here depends on the Campfire user
  existing.

### Hermes log

The **Hermes** tab (`GET /workspace/hermes`, the fifth tab; everyone signed in, D4):

- **Pending**: Hermes's proposals waiting for a confirmation (below), with File/Confirm, Edit (opens
  the draft in its room) and Dismiss for those the confirm policy lets decide. **Only the members of
  the proposal's room and the duty managers see a proposal** (its text is a report: description,
  transcript, the person, the room); a proposal without a room (no context from the bridge, a Fizzy
  comment) is for the duty managers only. The others don't see it at all, here or on Home, and
  deciding it answers them `404`. The same goes for the log lines of proposals that never became a
  card (proposed, refused, failed, dismissed, timed out, replaced); what was done shows to everyone,
  as the card itself does.
- **What Hermes did**, newest first, 200 lines at most, with filters: cards created, tags, moves and
  closes, comments, questions asked, failures. Each line says **direct** or **via Campfire**, when,
  for whom and from which source (or "source unknown"), who confirmed, dismissed or undid it, and
  has an **Undo** button when [undo](#undo) applies. Sources:
  - **direct**: Hermes's own Fizzy actions, read at every poll from
    `/activities.json?creator_ids[]=<Hermes's Fizzy user>&board_ids[]=<incident board>` (page 1;
    3 pages at the first poll after a start, and the next page while a whole page is new, 3 at
    most). Hermes's Fizzy user is the settings' `hermes_fizzy_user_id`, else `HERMES_FIZZY_TOKEN`'s
    user (`GET /my/identity`, retried every 10 minutes while unknown). Unknown: the tab says its
    direct actions can't be shown. **Fizzy's feed has no event for tags or steps, and gives only
    the new column of a move**: Hermes's direct tag and step changes aren't in the log, and a direct
    move can't be undone. For a card Hermes created, "for whom" is the report's "Reported by" line
    (marked "from the report", unverified), and the source the report's wording (voice note, live
    voice report).
  - **via Campfire**: its proposals and their outcome (proposed, run, confirmed by, dismissed by,
    dismissed (timed out), replaced, refused, failed), with the person the bridge said it was for
    (checked against Campfire's database) and the source (chat, voice note, live voice report,
    Fizzy comment).
  - From the **viewer's own rooms** only, not stored: Hermes's text drafts, its questions (a last line
    ending with "?") and its error messages, from the last 24 h.
- **Not labelling people's actions as Hermes's**: a direct activity that matches a write in the
  durable log (same card, same kind, within 2 minutes, `outcome` ok) through a token that is
  Hermes's Fizzy user is Campfire's own: a proposal run with `HERMES_FIZZY_TOKEN` (already logged
  "via Campfire"), or, when the workspace's token is Hermes's too (no "Campfire" user yet:
  `FIZZY_TOKEN`'s user, from `GET /my/identity`, is Hermes's), a person's workspace action. Those
  are left out; the tab says so in that setup.
- Entries are kept 90 days (`hermes-log.jsonl`), so the log survives restarts.

### Proposals and the autonomy dial

Hermes asks Campfire instead of writing to Fizzy: the incident-report skill runs its `propose`
helper, which POSTs to the Hermes bridge (`POST /propose/<BRIDGE_PROPOSE_SECRET>`); the bridge adds
the conversation it is relaying (room, person, message, source: its `context`) and forwards the
request with the bot key to `POST /hermes/:bot_key/workspace/proposals`. **Hermes never holds the bot
key.** The bridge is the trust boundary for `context`: it gives each Hermes call an id (`call:<id>`
in the message's prefix), the skill passes it back (`--call`), and the bridge adds the context of
that call only while it runs or for 10 minutes after; without a known call id, no context (the
proposal then has no room and no person: duty managers only). Campfire checks the context against
its database anyway (a room the bot is a member of; an active person **who is a member of that
room**; their message in that room; what doesn't hold is dropped, a name is kept for display only).
**Only Hermes's bot may propose** (`HERMES_BOT`, [Configuration](#configuration)): another bot on
the instance gets `403 not_hermes_bot`, logged.

- **Parsing**: exactly like a person's request (`Change::parse`, `NewCard::parse`): `create`
  (title, description, severity, `department`/`departments`, plus `tags` for the report's type, at
  most 5 simple tags, and `steps`, at most 20), `move` (`to`, or a column by name), `close`,
  `severity`, `departments`, `step`, `comment`, each with `"card": n`. `delete`, `assign` or
  anything else is `422 unknown_action`: Hermes can't ask for what a person couldn't, and never for
  a settings change. A card that isn't on the incident board is `404`.
- **The dial** (`settings.autonomy`, set by administrators on the settings page, D6), per kind:
  create, comment, tag (severity and departments), move, close, step. Defaults (D5): comment and
  tag **Alone**; create, move and close **Ask first**; step **Ask first** (D5 didn't list steps;
  the cautious choice). Delete and reassign: **Never**, fixed.
  - **Alone**: run at once, as Hermes (`HERMES_FIZZY_TOKEN` when set), through the same `Writer`
    (fresh read, toggles handled, one write sequence per card, audited with `via: proposal`),
    recording the card's state before for undo. `201 {"status": "done", "card", "url", …}`.
  - **Ask first**: stored in `proposals.json` and posted in the context's room **as the bot**:
    "Hermes proposes: …", the card's details, and a link to the proposal whose `title` is the
    **structured marker** `hermes-proposal:<id>` (Action Text keeps `title` on links). `202
    {"status": "pending", "id", "draft_message_id", …}`; without a room (a Fizzy comment, no
    context), it waits in the Hermes tab only. The same proposal twice (same request, same source
    message) is one (`"duplicate": true`); `"replaces": "<id>"` (after "change: …") marks the older
    pending one "replaced".
  - **Never**: `403 {"status": "refused", "error": "never"}`, logged; Hermes says so.
- **Confirming**: the message Campfire posted for a proposal (its `draft_message_id`, known before
  the message is broadcast) gets **File**
  (Confirm for a change) / Edit / Dismiss, which POST `{"decision"}` to
  `/workspace/hermes/proposals/:id/decision`. The marker alone does nothing (the proposal must
  exist, be that bot's, and this be its draft's message: the same marker quoted in another message
  gets nothing). The confirm policy applies (`Act::ConfirmDraft` with the person it was
  for as the reporter; the owner kept `anyone` for now). Confirming runs it as above, as Hermes, logged
  "confirmed by Karim", and notes it in the room as the person ("Filed Hermes's proposal: <card
  link>", a chip) when they're a member. A proposal is decided once: a second File says who did it
  ("Confirmed by Karim"). If the dial of its kind has become Never since, confirming is refused. A
  proposal that no longer parses (a department removed since) fails, logged. The buttons live in the
  cached message HTML, so the page asks `GET /workspace/hermes/proposals.json?ids=…` for their state
  and replaces decided ones by it; the text matcher of phase 0 stays for other drafts (older
  messages, the skill's fallback), and a marker message is never a text draft.
- **Expiry**: a pending proposal nobody answered in 24 h becomes "Dismissed (timed out)" (checked at
  every poll and whenever proposals are read), logged.
- **Confirmed live voice reports** count as confirmed: when the context's message is the reporter's
  own live voice report ("Live voice ticket, confirmed by the reporter", `LIVE_REPORT_OPENING`; the
  French opening of images up to v0.1.2-hermes.16 is still recognized; posted in the last 2 h under
  their name), a `create` runs at once unless the dial says Never.
  Once per report: a second card from the same message asks first, even while the first is still
  being filed (checked and recorded in one step under the proposals' lock).
- Home's **To confirm** lists pending proposals too, with their buttons.
- Hermes can ask a proposal's status: `GET /hermes/:bot_key/workspace/proposals/:id` (its own only).

### Undo

`POST /workspace/hermes/actions/:id/undo`, the **Undo** button of a log line, for 24 hours:

| Action | Undo | Recorded |
|---|---|---|
| Hermes created a card (via Campfire, or directly) | Comment "Undone from Campfire: Hermes created this card by mistake. Closed, not deleted." (as the person), then close. Never deleted | — |
| Severity or departments (via Campfire) | The managed tags back to their recorded "before" (only the differences toggled, checked) | before and after tags |
| Move or close (via Campfire) | Back to the recorded place (`new`, `column:<id>`, `not_now`, `closed`) | before and after |
| Step (via Campfire) | The step back | before |
| Comment (via Campfire, or directly) | Deleted, with the token that wrote it: Fizzy lets only a comment's creator delete it, so a direct comment of Hermes needs `HERMES_FIZZY_TOKEN` | the comment's id |
| Hermes closed / reopened a card (direct) | Reopen (Fizzy puts it back in its column) / close | — |
| Hermes renamed a card (direct) | The old title (from the activity's `particulars`) | before and after |
| Hermes moved a card (direct), changed owners, moved it between boards | **Refused**: Fizzy doesn't say where the card was; Campfire never reassigns | — |

- **Who** (D8): duty managers, and the person it was for (a Campfire user Campfire checked), whatever
  the confirm policy (`Act::Undo`). For a direct action nobody is known to be "for": duty managers.
- **Nobody touched it since**, checked on a fresh read, before anything is written, under the
  card's lock; otherwise **refused** with the reason (`422 changed_since`), never forced: the card's
  state must still be what the action left (tags, place, step, title, closed or not); no write to
  that card in the durable log after the action (other than the action's own); and no activity on
  that card in Fizzy's feed after it (the incident board's, newest first, read page after page
  until it reaches the action's time, 10 pages at most: past that, **refused**, "too much activity
  to check"), except Hermes's own follow-ups within 5 minutes of a card it created. If Fizzy's feed
  can't be read, nothing is undone. Also refused: already undone (`already_undone`, by whom), older than 24 h (`too_old`), no
  way back (`cannot_undo`, the reason).
- The undo is itself logged ("Undone: …", by whom) and each of its writes is in the write log
  (`via: undo`); it is written with the person's workspace identity (their name first in the
  comment), except a comment's deletion.

## Phase 2, second batch: alerts, handover, visibility

Slices **2.5**, **2.6** and **2.7** of the plan, with the owner's decisions: D9 as recommended
(critical and high alert every duty manager, a notice in the department's room, reminders), D10 as
recommended (07:00, 15:00, 23:00; post only, no Fizzy card; the room is a setting, none by default),
D2 = yes, **as a setting** (below), D3 as recommended (untagged cards visible to all, listed "No
department" on the duty managers' Home). Nothing here writes to Fizzy or calls Hermes.

### 2.5 Alerts

`campfire_workspace::alerts`. After every successful poll, `alerts::detect` compares the new picture
with the one the previous successful poll produced (kept for the alerts; not the live picture, which
`Workspace::remember` updates as soon as Campfire creates, changes or reads a card, so a card created
or raised through Campfire, by a person or a proposal, or read by the sheet after a change in Fizzy,
still alerts at the next poll) and the proposals:

| Event | When | Who |
|---|---|---|
| **Raised** (`sev:<card>:<severity>`) | An open card filed since the first poll with an alerting severity (`notifications.severities`, critical and high by default), or whose severity rose to one | Each duty manager: a direct message from Hermes's bot, "**Critical**: Guest slip in lobby · #13 · nobody assigned" and the card's link (a chip). With `department_rooms`, a notice in the linked rooms of the card's departments (of its restricted departments only, when it has some and visibility is restricted), where Hermes's bot is a member |
| **Still in New** (`new:<card>`) | Such a card (alerting severity) still in New `new_reminder_min` minutes after it was created (15), while recently due (below) | The duty managers |
| **Proposal waiting** (`proposal:<id>:person`, `…:managers`) | A pending Hermes proposal `draft_reminder_min` minutes old (10): its person; twice that: the duty managers (at once for a proposal without a person); while recently due | The person (if still a member of its room and they may see it), then the duty managers |
| **Handover due** (`handover:<end>`) | Within 30 minutes after a shift end, when `handover.reminder` is on and a handover room is set | The duty managers, with a link to prepare it |

- **Once**: each event's key is claimed in `notified.json` before anything is sent, so nothing is
  sent twice, restarts included (at most once: a message that fails to post is logged, not retried).
  The **first poll after a start alerts on nothing**: whatever would alert then is recorded without
  being sent (a card unknown until now alerts only if Fizzy says it was created after that first
  poll, so an old card a chip asks for later doesn't).
- **Rate**: each person (and room) gets at most one message per poll, whatever the number of events
  ("3 alerts from the incident board", 10 lines at most, then "… and N more"). New serious incidents
  come first (critical, then high…), then the reminders, so the cap never cuts a new critical one.
- **Nothing stale**: a reminder is sent only while **recently due**, for 3 times its delay after it
  fell due (10 minutes at least), so a shorter delay doesn't send every reminder long overdue at
  once; and while alerts are off (`enabled: false`), what they would send is **recorded without
  being sent**, so turning them back on sends nothing that happened meanwhile.
- **Recipients follow the visibility** (2.7): duty managers see every card; a proposal's person is
  reminded only when they may see it; room notices go only where every member may see the card.
- **Delivery** (the adapter, `deliver_alerts`, from the poll task): as Hermes's bot (`HERMES_BOT`,
  else `GEMINI_LIVE_VOICE_BOT`, else the only bot, as for proposals; none: the alerts are dropped
  and the log says so). A person's message goes to their direct room with the bot (found or
  created, like `POST /hermes/:bot_key/directs`), a notice to its room; the message is created like
  a bot's reply to a webhook (`integrations::jobs`: `canonicalize_body`, `Message::create`, whose
  commit queues Campfire's own **Web Push**, then `broadcasts.message_create`). A direct room is
  "everything" involvement, so the phone is notified when Web Push is set up (`VAPID_*` keys,
  HTTPS, and on iPhone the app on the Home Screen). No bot webhook: Hermes isn't asked anything.
- Alerts carry a card's title and number: lock-screen previews can show them.
- `enabled: false` sends nothing (and records what it would have sent, above).

### 2.6 End-of-shift handover

`campfire_workspace::handover` and `::shifts`. Duty managers get **Prepare handover** in Home's
Handover section, which opens `GET /workspace/handover`:

- A **summary built from the last poll**, in an editable box: the header ("Handover: shift ending
  Wed 30 Sep 15:00", then counts), **Still open** by severity (critical first, then "No severity"),
  each line `#13 Guest slip in lobby · New · nobody assigned · <card link>`, **New this shift**,
  **Closed this shift** (by `last_active_at`, Fizzy's cards having no closing time), **Waiting for a
  confirmation** (Hermes's pending proposals: summary, for whom, where). Only board data; no AI
  prose (phase 3), no Fizzy write, no Fizzy card (D10).
- **This shift**: the shift whose end is nearest to now (before or after; the coming one on a
  tie), from the end before it; or from the last handover posted, when that is later (so a second
  handover the same shift covers what came since), or when it handed over the shift just before,
  early (posted at 14:50 for the 15:00 end: the 15:00–23:00 handover starts at 14:50, so what came
  in those ten minutes is in one of them). A late handover of an older shift doesn't count.
- A summary longer than a message can be (10 000 characters) is cut at a line, with a note saying
  the rest is on the board, so it can always be posted as prepared.
- **Posting** (`POST /workspace/handover {"text"}`): as the duty manager, in the handover room,
  through `create_message` and `broadcast_create` (no bot webhook), like a message they type.
  Paragraphs at blank lines, lines kept, `http(s)://` words made links (a card's renders as its
  chip). Duty managers only (`Act::Handover`, whatever the confirm policy); `422 no_room` until an
  administrator sets the room, `422 not_a_member` when they aren't in it, `blank_text`,
  `text_too_long` (10 000 characters). Then `notified.json` records when and by whom.
- **Visibility**: only what every member of the handover room may see is listed; the rest is
  counted in a note above the box ("2 items aren't listed…"), never named, and isn't in the post.
  While the room's members aren't known (the people and rooms not read yet, or nobody known to be
  in it), only what a person in no room may see is listed, and no proposal: an empty member list
  is never taken as "everyone may see it".
- **Time zones**: an IANA name from the system's database when it has one. Campfire's image is a
  slim Debian without `tzdata`, so `shifts::builtin` knows today's rules of common zones (Europe,
  North America, the French overseas departments, UTC…) as POSIX rules, daylight saving included; a
  POSIX TZ string (`CET-1CEST,M3.5.0,M10.5.0/3`) is accepted as is. Anything else doesn't validate
  on save; in a file already saved (a zone this system no longer resolves), it's read as UTC with a
  warning. Default `Europe/Paris`.
- The reminder at each shift end is 2.5's "Handover due".

### 2.7 Visibility by department room

`campfire_workspace::visibility`. A setting, not a new default:

- `visibility.mode = "everyone"` (the default: nothing changes on deploy) — every signed-in user
  sees every incident-board card, as before.
- `"by_department_room"` — the cards of a department marked **restricted** are seen only by the
  members of that department's linked rooms, the duty managers and the administrators; a card with
  several restricted departments, by the members of any of them. Cards of departments that aren't
  restricted stay visible to all (the plan's recommended "restricted departments only": the flag
  makes "all departments restricted" a matter of ticking them all). Cards with no department:
  `untagged = "everyone"` (D3), or `"duty_managers"`. A restricted department without rooms: duty
  managers and administrators only. (Chosen over a separate "all or restricted" switch: one flag per
  department, one mode.)
- **Where** (the check is `Settings::card_visible` with the viewer's `Audience`, one place):
  Home (open incidents, the 12 h list, mentions, "No department"), the board, a room's panel
  (including the one the layout renders), the card sheet (`404`) and every change to a card
  (`404`, checked on a fresh read before any write, nothing written), undo (`404`), chips
  (`cards.json` and the replies' chips), the Hermes tab (log lines about a hidden card, or about a
  proposal for one, aren't shown), proposals (listed, decided and reminded only when the card or the
  departments of the card they'd create are visible: `Workspace::proposal_visible`; a new card's
  departments are those its request resolves to, `tags` naming a department included, stored on the
  proposal when it's made), a proposal's draft (posted in its room only when every member may see
  it — never while the room's members aren't known —, else it waits in the Hermes tab;
  "Filed …" notes about a change name the card only then), alert recipients, the handover. A card
  the picture doesn't have (so its tags aren't known) is hidden while visibility is restricted.
- **Chips**: message HTML is rendered once per message version and shared by every viewer (the
  fragment cache). While visibility is restricted, the message hook only **marks** card links
  (`<a data-ws-card="12" href="…">…</a>`, no card data), and `hermes/workspace.js` fills each one in
  from `GET /workspace/cards.json`, which answers per viewer: a hidden card stays a plain link. The
  reply says `"visibility": "by_department_room"`, and a chip on the page that the reply leaves out
  (rendered earlier) goes back to a plain link. With `everyone`, the hook renders chips as before,
  byte for byte. Saving a different mode clears the fragment cache (`AppState::fragment_cache`), and
  again 5 seconds later (a message rendered with the old mode just before the switch may insert its
  fragment after the first clear), so no cached message keeps chips of the other mode. No new seam: the message hook already is one.
- **Room membership** is Campfire's `memberships` table, read-only: all of it at every poll (for the
  layout's panel and the alerts), and the viewer's own at each workspace request (fresher). Someone
  added to or removed from a room outside a workspace request is seen at the next poll.
- **What it doesn't do**, said on the settings page: it only hides cards **in Campfire**. Anyone
  with a Fizzy login sees the whole board in Fizzy. A room's members are whoever was added to it, so
  turn on Campfire's "Only administrators can create rooms" (otherwise anyone creates a room, and an
  administrator who links it to a department grants its members that department's cards). Hermes
  itself (its skill reads Fizzy with its own token) isn't restricted. Alerts' previews on lock
  screens are the duty managers'. **Nothing already posted is taken back**: an alert's notice in a
  room, a direct message, or a handover posted before a department was restricted (or before a
  card was given a restricted department) keeps the card's title and link; only chips re-check.
  A restricted department can't be linked to an open room (saving refuses it), but a room made
  open afterwards isn't re-checked until the next save.

## Tickets, not only incidents

The board holds **tickets**: any operational request, task, fault, complaint, incident or safety
issue ("refill the water bottles in room 101", "fix the lift in building 7", "guest in room 403
complained about noise"). The Fizzy board keeps its configured name (`WORKSPACE_INCIDENT_BOARD`)
and every internal identifier and route keeps "incident" (`incident_board`, `submit_incident`,
`/rooms/:id/voice/report`…); only user-facing wording says ticket where it reads better: Home's
*Open tickets*, "Hermes drafted a ticket", "No ticket changed in the last 12 hours", the loading
line ("Loading the ticket board…"), the Hermes log's "Drafted a ticket in …", the department-room
alert ("**Critical ticket** (Engineering): …") and a text draft's fallback title ("Ticket draft").
The ticket's type is an extra tag (`request`, `task`, `fault`, `complaint`, `incident`, `safety`,
plus `handover` and `inspection`, and the older `maintenance`, `other`), chosen by the Hermes
`incident-report` skill; tags, severities and department tags are fixed identifiers in any
language. A department can't use a ticket type as its tag (`settings::TICKET_TYPE_TAGS`: request,
task, fault, complaint, incident, safety, handover, inspection), or every ticket of that type would
count as that department's: a save refuses it with « “incident” is a ticket type (…); pick another
tag for “…”, e.g. “incident-team” ». A settings file saved before that rule is **not** failed
closed over it: it's read as saved, with a load warning (log and settings page) naming the tags,
until an administrator renames them — failing every department closed would hide more than the
ambiguity it avoids. Drafts' typed answers (`drafts::is_decision_reply`) also count yes / no /
confirm / cancel words in Spanish, Portuguese, Italian, German, Arabic, Tagalog and Hindi (whole
words; « si », « hindi » and « لا » only on their own). The live voice page (`docs/hermes-gemini-live.md`) takes tickets in any language.

## Tickets stay in Campfire

Most staff have no Fizzy account (Fizzy's sign-in is a dead end for them, its email codes can't be
delivered), and Fizzy isn't reachable from outside once Campfire is published on a public hostname.
So nothing on the workspace's pages sends people to Fizzy:

- **Everyone**: Home's tiles (Open tickets, No department, Handover), its Mentions and the Hermes
  log's card lines open the card's sheet, like the chips, the room panel and the board: an `href`
  to `/workspace/cards/<n>` (the sheet's page, without the script) and `data-ws-open-sheet`
  (the overlay; a modified click keeps the link's own behaviour, the sheet's page in a new tab).
  Older comments load in the sheet ("Show earlier comments").
- **Duty managers and administrators, on the LAN only** (`Workspace::fizzy_links(viewer, lan)`,
  one `fizzy_links: bool` on each of `HomeView`, `BoardView`, `CardSheet` and `HermesPage`, set by
  the adapter): "Open in Fizzy (browser) ↗" in a "⋯" menu in the sheet's and the board's headers,
  "<board> in Fizzy ↗" in Home's Open tickets header, and "Fizzy ↗" after a Hermes-log line about a
  card. All `target="_blank" rel="noopener"`.
- **LAN or public** is the adapter's call (`on_lan`), from `WorkspaceConfig::is_public_request`: a
  request is public when `CAMPFIRE_PUBLIC_URL` is set, its host differs from `FIZZY_PUBLIC_URL`'s,
  and the request's `Host`, one of its `X-Forwarded-Host` values or its URI's authority is that host
  (case-insensitive, ports ignored). On the LAN-only setup both URLs are the LAN host: nothing is
  public. Any match counts, so a forged `X-Forwarded-Host` can't make a tunnelled request (whose
  `Host` Cloudflare sets to the public hostname) look local; a LAN browser that forges one only
  loses the links. This is a convenience, not a control: Fizzy still asks for its own sign-in.
- **Chips** link to the sheet (`/workspace/cards/<n>`), so a long-press "open in new tab", a
  modified click or a page without the script stays in Campfire too; the sheet 404s for a card the
  viewer may not see.
- **Left as they are for now** (stored message content): the alerts' and direct messages' card
  links (`alerts::link_html`), the handover message's card lines, "New card: <link>" posted in the
  room after "Create a card", and the card links in Hermes's own messages all point to Fizzy. They
  render as chips (to the sheet) where the viewer may see the card; where the viewer may not (a
  chip `cards.json` leaves out, phase 2.7), or where the card isn't known yet, the plain Fizzy link
  remains. Rewriting those messages' links is still to do.

## Routes and contract

All answer **404 while the workspace is off**, run `ApplicationController`'s chain (session cookie
only, bots denied — except the bot route —, `Sec-Fetch-Site` forgery protection on the POSTs) and
live in the fork's route table (`controllers::HERMES_ROUTES`, tried after the Rails one). JSON
errors are `{"error": code, "message"}`; the script shows the message. An expired session
redirects to the sign-in page (fetch follows it): the script says to sign in again.

| Route | Answer |
|---|---|
| `GET /workspace` | The Home page (HTML) |
| `GET /workspace/cards.json?numbers=12,13` | `200 {"cards": {"12": "<a class=\"ws-chip\" …>…</a>"}, "visibility": "everyone" \| "by_department_room"}`: the chips of the incident-board cards the workspace knows **that the viewer may see** (at most 50 asked); unknown numbers are omitted and fetched at the next poll, cards on other boards and hidden cards are always omitted |
| `POST /workspace/drafts/:message_id/reply` `{"decision": "confirm" \| "dismiss"}` | `201 {"message_id", "reply": "confirm" \| "cancel"}`; `404` when the message isn't in one of the user's rooms; `422 invalid_decision`; `422 not_a_draft` (not from an active bot, or not a draft); `403 forbidden` (policy) |
| `GET /workspace/board?dept=&sev[]=` | The board (HTML) |
| `GET /workspace/cards/:number[?fragment=1][&comments=all]` | The card's sheet (page, or fragment for the overlay; `comments=all`: up to 1,000 comments instead of 100); `404` not on the incident board or not visible to the viewer; `429` + a notice when 3 `comments=all` reads already run; `502` + a notice when Fizzy doesn't answer |
| `GET /workspace/cards/new?message_id=…\|room_id=…[&fragment=1]` | The new-card form; `404` for a message or room that isn't the user's, then `403` (a notice) when the policy doesn't let them create cards |
| `POST /workspace/cards` `{"title", "description", "severity", "department" \| "departments", "message_id" \| "room_id"}` | `201 {"number", "url", "chip", "message_id", "warning"}` (`message_id` = the room message with the link); `403`, `404`, `422`, `502` |
| `POST /workspace/cards/:number/:change[?comments=all]` — `move {"to": "new"\|"column:<id>"\|"not_now"\|"closed"}`, `severity {"severity": "high"\|""}`, `departments {"tags": […]}`, `step {"step_id", "completed"}`, `comment {"body"}` | `200 {"number", "sheet", "chip"}` (the fresh sheet, with every comment for `comments=all`, and chip); unknown change `404`; `403`, `404`, `422`, `502` |
| `GET /workspace/rooms/:room_id/panel` | The panel (fragment); `204` room not linked; `404` not the user's room |
| `GET /workspace/settings` | The settings page; `403` unless administrator |
| `POST /workspace/settings` (JSON above) | `200 {"ok": true}`; `422 invalid_settings` with the reason; `500 settings_not_saved` when the file can't be written (nothing changed); `403` unless administrator |
| `GET /hermes/:bot_key/workspace/settings.json` | Bots only (`403` for people): `{"incident_board", "severity_tags", "departments": [{"name", "tag", "restricted", "rooms": [{"id", "name"}]}], "duty_managers": [{"id", "name"}], "confirm_policy", "autonomy"}`, for Hermes's skill to tag cards by department. No bot endpoint writes settings |
| `GET /workspace/hermes[?filter=created\|tags\|moves\|comments\|questions\|failures]` | The Hermes tab (HTML) |
| `GET /workspace/hermes/proposals.json?ids=a,b` | `200 {"proposals": {"a": {"status", "label"}}}` (unknown ids left out; 50 at most) |
| `POST /workspace/hermes/proposals/:id/decision` `{"decision": "confirm" \| "dismiss"}` | `201 {"status", "message", "card", "url", "chip", "warning"}`; `422 invalid_decision`, `422 not_pending` (with its state), `403` (policy, or the dial now Never), `404` (also when the user isn't in the proposal's room and isn't a duty manager), `502` |
| `POST /workspace/hermes/actions/:id/undo` | `200 {"ok", "message", "chip"}`; `422 changed_since` / `already_undone` / `too_old` / `cannot_undo` with the reason; `403` (D8); `404`; `502` |
| `POST /hermes/:bot_key/workspace/proposals` (the proposal JSON, plus the bridge's `context`: `{"source", "room_id", "room_name", "user_id", "user_name", "message_id"}`) | Hermes's bot only (`403` for people; `403 {"status": "refused", "error": "not_hermes_bot"}` for another bot). `201 {"status": "done", "id", "action", "card", "url", "message", "warning"}`; `202 {"status": "pending", …, "draft_message_id", "duplicate"}`; `403 {"status": "refused", "error": "never"}`; `404 {"status": "not_found"}`; `422 {"status": "invalid", "error": code}`; `502 {"status": "failed"}` |
| `GET /workspace/handover` | Phase 2.6, duty managers: the handover page (HTML); `403` (a notice) for others |
| `POST /workspace/handover` `{"text"}` | Duty managers: posted in the handover room as them. `201 {"message_id", "url", "message"}`; `403`; `422 no_room` / `not_a_member` / `blank_text` / `text_too_long` |
| `GET /hermes/:bot_key/workspace/proposals/:id` | Hermes's bot only: `200 {"status": "pending"\|"done"\|"dismissed"\|"expired"\|"failed"\|"superseded", "id", "action", "card", "url", "message", "decided_by"}`, the bot's own proposals; else `404` |

## Crate layout

| Path | What |
|---|---|
| `crates/workspace` (`campfire_workspace`) | The feature. Depends on no `campfire_*` crate (askama, base64, jiff, regex, serde, serde_json, and tokio for its `sync::Mutex` only), so `cargo test -p campfire_workspace --offline` runs anywhere, without libvips or the parity seed |
| `…/src/config.rs` | `WorkspaceConfig::from_lookup`, `is_public_request`, `Secret` |
| `…/src/fizzy.rs` | Fizzy types (lenient decoding), `HttpClient` trait (the app implements it), `Client` (paths, pagination), `@mention` reading from rich text |
| `…/src/cache.rs` | `Snapshot` and the poll |
| `…/src/chips.rs` | Card URL matching, chip HTML, message decoration |
| `…/src/drafts.rs` | Draft detection, pending drafts, `Decision`, the buttons |
| `…/src/home.rs`, `templates/workspace/home.html`, `_card.html` | Home view-model and template |
| `…/src/overlay.rs`, `templates/workspace/_tab_bar.html`, `_nav.html` | Tab bar (and the room panel it carries), pages' nav, page location |
| `…/src/settings.rs` | `Settings` (version 2), `Department`, `Policy`/`Act` (the policy point), `Autonomy`/`Dial`/`ActionKind` (the dial), `SettingsStore` (the JSON file, atomic serialized writes, fail-closed reads) |
| `…/src/store.rs` | The files under `hermes/`: atomic JSON documents, JSON Lines appended and rotated by size, compaction |
| `…/src/journal.rs` | `ActionLog`: the durable write log (`actions.jsonl`) |
| `…/src/hermes_log.rs`, `templates/workspace/hermes.html` | `HermesLog` (`hermes-log.jsonl`), `Entry`, `Reverse` (undo), Hermes's direct actions from Fizzy's feed, Campfire's own writes recognized |
| `…/src/proposals.rs` | `Proposal`, `ProposalStore` (`proposals.json`), parsing, the draft and its marker, confirmed live reports |
| `…/src/hermes.rs` | `Workspace::{propose, decide, undo, poll_hermes, hermes_page, proposal_visible, proposal_visible_to_room}`, the Hermes tab's view |
| `…/src/visibility.rs` | Phase 2.7: `Visibility` (`Mode`, `Untagged`), `Audience`, `Settings::{audience, card_visible, notice_rooms}`, `Directory` (people, rooms, memberships, as the app reads them) |
| `…/src/alerts.rs` | Phase 2.5: `Notifications` (the settings), `detect`, `plan` (`Delivery`, `To`), `NotifiedStore` (`notified.json`) |
| `…/src/handover.rs`, `…/src/shifts.rs`, `templates/workspace/handover.html` | Phase 2.6: `HandoverSettings`, the summary, the posted HTML, `Workspace::{handover_page, handover_message, handover_posted}`; shift ends and time zones |
| `…/src/writes.rs` | `TokenSource`/`SharedToken`/`ActingIdentity`, `Writer` (every Fizzy write, audited, with its `Purpose`), `ActionError`, `WriteRecord`, toggle diffing |
| `…/src/actions.rs` | `Change`, `Target`, `NewCard` (input parsing and validation); `Workspace::{authorize, card_sheet, change_card, create_card}` |
| `…/src/pages.rs`, `templates/workspace/{board,sheet,_panel,_list_card,new_card,settings}.html` | Board, card sheet, room panel, new-card form, settings page, the bot's settings JSON |
| `…/src/lib.rs` | `Workspace` (state shared by poll, hooks and routes: snapshot, settings, token source, audit sink), `ChatSource` trait (the app implements it) |
| `crates/campfire/src/controllers/workspace.rs` | **The adapter**: boot (`build`, `start`: bots, hooks, poll task, the write log), the directory and the alerts' delivery from the poll task (phase 2.5/2.7), the actions, `ChatSource` over campfire_db (one SQL query: the user's rooms' messages of the last day), a draft's reporter (one SQL query), rooms and people for the settings, `HttpClient` over `integrations::net` (GET, POST, PUT, DELETE) |
| `crates/views/src/hermes.rs` (end) | `WorkspaceHooks`, `install_workspace_hooks`, the seam functions; `PageAssets`, `install_page_assets`, `head_tags` (fork-owned file) |
| `crates/assets/overrides/hermes/workspace.css`, `workspace.js`, `workspace_logic.js`, `home.svg`, `board.svg`; `crates/assets/tests/js/workspace_logic.test.mjs` | Frontend (the Hermes tab's icon is upstream's `bot.svg`). Under `hermes/`, which `build.rs` leaves out of `stylesheet_link_tag :all`; the script isn't pinned in the import map (only `pin_all_from` directories are), so upstream's asset tags stay the reference's. The stylesheet is linked from the head seam (docs/hermes-theme.md) |

## Seams in upstream-owned files

Every one is marked `Hermes fork:` in the file. Line numbers as of this commit.

| File:line | What | Why |
|---|---|---|
| `Cargo.toml:29-30` | `campfire_workspace = { path = "crates/workspace" }` in `[workspace.dependencies]` | Shared dependency versions live in the root (AGENTS.md). The crate itself is a member through `members = ["crates/*"]` |
| `crates/campfire/Cargo.toml:21-22` | `campfire_workspace.workspace = true` | The adapter uses it |
| `crates/campfire/src/config.rs:69-71` | `Config::workspace: Option<WorkspaceConfig>` | The app's config carries the parsed env |
| `crates/campfire/src/config.rs:217-218` | `workspace: WorkspaceConfig::from_lookup(&get)?` | Parsing lives in the workspace crate |
| `crates/campfire/src/app.rs:47-48` | `AppState::workspace: Option<Arc<Workspace>>` | Actions reach it with `c.app()` |
| `crates/campfire/src/app.rs:112-113, 126` | `let workspace = controllers::workspace::build(&config);` and the field in the `AppState` literal | Built before `config` moves into the state |
| `crates/campfire/src/app.rs:129-130` | `controllers::workspace::start(&app).await;` | Bots, hooks and the poll task need the booted app; while off it only makes sure no hooks are installed. It also installs the fork's page assets and theme (`install_page_assets`), workspace on or off |
| `crates/campfire/src/controllers.rs:48-49` | `pub mod workspace;` | The adapter module |
| `crates/campfire/src/controllers.rs:329-354` | Twenty rows in `HERMES_ROUTES` (three for phase 0, nine for phase 1, six for phase 2, two for phase 2.6) | The fork's own route table (the Rails table stays identical to `bin/rails routes`) |
| `crates/views/src/messages/presentation.rs:19-20` | `MessageContent::Text { html } => crate::hermes::workspace_message_html(message, html)` | The one hook in message rendering: chips and draft buttons, at render time |
| `crates/views/templates/layouts/application.html:26` | `{{ crate::hermes::head_tags(ctx)\|safe }}` after `custom_styles_tag`, same line | The head seam (docs/hermes-theme.md): `tokens.css`, `hermes.css`, `workspace.css` while the workspace is on, then `theme.css`, each with `data-turbo-track="reload"`, after Custom styles so the theme wins, and with the theme the light/dark switch's inline script. Renders `""` until the app installs its page assets (so the goldens keep upstream's bytes), and being on the same line adds no whitespace |
| `crates/views/templates/layouts/application.html:55` | `{{ crate::hermes::workspace_overlay(ctx)\|safe }}` after the lightbox include, same line | The body seam: scripts and tab bar. Renders `""` while off, and being on the same line adds no whitespace |
| `crates/views/askama.toml:1-5` | `dirs = ["templates-hermes", "templates"]` | Template shadowing (docs/hermes-theme.md): a file in `templates-hermes/` replaces upstream's at the same path. `crates/views/build.rs` (new, fork-owned) makes a new shadow trigger a rebuild |
| `crates/campfire/src/config.rs:33-34, 72-73, 219-220, 245-252, 376-388` | `CAMPFIRE_THEME`, on unless `off`: module docs, `Config::theme`, its parsing, `theme_switch`, a test | The theme's kill switch (docs/hermes-theme.md) |
| `crates/views/src/helpers/application.rs:12-15` | `page_title_tag` falls back to `crate::hermes::product_name()` | Branding (docs/hermes-theme.md, "Branding"): "MeshDuty" once the app installed it, upstream's "Campfire" in the goldens |
| `crates/views/src/sessions.rs:39-40`, `crates/views/src/first_runs.rs:18-19` | The Apple Messages and first-run titles name `crate::hermes::product_name()` | Branding |
| `crates/views/src/helpers/translations.rs:15-19` | The translation popups' sentences go through `crate::hermes::rebrand` | Branding |
| `crates/views/src/pwa.rs:23-24, 36-73` | `Manifest::has_logo` and the methods the shadowed `pwa/manifest.json` calls | Branding: name, short name, colours, default maskable icon |
| `crates/campfire/src/controllers/pwa.rs:29-48` | The manifest action also reads whether a logo is attached | Branding: an uploaded logo stays the maskable icon |
| `crates/campfire/src/integrations/web_push.rs:203-215` (and its test, `web_push/tests.rs:286-288`) | The test notification's title is `"{product_name} Test"` | Branding |
| `crates/db/src/models/first_run.rs:18-30`, `crates/campfire/src/controllers/first_runs.rs:41-43` | `FirstRun::create_named`; the first run names the account after the product | Branding: a new install's account is "MeshDuty" (upstream: "Campfire") |
| `crates/views/tests/parity_a.rs:412, 431`, `crates/views/tests/support/facts.rs:86-92` | `has_logo: false` in the manifest goldens' view models; the fork's `hermes/` assets resolve in the goldens' context | Branding tests (`tests/hermes_branding.rs`) render upstream pages through the golden facts |

Phases 1 and 2 added no seam: only rows in the existing `HERMES_ROUTES` block. Phase 2.7's per-viewer
chips live behind the existing message hook (it marks links instead of rendering chips while
visibility is restricted), and the alerts are posted from the adapter with the app's own functions. Everything else is
in fork-owned files (phase 2's new environment variables, `HERMES_FIZZY_TOKEN` and `HERMES_BOT`, are
read by the workspace crate's own `WorkspaceConfig::from_lookup`; the adapter reads
`GEMINI_LIVE_VOICE_BOT` from the app's existing config).

Not seams (fork-owned or new files): `crates/workspace/**`, `controllers/workspace.rs`,
`crates/views/src/hermes.rs`, `crates/views/tests/workspace_hooks.rs`, `crates/views/tests/hermes_head.rs`,
`crates/views/build.rs`, `crates/views/templates-hermes/**`, `crates/views/script/check-shadowed`,
`crates/views/tests/shadowed_templates.rs`, `crates/views/tests/hermes_branding.rs`,
`crates/assets/overrides/hermes/*`, a test in `crates/assets/tests/reference.rs`, a row in
`crates/assets/OVERRIDES.md`, this document.

## After each upstream merge

1. `git grep -n "Hermes fork" -- ':!docs' ':!crates/workspace'` and compare with the table above:
   every seam still there, once.
2. `cargo test -p campfire_workspace --offline` (the feature, anywhere), and
   `node --test crates/assets/tests/js/*.test.mjs` (the page logic, Node 22, no npm).
3. `cargo test -p campfire_views --offline`: the goldens (hooks and page assets not installed =
   upstream's bytes), `tests/workspace_hooks.rs` (the overlay lands once after the lightbox; removing
   the hooks gives upstream's bytes back) and `tests/hermes_head.rs` (the head seam's links land
   once, after Custom styles, in order). If upstream moved the lightbox include, the `app-logo` link
   or `custom_styles_tag`, move the seam and fix those tests.
4. `cargo test -p campfire_assets --offline`: the import map and `stylesheet_link_tag :all` still
   equal the reference's; the workspace assets are served, not linked, not pinned.
5. `crates/views/script/check-shadowed` (docs/hermes-theme.md, "Template shadowing"): every
   shadowed template still has its upstream file (exit 1 otherwise), no bare-name include and no
   `include_str!` of a shadowed template (exit 1); exit 2 lists the shadowed templates upstream
   changed since they were copied, with the diff: port each change into the shadow, then record
   the new `git hash-object` in `templates-hermes/SHADOWED.md`.
6. If upstream changed any of what `controllers/workspace.rs` borrows, fix that file (the compiler
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
   - phase 2.5–2.7: `controllers::messages::canonicalize_body`, `Message::create`/`NewMessage`,
     `Room::find_direct_for`/`find_or_create_direct_for`, `Membership::for_room`,
     `Presenter::message`/`sidebar_direct`, `presenters::page::{render_detached, Rendered}`,
     `broadcasts.message_create`/`direct_room_create`, `campfire_views::messages::message`,
     `campfire_views::users::direct_room`, `AppState::fragment_cache` (`clear`), and the whole
     `memberships` table (`user_id`, `room_id`);
   - db: `Message::find_reachable`, `Message::find`/`room`/`creator`/`body_html`/`created_at`,
     `Room::find`/`find_for_user`/`all`/`direct`/`name`, `User::active_bots_ordered`,
     `User::active_ordered_without_bots`, `User::is_administrator`/`is_bot`, `User::email_address`,
     and the `messages`/`memberships`/`users.role` columns used by `recent_messages` and
     `reporter_of`;
   - HTTP: `campfire_richtext::uri::parse` (`host`, `port`, `scheme`, `is_http`),
     `integrations::net::Network::system`, `integrations::net::http::{Request::net_http(…).transport(…),
     Request::body, request_uri, exchange, Endpoint, Timeouts, Body}` and the response's
     `status`/`header`/`read_body`.
7. If upstream changed the room page's DOM (`.message[data-user-id][data-message-id]` siblings,
   `.message__actions-grid` in the message menu, `form#composer`, `[data-composer-target=text]`,
   the composer controller's `replaceMessageContent`, `#nav .room--current`,
   `meta[name=current-room-id]`) or the layout grid (`body`'s `grid-template-areas`/`-columns`
   with `--sidebar-width`, `#main-content`, `#sidebar` z-index, the `100ch` breakpoint, `#nav`
   being `position: fixed` up to `inset-inline-end: var(--sidebar-width)`, which the open panel
   moves to end where the panel starts): check
   `hermes/workspace.js` and `workspace.css` in a browser (phone and desktop, light and dark): the
   room panel, the message menu's card entry, the overlay, the board's column switcher.
8. `cargo clippy --workspace --all-targets` and the parity gate as usual; with the workspace **off**
   and `CAMPFIRE_THEME=off` the parity screenshots must not move (the head still links
   `tokens.css` and `hermes.css`, whose rules only match the fork's markup; the owner retired the
   gate for the theme itself, docs/hermes-theme.md). The server-HTML and live-DOM parity cells,
   though, now differ from the reference in `<head>` on every page, even with `CAMPFIRE_THEME=off`:
   the head seam's `<link>`s (`hermes/tokens.css`, `hermes/hermes.css`, and `hermes/theme.css` when
   on) need a mask or allowlist entry in `parity/` before those cells can pass.
9. If Fizzy changed (a new image): re-check the write endpoints against
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
  `crates/views/tests/hermes_head.rs`: the head seam's order (upstream, Custom styles, `tokens.css`,
  `hermes.css`, `workspace.css`, `theme.css`, then the light/dark script), `CAMPFIRE_THEME=off`,
  nothing while not installed; the profile's Light / Dark / System switch only with the theme on.
  `crates/assets/tests/theme.rs`: the theme's two dark blocks identical, every `--lch-*` triplet
  the OKLCH of its colour, the fonts digested, served and licensed.
  `crates/views/tests/shadowed_templates.rs`: every file in `templates-hermes/` listed in
  `SHADOWED.md` with an existing upstream template, no bare-name include, no `include_str!` of a
  shadowed template (and the checks' own parsers).
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
  (fields, escaping, controls disabled by the policy; with 250 comments in geared pages the newest
  100, in three requests, under "Show earlier comments", also without `X-Total-Count`; with
  `comments=all` all 250, and 1,000 of 1,030 with the note, `429 busy` with every permit held and no
  Fizzy request, a thread past the page cap without `X-Total-Count` flagged as truncated; no Fizzy link for staff, the "⋯" menu
  with `fizzy_links`; `fizzy_links` only for duty managers and administrators on the LAN), the
  board (columns, filters, Move menus, settings link, the "⋯" menu only with `fizzy_links`), Home
  (every tile, mention and handover card opens the sheet; the board's Fizzy link only with
  `fizzy_links`), the Hermes log's card lines (the sheet; Fizzy only with `fizzy_links`), public
  requests (`is_public_request`: unset, LAN-only, tunnel host, ports, case, a forged
  `X-Forwarded-Host`), the earlier comments of a hidden card (404, no comment read), room panels, the prefilled form, the settings page and the bot JSON; settings
  (validation, normalization, atomic save and reload, a broken, invalid or unreadable file failing
  closed while a missing one is the defaults, the settings page's warning, a failed write being an
  I/O error that changes nothing, concurrent saves ending with file and memory agreeing), the
  policy table.
- Phase 2, in `crates/workspace` against the stateful fake Fizzy (now also: tokens' identities,
  comment ids and their author's token for deletion, steps added, titles, the activity feeds):
  settings v1 → v2 (defaults, partial maps, a bad dial failing closed, the Hermes user id), `Undo`
  permissions under every policy; the write log (append, rotation, torn lines, read back after a
  restart, a failed append reported once, never a token); proposals by the dial (Alone runs at once
  as Hermes with its token and is logged with its way back; Ask first writes nothing, is kept and
  logged, the same proposal twice is one; Never is refused and logged; parse refusals: delete,
  assign, unknown columns and departments, bad tags and steps; a card on another board is 404;
  nothing kept for any of them), confirming (the draft's marker gets buttons only on the message
  Campfire posted for it, not before it's known nor in another message of the bot, and isn't a text
  draft, the card created with its tags, steps and link back, as Hermes,
  "confirmed by", a second File refused with who did it, Dismiss), expiry after 24 h, the confirm
  policy on proposals, confirmed live reports (once, only the reporter's own; one still running
  counts; eight concurrent proposals from one report file one card), the proposals' eviction
  (decided first, never pending), only Hermes's bot (`HERMES_BOT`, `GEMINI_LIVE_VOICE_BOT`, the only
  bot, several bots refused), without
  `HERMES_FIZZY_TOKEN` ("Hermes: " comments with the workspace token); undo of each reverse action
  (severity, move, comment deleted with the token that wrote it, a created card closed with a
  comment, a direct creation), and its refusals (D8, changed since in Campfire, state changed,
  a later change in Fizzy's feed, on page 2 of it too, too much activity to check within 10
  pages, too old, already undone, a direct move); the Hermes log (only
  Hermes's activities on the incident board, deduplicated, kept across a restart, Campfire's own
  proposal writes not logged twice, people's workspace actions left out when the workspace shares
  Hermes's token), the Hermes tab (pending with buttons, via labels, undo buttons by permission,
  filters, questions from the viewer's rooms, the notice when Hermes's user is unknown; a
  proposal's details and unfiled lines only for its room's members and duty managers, a roomless
  one for duty managers only), Home's pending proposals, the proposals' states JSON; chips re-read after 15 minutes, a deleted card
  dropped.
- Phase 2.5–2.7, in `crates/workspace` (`src/tests/phase2b.rs` against the fake Fizzy, and each
  module's own tests): the visibility matrix (restricted, unrestricted, several departments, no
  department either way, duty managers, administrators, the `everyone` mode, a damaged file) and
  every surface (chips per viewer, the shared message HTML only marked, Home's open incidents, 12 h
  list, mentions and "No department", the board, room panels, the card sheet and a change refused
  with nothing written, the Hermes tab's lines, pending proposals and where their draft may be
  posted, membership refreshed by requests); notice rooms; the alert detector (nothing at the first
  poll, a card filed since, a severity raised, medium not alerting, once per key, nothing again
  after a restart even when the severity flaps, alerts off, listed duty managers, no room notice when
  off, no bot = dropped and counted; cards created, re-rated, filed by a proposal alone or
  confirmed, or read by the sheet after a Fizzy change alert once at the next poll), reminders
  (still in New after 15 minutes, a proposal's person then the duty managers, only what the person
  may see, once; nothing stale after alerts are turned back on or a delay shortened), new serious
  incidents first in a capped message, the handover reminder at a shift end; a proposed card's
  departments from `tags`; unknown room members (directory not read, empty room) seeing nothing
  restricted; a restricted department in an open room refused;
  `notified.json` (claims across a restart, 14 days, a broken file); the handover (duty managers
  only, the nearest shift, sections, hidden cards counted not named, not a member / no room, posted
  HTML escaped with links, the next one from the last posted, or from an early one, a long summary
  cut to fit); shift ends across midnight and daylight saving (Paris by its POSIX rule), time
  zones, settings v1/v2 → v3 (defaults, nothing restricted, no handover room), invalid values
  failing closed, an unknown time zone read as UTC with a warning; the settings page's new sections.
- `crates/assets/tests/js/workspace_logic.test.mjs` (`node --test`): chip versions, chip numbers,
  proposal ids, reply handling (signed out, server messages), drafts' status lines, proposal states,
  card sheet change bodies, the settings body (with visibility, alerts, handover; an invalid
  reminder delay is an error, not a default), shift ends,
  which chips go back to plain links, the handover's checks.
- `crates/campfire/src/controllers/workspace.rs` (needs the app to link, i.e. libvips; runs in CI):
  route recognition (all routes), `FizzyHttp` against a fake server (headers, query, `Link`,
  `X-Total-Count`, 404,
  errors without the token, a POST's method and body), settings ids; request tests against the
  parity seed (skip without it): phase 1 routes 404 while off, errors before Fizzy answers (502,
  422, 404, the form, the panel's 204), settings saved by an administrator and read by a bot (a restricted department in an open room refused);
  phase 2 routes (404 while off, the Hermes tab, decisions and undo of unknown ids, the proposals'
  states, the bot route refusing people and bots other than `HERMES_BOT`, a proposal that doesn't
  parse, Fizzy down, no token in the reply).

Not covered by automated tests (phase 2.5–2.7): the alerts' actual posting (the direct room created,
Web Push on a phone), the handover's `POST` end to end, the page script's chip downgrade and handover
form in a browser, and the adapter's new request paths (they need libvips, like the others).

Not covered by automated tests (phase 2 too): the DOM behaviour of `hermes/workspace.js` (proposal
buttons' states, Undo, the settings' dial), the bridge ↔ Campfire proposal route end to end, and
Fizzy's real activity feed for Hermes (the `creator_ids[]`/`board_ids[]` filters, `particulars`,
the comment ids a reply carries). Also: the actions end to end against a real Fizzy (writes are only
tested against the fake, and request tests only reach Fizzy-down paths), `hermes/workspace.js` (no
JS test harness in the repo), and the look of the CSS. Check in a browser after deploying: a chip
in a room (a click opens the sheet), File/Dismiss/Edit on a real Hermes draft (the Edit prefill
relies on Lexxy accepting an `<action-text-attachment … content="…">` in `replaceMessageContent`),
the tab bar on a phone (composer not covered, hidden while typing), Home; for phase 1: the
settings page (add a department linked to a room), the room's "N cards" button and panel (wide:
a column between the conversation and the sidebar; phone: a sheet), the message menu's card
entry and the form, the board (phone: one column and the switcher; wide: columns side by side),
Move, the sheet (severity twice, a department, a step, a comment, "Show earlier comments", the
"⋯" menu as a duty manager on the LAN and its absence through the public hostname), Home's tiles
opening the sheet, light and dark.

## Deliberately deferred or different from the mockup

Phase 2 (2.0, 2.2–2.4):

- **Hermes keeps its own Fizzy write token** (D7 = no): the dial binds proposals only; direct
  actions are logged ("direct") as far as Fizzy's feed tells and only simple ones can be undone.
- **Direct tags and steps aren't logged** (Fizzy's feed has no event for them), and a direct move
  can't be undone (the feed gives only the new column).
- **"For whom" of a direct action** is the report's "Reported by" text, unverified; only duty
  managers can undo it.
- **A typed "@Hermes confirm" under a proposal's draft** doesn't confirm it (Hermes can't confirm its
  own proposal); the skill answers to press File.
- **Steps default to Ask first** (D5 didn't list them).
- **The Hermes tab's bot messages** (drafts, questions, errors) come from the viewer's own rooms
  only, so nobody reads a room they aren't in there.
- Not in this build: 2.1's bridge part, 2.8 personal Fizzy logins.

Phase 2, second batch (2.5–2.7):

- **Alerts are at most once**: a message that fails to post (Hermes's bot not in a room, the database
  busy) is logged, not retried. No quiet hours or duty roster: every duty manager is alerted (D9).
- **No alert for text drafts**: the waiting-draft reminder is for Hermes's proposals (2.3), which the
  skill uses by default; its text-draft fallback isn't reminded.
- **Not built**: D13's alert when Fizzy moves an untouched card to "Not Now" (the plan made it ride
  on 2.5; a card moved there on purpose looks the same to the poll).
- **Handover**: post only (D10), no Fizzy card; "closed this shift" is by the card's last activity.
  Only duty managers post it (not the whole team).
- **Visibility is Campfire's only**: Fizzy logins see everything, and so does Hermes; membership seen
  from the layout's room panel can be one poll old.

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
- **Open tickets** (named *Open incidents* before the broader tickets) = every open card of the
  incident board, not "cards I own + unowned" (per the
  task); owners are shown on each card.
- **Mentions** come from Fizzy comments only; Campfire @mentions already notify through Campfire.
- **In-memory cache**, not SQLite: the first poll after a restart rebuilds it (activity pages 1–3,
  so mentions older than that aren't recovered).
- **Chips on every message**, not only Hermes's; unknown card numbers are fetched lazily.
- **Anyone in the room** can answer a draft; since phase 1 that's the settings' policy (`anyone`
  by default; `author_or_duty_manager` is "only the author or the Duty Manager").
