# Hermes fork: live voice tickets (Gemini Live)

An employee opens a room's voice page, talks a **ticket** through with a Gemini Live voice
interviewer, in **any language**, that asks only for what's missing, confirms the recap, and the
ticket is posted **in the room, as that employee**, with an @mention of the Hermes bot. A ticket is
any operational request, task, fault, complaint, incident or safety issue: "refill the water
bottles in room 101" (a low request), "fix the lift in building 7" (a high fault; critical with
people stuck), "the guest in room 403 complained about the noise" (a medium complaint). The mention fires the bot's webhook
exactly as a typed @mention would, so the Hermes bridge and its `incident-report` skill take it
from there (Fizzy card etc.).

During the interview the assistant can also **ask Hermes** (`ask_hermes`, with `HERMES_ASK_URL`):
a procedure, the tickets already open on the Fizzy board, a contact… It says it's checking with
Hermes (in the employee's language), the page forwards the question through Campfire to the Hermes bridge, and the
assistant speaks the answer and carries on with the interview.

This is a fork-only feature: none of it exists in the reference app or upstream
`once-campfire-rust`. Everything is off unless `GEMINI_API_KEY` is set.

## Flow

```
Browser (Campfire session)                 Campfire (this fork)                    Gemini / Hermes
GET  /rooms/:id/voice ───────────────────▶ membership check, page
POST /rooms/:id/voice/token ─────────────▶ POST v1alpha/auth_tokens (x-goog-api-key)
     ◀── {token, ws_url, model, expires_at}: single use, 1 min to open, 30 min life,
         model/instructions/tools/transcription locked in the token
mic 16 kHz PCM ══WSS (BidiGenerateContentConstrained, {"setup":{}})══▶ Gemini Live
     ◀══ 24 kHz audio + input/output transcriptions
tool call ask_hermes(question)             (only with HERMES_ASK_URL; Gemini waits for the answer)
POST /rooms/:id/voice/ask ───────────────▶ POST HERMES_ASK_URL ─────────────────▶ bridge /ask/<secret>
     ◀── 200 {answer} ◀──────────────────── {answer} ◀──── Hermes /v1/responses ◀┘
toolResponse {result: answer} ══▶ Gemini speaks it, resumes the interview
tool call submit_incident(...)
POST /rooms/:id/voice/report ────────────▶ message as Current.user, "@Hermes …"
                                            broadcast + deliver_webhooks_to_bots ──▶ bot webhook
     ◀── 201 {message_id, message_url}
```

The API key never reaches the browser. The browser only gets a single-use ephemeral token whose
`bidiGenerateContentSetup` fixes the model, the interviewer instructions (English, multilingual —
see *The interviewer* below — with the room and user names and the browser's languages frozen in,
as quoted data), the `submit_incident` declaration (plus `ask_hermes` and its
paragraph of instructions when `HERMES_ASK_URL` is set), audio transcription both ways, session
resumption and sliding-window context compression.

## Configuration

| Variable | Default | Meaning |
|---|---|---|
| `GEMINI_API_KEY` | unset (feature off) | Gemini API key (paid tier: the free tier's terms exclude EEA/CH/UK end users and allow human review). Server-side only; never logged. |
| `GEMINI_LIVE_MODEL` | `models/gemini-3.8-live` | Live model; a bare name gets `models/` prepended. |
| `GEMINI_LIVE_VOICE_BOT` | unset | The bot the report @mentions: a bot **user id** (e.g. `3`) or its **exact name** (e.g. `Hermes`). It must be an active bot **member of the room**. Unset: the room's only active bot; a room with none or several answers 422 `bot_not_in_room`. |
| `GEMINI_LIVE_TOKENS_PER_HOUR` | `10` | Tokens one user may mint per rolling hour (in memory, per process) before 429. |
| `GEMINI_LIVE_EXTRA_INSTRUCTIONS` | unset | Text appended to the interviewer's system instruction (e.g. site-specific questions). |
| `HERMES_ASK_URL` | unset (`ask_hermes` off) | The Hermes bridge's ask endpoint, e.g. `http://campfire-bridge:8645/ask/<BRIDGE_ASK_SECRET>`. Its path is a secret: never logged, never sent to the browser (`Debug` redacted, and a malformed value fails the boot without being quoted). Only read with `GEMINI_API_KEY`. |
| `HERMES_ASKS_PER_HOUR` | `30` | `ask_hermes` questions one user may ask per rolling hour (in memory, per process) before 429. |

Browsers only allow the microphone on a secure origin: serve Campfire over HTTPS (or localhost).

## Routes and contract

All four answer 404 while the feature is off (`/voice/ask` also while `HERMES_ASK_URL` is unset), run `ApplicationController`'s chain (session
cookie only, bots denied, `Sec-Fetch-Site` forgery protection on the POSTs, so same-origin `fetch`
needs no token) and `RoomScoped#set_room` (404 unless the user is a member of the room). They live in
a separate route table tried after the Rails one (`controllers::HERMES_ROUTES`), so the Rails table
still matches `bin/rails routes`.

### `GET /rooms/:room_id/voice`

HTML page in the application layout (Turbo-Frame requests get the frame layout). Root element:

```html
<section class="voice" data-controller="voice"
  data-voice-token-url-value="/rooms/:room_id/voice/token"
  data-voice-report-url-value="/rooms/:room_id/voice/report"
  data-voice-ask-url-value="/rooms/:room_id/voice/ask"          only with HERMES_ASK_URL
  data-voice-worklet-url-value="/assets/voice/pcm-worklet-<digest>.js"
  data-voice-room-url-value="/rooms/:room_id"
  data-voice-room-name-value="<room display name>">
  <div data-voice-target="transcript" aria-live="off"></div>      chat bubbles, newest at the bottom
  <div data-voice-target="notice" role="alert" hidden>…</div>      errors (+ « Détails techniques »)
  <div data-voice-target="confirm" hidden>…</div>                  « Recommencer » confirmation
  <div data-voice-target="result" hidden>…</div>                   success card, « Voir le message »
  <div class="voice__bar">                                         bottom control bar
    <div data-voice-target="control">                              mic-level ring (--voice-level)
      <button data-voice-target="toggle" data-action="voice#toggle">…</button>
    </div>
    <p data-voice-target="status">…</p> <span data-voice-target="timer">00:00</span>
    <p data-voice-target="hint"></p>
    Annuler (cancel) · Reprendre (resume → voice#retry) · Recommencer (restart)
  </div>
  <p data-voice-target="announcer" role="status" class="for-screen-reader"></p>
</section>
```

The page (title « Rapport vocal », the room name in the intro) is a column: intro, transcript (the
only part that scrolls, `flex: 1`), and a bottom bar with a 72 px round button: a mic (Campfire's
primary look) to start or resume, a hang-up (`.btn--negative`) while live. `data-voice-state` is
`idle | starting | live | finishing | stopped | closed | error | unavailable | done`; while live,
`data-voice-activity` is `speaking` while the assistant's audio plays (« L’assistant parle… ») and
`listening` otherwise (« À vous », with the mic-level ring from the worklet chunks' RMS and a
running `mm:ss` clock). Right after setup the controller sends a short text instruction
(`KICKOFF_TEXT`) so the assistant speaks first; text input isn't transcribed, so it's neither on
the page nor in the report. « Raccrocher » keeps the transcript and the session: « Reprendre »
(`retry()`) reconnects with a fresh token and replays the transcript; « Recommencer » wipes it after
an in-page confirmation. Publishing is by voice (« Confirmez le récapitulatif à l’assistant pour envoyer
le ticket. »). The page's own texts are still French (Campfire has no i18n); the conversation is
in the employee's language. Error sentences stay plain; HTTP/WebSocket codes and hostnames go in a
« Détails techniques » disclosure and `console.warn`. Screen readers get the step while starting,
each completed assistant turn once (the transcript itself is `aria-live="off"`), and the focus moves
to « Voir le message » when done. Styles: `hermes/hermes.css` (below).

The layout's viewport meta (upstream `layouts/application`) has no `viewport-fit=cover`, so
`env(safe-area-inset-bottom)` is 0 in Safari's browser tab (content already stops above the home
indicator there); the bar still honours it where it's set.

The worklet URL is the Propshaft-digested path of `voice/pcm-worklet.js`, resolved through the asset
manifest (`campfire_assets::try_asset_path`); it's served like every digested asset
(`text/javascript`, immutable), which `audioWorklet.addModule(url)` accepts.

### The interviewer

`Interview::system_instruction` (`gemini_live.rs`) is written in English (the model follows it
best) and tells the model to:

- **Speak the employee's language**, whatever it is (French, English, Spanish, Portuguese, Arabic,
  Tagalog, Hindi…), and switch when they switch. Before they speak, greet in the first of the
  device's preferred languages, which sit in the quoted context block as data (« - device's
  preferred languages, most preferred first: "es-MX", "es" », or `unknown` → a short greeting in
  English). They come from `Accept-Language` (`preferred_languages`: only its first 256 bytes and
  20 entries are read; at most three well-formed tags by quality, each at most 35 characters and 4
  subtags; `*`, `q=0` and duplicates dropped).
- **Take any ticket**: type `request | task | fault | complaint | incident | safety`, where (room,
  building, floor, area), what needs to be done, how urgent; ask only what's missing and matters
  (a simple request needs what and where; incidents and safety issues also when, who, injuries,
  actions taken); never invent.
- **Severity guide**: low (routine request), medium (inconvenience or complaint, fix today), high
  (a service down or someone seriously affected, e.g. a broken lift), critical (people in danger,
  injured or trapped; a lift with people stuck inside is critical).
- **Fixed identifiers**: the ticket's text fields in the employee's language; `type` and
  `severity` are English enum values, never translated; room numbers, building names, people's
  names and codes kept exactly as said ("room 101", "building 7").
- **Never claim the ticket exists**: after `submit_incident` answers ok, say it was sent to Hermes,
  who files it and confirms in the room; no card number. On `already_submitted` (the page answers
  that to a second call in the same conversation), don't call it again: say it was already sent
  (another ticket = a new conversation). On an error, say so and offer to retry.
- **Title**: « verb object — place » (`Refill water bottles — room 101`), the same format as the
  `incident-report` skill's.

The page's kickoff and reconnection texts (`KICKOFF_TEXT`, `recapText`) and the tool errors it
returns to the model are English too; the transcript's labels are `Employee:` / `Assistant:`.
`GEMINI_LIVE_EXTRA_INSTRUCTIONS` is appended under « Additional instructions from the
organization » (any language).

### `POST /rooms/:room_id/voice/token`

No body needed; the `Accept-Language` request header (sent by every browser) picks the greeting's
language. `200`:

```json
{"token": "auth_tokens/…",
 "ws_url": "wss://generativelanguage.googleapis.com/ws/google.ai.generativelanguage.v1alpha.GenerativeService.BidiGenerateContentConstrained",
 "model": "models/gemini-3.8-live",
 "expires_at": "2026-09-29T12:30:00Z"}
```

The browser connects to `ws_url?access_token=<token>` and sends `{"setup":{}}`. Errors are JSON
`{"error": code, "message": French text}`: `429 rate_limited`; `502 upstream_error` or
`502 upstream_timeout` (the whole upstream call is capped at 10 s).

Upstream call: `POST https://generativelanguage.googleapis.com/v1alpha/auth_tokens`, header
`x-goog-api-key`, body `{"uses":1, "expireTime": now+30min, "newSessionExpireTime": now+1min,
"bidiGenerateContentSetup": {...}}`; the reply `{"name": "auth_tokens/…"}` is the token.
Checked against the live API on 2026-09-29 (200 with that shape, including the `enum` in the
function schema).

`submit_incident` (the name is kept; it sends any ticket) parameters (all strings): `title` (short
and actionable: verb and object — place, "Refill water bottles — room 101") and `summary` (required),
`type` (`request` | `task` | `fault` | `complaint` | `incident` | `safety`), `what_happened`,
`location`, `occurred_at`, `people_involved`, `injuries`, `actions_taken`, `severity`
(`low` | `medium` | `high` | `critical`).

### `POST /rooms/:room_id/voice/ask`

The interviewer's `ask_hermes`. JSON body `{"question": "…"}`: whitespace folded to single
spaces, cut at 1,000 characters (`…`). Campfire forwards, server-side, `{"room_id", "user_name",
"room_name" (the display name), "question"}` to `HERMES_ASK_URL` over the `integrations::net`
client (10 s to connect, 60 s in all) and answers `200 {"answer": "…"}`. Errors, JSON
`{"error": code, "message": French text}`: `422 invalid_question` (blank), `429 rate_limited`
(`HERMES_ASKS_PER_HOUR`), `504 upstream_timeout` (no answer within 60 s, or the bridge's own 504:
it gives Hermes 55 s), `502 upstream_error` (anything else). One log line per question: room,
user, lengths and duration, never the text.

`ask_hermes` declaration: one required string parameter, `question` (in the employee's language,
self-contained, room numbers and names as said); an English description (the organization's
internal agent: procedures, tickets already open on the Fizzy board, contacts, anything
organization-specific; files nothing; the answer can take several seconds). No `behavior` field: the default
blocking call was checked against the live API on 2026-09-29 (the model says it's checking, waits
for the `toolResponse`, then speaks the answer; 8 s tested). `NON_BLOCKING` + `INTERRUPT` worked
too, `WHEN_IDLE` never delivered the answer. The system instruction gets a paragraph (before the
quoted context): when the employee asks something organization-specific or such a fact is missing
(e.g. whether this fault is already reported), say briefly, in the employee's language, that it's
checking with Hermes, call `ask_hermes`, give the answer in one or two sentences in the employee's
language and resume; never invent procedures; if Hermes doesn't answer, say so; such questions
aren't off-topic; `ask_hermes` files nothing. `submit_incident` is unchanged.

The page (`#askHermes` in `voice_controller.js`) POSTs the question (same-origin, same headers as
the report) with a 65 s client timeout, and answers the tool call with `{result: answer}` or
`{error: "Hermes did not answer."}`. While it waits the status reads « Hermes consulté… », and the
transcript shows a centred, dashed « Question à Hermes » note (the question, then « En attente de
la réponse… » → « Réponse reçue » or « Pas de réponse »). The answer itself isn't repeated there:
the assistant speaks it, so it's in the assistant's next bubble. These notes stay **out of the
report's transcript** (and of the reconnection recap): the assistant's own lines already carry
the "checking with Hermes" sentence and the answer.

The bridge side (`campfire-bridge/server.py` in the Hermes repo, `BRIDGE_ASK_SECRET`) prefixes
the question with `[user in room]` and a voice-style instruction (answer in the question's
language, 1–3 short sentences, no markdown, room numbers and names as written, say so if unknown), chains it on the room's own `voice:<room_id>` thread (apart from the
room's chat thread), and strips leftover markdown from the answer.

### `POST /rooms/:room_id/voice/report`

JSON body: the `submit_incident` arguments plus an optional `transcript` string. Each field is cut
at 2 KB and the transcript at 20 KB (cut text ends with `…`, and the transcript heading says
"(cut)"); an unknown `severity` or `type` is dropped. `201 {"message_id": 123, "message_url":
"https://host/rooms/:room_id/@123"}`. Errors: `422 invalid_report` (no title or summary),
`422 bot_not_in_room`.

The message is created **as the current user** through `MessagesController#create`'s own path
(`create_message` → `broadcast_create` → `deliver_webhooks_to_bots`), so it's canonicalized,
stored, broadcast, pushed and delivered exactly like a typed message. Its body, every value
HTML-escaped (the text comes from a model and a browser):

```html
<p><action-text-attachment sgid="<bot's attachable sgid>" content-type="application/vnd.campfire.mention"></action-text-attachment> Live voice ticket, confirmed by the reporter.</p>
<h3>title</h3>
<p><strong>Summary:</strong> summary</p>
<ul><li><strong>Type:</strong> request</li><li><strong>What happened / what is needed:</strong> …</li>
<li><strong>Location:</strong> room 101</li> … <li><strong>Severity:</strong> high</li></ul>
<p><strong>Conversation transcript:</strong></p>
<blockquote>Assistant: …<br>Employee: …</blockquote>
```

Only tags the display sanitizer (`ContentFilters::SanitizeTags`) keeps are used (`<details>` is not
one of them, hence the blockquote). Missing fields read *not stated*. The labels, the type and the
severity are fixed English identifiers the `incident-report` skill reads; the values are in the
employee's language. The opening sentence is `campfire_workspace::proposals::LIVE_REPORT_OPENING`,
which the workspace uses to recognize a confirmed live report; the French opening images up to
v0.1.2-hermes.16 posted (« Compte rendu d’incident dicté en direct (voix), confirmé par
l’auteur ») is still recognized (`LEGACY_LIVE_REPORT_OPENINGS`).

### Why the bot's webhook fires (the plan's open point)

In a shared (non-direct) room, `deliver_webhooks_to_bots` calls only the *mentioned* active bots:
`message.mentionees` = the `User` attachables of the stored body (`campfire_richtext::mentioned_users`,
verified SGIDs only) that are members of the room. The server writes the same
`<action-text-attachment sgid=… content-type="application/vnd.campfire.mention">` the composer
inserts, with the bot's `attachable_sgid` (the one the mention autocomplete hands out), and the
body goes through the same canonicalization, so the mention is recognized like a human's.
`controllers/voice/tests.rs` (`a_report_mentions_the_bot_whose_webhook_then_fires_in_a_shared_room`)
proves it end to end: the report in *All Talk* (a closed room) makes Bender's webhook receive the
message. Consequence: **the bot must be a member of the room**, or nothing can be delivered (hence
the 422 rather than a silent message).

## Composer buttons and voice notes

The room composer gets up to two buttons after the attachment (paperclip) button, both from
`hermes/_composer_buttons.html`:

- **Voice note** (microphone, always on: `ShowView::voice_note`, set by `rooms#show`). Tap to record,
  tap again to stop and send; the bin discards (and gives the focus back to the mic). While
  recording, the input row becomes a recording bar (hermes/hermes.css hides the text field, the
  attachment, rich-text and live buttons and the composer's Send, via `:has()`): bin, a blinking red
  dot with the timer, a level meter (`AnalyserNode`), and the record button turned into a pulsing
  send arrow (« Envoyer le message vocal »). At 4:30 « Envoi automatique dans 30 s » replaces the
  meter; recording stops and sends by itself at 5 minutes. `voice_note_controller.js` records with `MediaRecorder`
  (`audio/webm;codecs=opus`, else `audio/mp4` for Safari, else `audio/ogg;codecs=opus`, else the
  browser's default) and names the file `note-vocale-YYYYMMDD-HHMMSS.webm|m4a|ogg` with a plain
  `audio/*` type. The file goes through the composer's own attachment path: the controller fires
  `drop-target:drop` on `window` (what a file dropped on the room fires, which `composer#dropFiles`
  handles) and clicks the composer's Send button, so the upload, its pending bubble, the
  `client_message_id` and the Turbo Stream answer are those of a picked file. Like Send, that also
  sends any text typed in the composer. The button stays hidden where the browser can't record; on
  plain HTTP it shows and explains that HTTPS is needed.
- **Live report** (headset, only when `ShowView::voice_path` is set, i.e. `GEMINI_API_KEY`): a
  link to `/rooms/:id/voice` named « Ticket vocal (demande, panne, incident) », out of the composer's turbo frame
  (`data-turbo-frame="_top"`). On touch screens (no hover title) it shows a small « Rapport » label
  under the icon, in the round buttons' footprint. It replaces the room nav's mic button of
  v0.1.1-hermes.2.

On touch screens (`pointer: coarse`) every composer button is 2.75rem (44 px).

Audio attachments (`audio/*`, e.g. voice notes, whatever recorded them) render as
`<audio controls preload="metadata">`, full width of the bubble (`AttachmentPreview::Audio`,
`hermes::audio_preview`). A voice note (`note-vocale-*`) gets a compact « Message vocal · 0:07 »
line and a download button instead of the file-name row; the `voice-player` controller fills in the
duration once the browser knows it (Chrome's MediaRecorder WebM has none until played). Other audio
files keep the reference's file link (name, download, share) under the player.

All these styles live in one stylesheet, `crates/assets/overrides/hermes/hermes.css`, linked by the
Hermes templates themselves (the composer partial, the voice page). `build.rs` leaves `hermes/` out
of `stylesheet_link_tag :all`, so every page without Hermes features keeps the reference's exact
`<link>` tags and the parity tests hold.

## Where the code is

| Path | What |
|---|---|
| `crates/campfire/src/config.rs` | `GeminiLiveConfig`, `ApiKey` (redacted `Debug`) |
| `crates/campfire/src/integrations/gemini_live.rs` | Token request body, system instruction, `submit_incident` and `ask_hermes` declarations, `HttpMinter` (the existing `integrations::net` HTTP/1.1 + rustls client, no new crate), `TokenMinter` trait (tests swap it), rate limiters, the asker |
| `crates/campfire/src/integrations/hermes_ask.rs` | `HttpAsker` (`POST HERMES_ASK_URL`, same client), `HermesAsker` trait (tests swap it), question cap |
| `crates/campfire/src/controllers/voice.rs` | The four actions, `IncidentReport` (caps, escaping, markup) |
| `crates/campfire/src/controllers/mod.rs` | `HERMES_ROUTES`, tried after the Rails table |
| `crates/campfire/src/app.rs` | `AppState::gemini_live` |
| `crates/views/src/hermes/`, `crates/views/templates/hermes/` | The page, the composer's voice buttons (`_composer_buttons.html`) and the inline audio player |
| `crates/views/templates/rooms/show/_composer.html` | The one upstream-template insertion: `hermes/_composer_buttons` after the attachment button |
| `crates/assets/overrides/controllers/voice_controller.js`, `voice/pcm-worklet.js`, `controllers/voice_note_controller.js`, `controllers/voice_player_controller.js`, `hermes/hermes.css`, `headset.svg`, `phone-hangup.svg`, `microphone.svg` | Frontend |
| `crates/assets/build/importmap.rs` | `pin_all_from` also picks up files the overrides *add* (otherwise `controllers/voice_controller` would never be pinned or registered) |

## Tests

- `integrations::gemini_live::tests`: request body shape, quoting of names, extra instructions, rate
  limit, `ask_hermes` declared (and its instructions added) only when enabled, and the HTTP minter
  against a fake server (path, `x-goog-api-key`, body; 403 and garbage replies are errors).
- `integrations::hermes_ask::tests`: the asker against a fake bridge (path, JSON body, answer;
  504 → timeout, other statuses and blank answers are errors, the secret never in an error).
- `controllers::voice::tests`: feature off → 404 and no live button (the voice-note one stays);
  the page's data values and worklet URL (served as JavaScript); token route builds the locked setup through an injected
  minter, forgery protection, membership, 429, 502; report membership / validation / no-bot room;
  the bot-mention webhook proof above; report escaping and caps; route order; `ask`: 404 without
  `HERMES_ASK_URL` (and no `ask_hermes` in the token), forwarding to a fake bridge and the answer,
  the question cap, membership / forgery / blank question, 504 on timeout, 502, 429.
- `crates/views/tests/hermes_views.rs`: the page renders; the composer's buttons are absent with both
  flags off (the goldens' input) and present, in place, with each on; voice notes get a player and a
  compact line, other audio files keep their file link.
- `crates/assets/tests/reference.rs`: the import map equals the reference's plus the added
  controllers' pins; `hermes/hermes.css` is served but not in `stylesheet_link_tag :all`.

The request-level tests need the parity seed (`parity/bin/seed build`) and skip without it.

## Privacy

Transcripts are personal data (GDPR): they end up in the room's message and in whatever the bot
files. Decide on retention, or drop the transcript from the message if a summary is enough.
