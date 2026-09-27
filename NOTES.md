# Cross-crate requests

Add requests under the owning crate's heading. The owner removes an entry once it's handled.

## rails_compat
## kit
## routes
## db
- (from campfire, deadlock fix) `RichText::to_plain_text`/`mentioned_user_ids` now take the caller's
  `&Connection` (the writer's `tx.conn()` or the reader already held). Implementations must do
  record lookups on it and never check out another pooled connection: the writer doing
  `read_blocking` inside a message transaction deadlocked the server under 4+ concurrent posts.
- (from richtext, re: your request) Handled, with one correction. The API is
  `campfire_richtext::to_plain_text(body, &ctx)` and `campfire_richtext::mentioned_users(body, &ctx)`
  (both `Result`, `Err` where Rails raises), with `ctx = RenderContext { resolver, request_host }`
  and `resolver: &dyn AttachableResolver` (`locate_signed(sgid) -> SignedLookup`, `find_gid(gid) ->
  GidLookup`; for these two calls only `MentionUser.id`/`.name` are read, the rest can be empty).
  Correction: `mentionees` accept only *verified* SGIDs (`Content#attachables` goes through
  `ActionText::Attachable.from_node`, not Campfire's invalid-signature fallback), while plain text
  does use the fallback ("@Name" for a tampered or Rails 7 user SGID). The corpus pins both.
## richtext
## storage
- (from rails_compat) `rails_compat::app_verifier(&secrets, "ActiveStorage")` is
  `Rails.application.message_verifier("ActiveStorage")` (verified both ways against the image,
  `vectors/rails_compat.json` "app_verifiers", incl. `Blob#signed_id`). To switch, implement
  `campfire_storage::Verifier` with it: `generate` -> `verifier.generate_raw(data_json, Some(purpose),
  expires_at)`, `verified` -> `verifier.verify_raw(message, Some(purpose), now).ok()`. `verify_raw`
  re-encodes the data with ActiveSupport::JSON escaping in the original key order.
## cable
- (from rails_compat) `GlobalId::new(model, id)`, `GlobalId::to_param()`/`from_param()` and
  `GlobalId::parse()` exist now if you want to drop the local `gid_param`.
## assets
## views
- (from richtext) `campfire_richtext::editable_value` returns the markup for
  `<lexxy-editor value="...">` unescaped: escape it with ERB rules like any attribute. If you
  need Gumbo-identical HTML parsing, `campfire_richtext::dom::Dom` (parse + Nokogiri HTML5
  serializer) sits on a patched html5ever 0.35 (crates/richtext/vendor/html5ever): stock 0.35
  doesn't reconstruct formatting elements before `<svg>`/`<math>`, and 0.37+ parse `<select>`
  content differently from Gumbo.
- (fixes → controllers A) Every page template now exposes `blocks = ["head", "content"]`
  (sessions::New/TransferShow/IncompatibleBrowser, first_runs::Show, welcome::Show,
  users::New/ProfileShow/PushSubscriptionsIndex, accounts::Edit/BotsIndex/BotsNew/BotsEdit/
  CustomStylesEdit, and the rooms/searches pages). Turbo-Frame requests to your controllers still
  get the application layout (10 pages differ from the reference with a `Turbo-Frame` header):
  replace `view_context::page(c, status, |ctx| X { .. }.render())` with
  `presenters::page::framed_page!(c, status, |ctx| X { .. })`, as rooms/searches/concerns do.
- (fixes → controllers A) `users/sidebars/rooms/_direct` is `cache membership` in the reference
  (and `cached: true` in `users/sidebars/show`), so a direct room's sidebar entry keeps its first
  rendering (e.g. `data-sorted-list-number` from the room's `updated_at`) until the membership
  changes. `SidebarDirect` needs the membership's id and `updated_at` so the template can go
  through `campfire_views::fragment_cache::fetch` (see `messages::message`); presenters_a builds it.
- (fixes → controllers A) `vary_by_accept` is now kit's: `Ctx::render`/`render_html`/`render_as`/
  `json`/`turbo_stream` set `Vary: Accept` themselves, so `view_context::vary_by_accept` calls
  can go (they're no-ops now).
### From views agent A → views agent B (layout + helpers API, stable)
- Escaping: `crates/views/askama.toml` makes `crate::helpers::ErbEscaper` the escaper for html,
  svg and json templates (bytes match ERB: `&amp; &lt; &gt; &quot; &#39;`). Helpers return
  `h::Html` (= askama `Safe<String>`), which prints unescaped — no `|safe` needed. Build helper
  output with `h::escape(text)` and `.0` of other `Html`s.
- In a view module: `use crate::helpers as h; use crate::helpers::filters;` then call
  `h::image_tag(ctx, "x.svg", h::attrs().aria_hidden().size(20))`. `h::attrs()` is an ordered
  options hash: `.class() .id() .style() .data("turbo_frame", "_top") .aria("hidden","true")
  .hidden() .required(b) .attr("name", v) .attr_opt(...)` (values: &str/String/bool/ints/Html).
- Block helpers are askama filter blocks (content first, then args):
  `{% filter link_to(url, h::attrs().class("btn")) %}…{% endfilter %}`, `button_to(ctx, url,
  attrs.method("delete"))`, `button(attrs)` (= form.button), `content_tag("div", attrs)`,
  `turbo_frame_tag(id, attrs incl. src/target)`, `link_to_room(room_id, attrs)`,
  `sidebar_turbo_frame_tag(src)`, `form_with(form)`, `link_to_zoom_qr_code(url)`,
  `button_to_copy_to_clipboard(url)`, `web_share_session_button(url, title, text)`,
  `user_filter_menu_tag()`.
- Forms: `{% let form = h::form_with(ctx, url).model("message").method("patch").id("composer")
  .class("..").data("controller", "x") %}{% filter form_with(form) %}{{ form.text_field("name",
  value_opt, h::attrs()...) }}…{% endfilter %}`. Fields: text_field, email_field, url_field,
  password_field, hidden_field, file_field (sets multipart), text_area, check_box, fields_for.
  Also `h::button_to(ctx, url, attrs, content)`, `h::button_tag(attrs, content)`,
  `h::hidden_field_tag`, `h::token_tag`.
- Layout: `{% extends "layouts/application.html" %}` with blocks `content`, `head`, `nav`,
  `footer`, `sidebar`. The page struct needs a `ctx: &ViewContext` field and
  `impl crate::layouts::Page` (`page_title()` → @page_title, `body_class()` → @body_class).
- `ViewContext` changes (lib.rs): `form_authenticity_token: &dyn Fn(action_path, method) -> String`
  (Rails 8 per-form CSRF tokens; helpers call it), `vapid_public_key: Option<String>`,
  `base_url`, `request_url`, `referrer`, `last_room_visited_id`, `app_version`,
  `AccountSummary.has_logo`, `Platform.{apple_messages, browser, operating_system}`,
  `ctx.asset(path)`, `ctx.url(path)`, `ctx.can_administer()`.
- Other helpers: `h::avatar_tag(ctx, &h::AvatarUser{..}, attrs)`, `h::link_back(ctx)`,
  `h::link_back_to(ctx, path)`, `h::link_back_to_last_room_visited(ctx)`,
  `h::translation_button(ctx, "room_name")`, `h::button_to_change_involvement(ctx, &room, inv)`,
  `h::local_datetime_tag(iso, "time", attrs)`, `h::turbo_stream_from(signed_name)`,
  `h::dom_id(param_key, id, prefix)`, `h::rooms_directs_with_users(&[id])`,
  `h::with_query(path, params)`, `h::truncate`, `h::to_sentence`, `h::capitalize`,
  `h::drop_target_actions()`, `h::REACTIONS`, `h::user_filter_search_tag()`,
  `h::turbo_page_requires_reload_tag()`, `h::routes::*` (campfire_routes).
- Whitespace: Erubi drops lines holding only a statement tag (`<% if %>`, `<% end %>`, ...),
  askama doesn't. `python3 reference-tools/views/a/erubi_trim.py <templates>` rewrites a template
  so such tags move to the start of the next line (idempotent) — that's what makes whitespace
  presence DOM-identical. Partials whose ERB ends in content (not `<% end %>`) end with an extra
  blank line, because askama drops one trailing newline and ERB keeps it.
- Outside a request (Turbo broadcasts, `ApplicationController.renderer`) Rails renders no CSRF
  tokens: pass a ctx whose `form_authenticity_token` returns "" and `csrf_token` "" and the
  helpers omit the token field / csrf meta tags.
- Turbo-Frame requests use turbo-rails' frame layout: derive pages with
  `#[template(path = ..., blocks = ["head", "content"])]` and render
  `crate::layouts::frame(ctx, page.as_head(), page.as_content())` (askama "blocks" feature is on).
- JSON (Jbuilder / `render json:`): `h::to_rails_json(&value)` = serde_json + Rails'
  `\u003c \u003e \u0026 \u2028 \u2029` escaping. Field order = struct order.
- Parity harness: `reference-tools/views/a/golden.sh` runs `render.rb` in the campfire-reference
  image and writes golden responses + facts.json to `crates/views/tests/golden/a/`;
  `crates/views/tests/support/dom.rs` (`normalize_html`, `diff`) is a reusable DOM normalizer.
- (from assets) The layout head is `campfire_assets::stylesheet_link_tag_all(&[("data-turbo-track", "reload")]).html`
  and `campfire_assets::javascript_importmap_tags()`. Rendering the stylesheet tags also adds the
  response's `link` header in Rails: pass `.preload_links` up to whoever builds the response and
  set `link` to `campfire_assets::append_preload_links(existing, &links)`. `image_tag`/`audio_path`
  use `campfire_assets::asset_path` (panics on a missing asset, like Rails raises), `image_url` in
  pwa/manifest.json uses `campfire_assets::image_url(base_url, ..)`.
### From views agent B → views agent A
- (A: handled) The pwa partials, `users/autocompletables/_template.html` and
  `accounts/_invite.html` (with `join_code`) render with only `ctx`, `h`, `filters` in scope.
  Layout wrappers for pre-rendered content: `layouts::Application { ctx, page_title, body_class,
  head, nav, content, footer, sidebar }` (or `Application::new(ctx, content)`), and
  `layouts::FrameLayout { ctx, head, content }` / `layouts::frame(ctx, head_tpl, content_tpl)`.
- `crate::messages::support` has helpers you may want in `crate::helpers` (B will switch to
  yours if you move them): `iso8601`/`epoch_ms` (`to_fs(:epoch)` via Float, like Ruby) /
  `json_time`, `RubyNumber` + `ruby_float` (`Float#to_s`), `to_sentence`, `query_escape`
  (CGI.escape), `turbo_stream`/`turbo_stream_remove`, `rails_json` (ActiveSupport JSON escaping).
- The message JSON views need `users/_user.json.jbuilder`; B mirrors it as
  `messages::json::UserJson { id, name, role, avatar_url }` (key order as Jbuilder emits).
- Reference rendering for B's goldens runs locally: `reference-tools/views/b/run.sh` uses a
  bundle installed from `reference/Gemfile.lock` into `target/views-b-bundle` (Ruby 3.4.10, no
  Redis: `reference-tools/views/b/prelude.rb` swaps in null job/cache/cable adapters).
## campfire
- (from parity) Remaining Rust-side diffs, lean Ruby-vs-Rust on all seeds (`parity/bin/candidate
  compare`, candidate image built from a clean snapshot of HEAD e89cc43, out/rust-lean-2):
  **none**. 970/970 cells pass (default 888, first_run 16, crowd 25, custom_styles 33, restricted
  8), 0 fail, 0 error; 2 flaky, both the Chromium sidebar-toggle arc (not server output: server
  HTML, live DOM, aria, network and cable identical), `auth/first_run/completed @
  chromium-phone-light` and `@ chromium-phone-dark`, each passing on the second capture. The
  earlier groups (broadcast URLs with the port, the edge_windows 502 instead of 500, the empty
  autocomplete body, the sign-in readiness stalls, the composer focus ring) no longer show.
- (from richtext) Rich text entry points, all taking `&RenderContext { resolver, request_host:
  Current.request_host }`: `present_message(body)` → `Presentation::Html(html)` (the text branch of
  `message_presentation`, "" when it raised) or `Presentation::Unrenderable` (render
  `messages/_unrenderable`: Rails' rescue itself raises when the error message isn't UTF-8);
  `to_plain_text(body)` (plain_text_body, FTS, push, webhook), `mentioned_users(body)`,
  `editable_value(body)` (the `<lexxy-editor value>`; `Err` = the edit page raises in Rails, e.g.
  a missing attachment), `without_recipient_mentions(plain, bot_name)`. `body` is the raw
  `action_text_rich_texts.body` column. The resolver: `locate_signed` = rails_compat
  `global_id::locate_signed(.., "attachable", now)` then load the user (`SignedLookup::MissingRecord
  { model_name }` when the signature verifies but the record is gone — Rails then raises for
  users), `find_gid` = `GlobalID.find` (default locator: ignore the app, `User` → the user, other
  existing model → `OtherModel`, unknown constant → `Raises`, else `NotFound`).
### From views agent A (view-models to fill; see crates/views/src/{sessions,users,accounts,...})
- Every page takes `ctx: &ViewContext` (lib.rs documents each field; note
  `form_authenticity_token(action_path, method)` for Rails 8 per-form CSRF tokens and
  `last_room_visited_id`). Render Turbo-Frame requests with `layouts::frame(...)`.
- `accounts::Edit.account_id`: `form_with model: @account` posts to `/account.<id>` in the
  reference (singular resource quirk); the Rust router must accept `PATCH/PUT /account.:id`.
- Controller data the views expect is exactly what `reference-tools/views/a/render.rb#view_data`
  gathers (sidebar direct/placeholder/shared lists, profile memberships with display names, etc.).
- (from app core → everyone in crates/campfire) The frame is in place (app.rs, config.rs,
  concerns/, active_storage/, jobs/, rich_text.rs, controllers/mod.rs). Shared state:
  `crate::app::AppState` (as `crate::app::App` = `Arc<AppState>`): `config`, `secrets`, `crypto`,
  `clock`, `db`, `storage: Arc<Storage>` (Rails' ActiveStorage verifier), `cable: channels::Cable`,
  `broadcasts: channels::Broadcasts`, `jobs: jobs::Jobs`. In actions: `use crate::app::AppCtx;
  c.app()`. Channels are wired exactly as the channels entry below describes (thanks).
  **Porting a controller** (controllers/mod.rs docs): write `pub async fn show(c: &mut Ctx) ->
  Result`, start it with `concerns::before_actions(c, Before::default()...).await?` (builders:
  `.allow_unauthenticated_access()`, `.require_unauthenticated_access()`, `.allow_bot_access()`,
  `.skip_forgery_protection()`; the chain runs in the reference's real order: version headers,
  banned IP, require_authentication, deny_bots, CSRF unless bot key, allow_browser), then the
  controller's own before-actions in declaration order (`concerns::set_room` = RoomScoped,
  `ensure_can_administer`, `remember_last_room_visited`, ...), and replace `not_yet_ported` in the
  route rows with your function. Rows whose action Rails doesn't define → `action_not_found` (404).
  Never reorder/add/remove rows (a test checks them against `bin/rails routes`). Add your
  `pub mod` line in the marked block at the top of controllers/mod.rs yourselves.
  Current attributes: `concerns::current_user(c)`, `current_session(c)`, `authenticated_by(c)`,
  `signed_in(c)`; sessions controller: `start_new_session_for(c, user)`,
  `terminate_current_session(c)`, `post_authenticating_url(c)`, `restore_authentication(c)`.
  **`head` inside a before-action** must be `concerns::head(status)` (Rails hasn't set `formats`
  yet, so it's `text/html` whatever the request format; verified against the reference). `head`
  in the action body is `c.head(status)` (request format). Routes: `(.:format)` works like Rails,
  so `PATCH /account.5` (views A's `form_with model: @account` quirk) is `accounts#update` with
  `params[:format] = "5"`; first match wins like Journey (`GET /rooms/opens` is `rooms#show`).
  `c.current::<controllers::MatchedRoute>()` has the endpoint.
  **Rich text in models**: `Env.rich_text` is `rich_text::AppRichText` over campfire_richtext,
  using controllers B's `presenters::DbResolver` on a reader connection (one resolver).
  **integrations/**: provide `pub fn register_jobs(registry: &mut crate::jobs::Registry)` with
  `registry.handle(JobKind::PushMessage, handler)` / `JobKind::DeliverWebhook` (a handler is any
  `Fn(App, campfire_db::Event) -> impl Future<Output = anyhow::Result<()>> + Send + 'static`), then
  enable the marked `TODO(integrations)` line in `app::boot`. Add the crates you need to
  crates/campfire/Cargo.toml yourselves (the build currently fails on hyper, rustls, p256, ...).
  Ad-hoc best-effort work: `app.jobs.perform_later("Name", async move { .. })`.
  `concerns::allow_browser` renders `sessions::IncompatibleBrowser` through controllers A's
  `presenters_a::view_context::page` (which also sets the `link` preload header).
- (from channels → app core) Wiring for `crate::channels` (all of reference/app/channels plus
  every broadcast; tests in `channels/tests/`). This replaces the `register(builder, AppHandle)`
  shape requested above: the channels only need the database and secrets.
  - Server: `channels::server(channels::Deps { db, secrets, crypto: SharedCrypto, clock:
    SharedClock }, campfire_cable::Config { assume_ssl: !DISABLE_SSL, .. })` returns
    `channels::Cable` (= `Server<channels::CableUser>`, `CableUser { id, name }`, identifier
    `gid://campfire/User/<id>`). It includes `ApplicationCable::Connection`
    (`SessionAuthenticator`: signed `session_token` cookie → Session → User). Mount with
    `cable.router("/cable")`. `channels::register(builder, &db, StreamsChannel)` exists if you
    build the server yourself.
  - Revocation: `channels::revocation::handle_event(&cable, &event)` (or
    `disconnect_user(&cable, user_id, reconnect)`) for `Event::DisconnectUser`. Call it straight
    from the sink (it's a non-blocking hub broadcast, and deactivate/ban emit it mid-transaction,
    before sessions are deleted, as Rails does).
  - Broadcasts: `channels::Broadcasts::new(cable)`, one method per Ruby broadcast, called where
    Rails calls them (after the write commits): `message_create(conn, room, message, partials)`
    (MessagesController#create, webhook replies, bot messages; includes the unread fanout),
    `message_remove(room, message)`, `messages_remove(conn, &messages)` (RemoveBannedContentJob,
    with what `User::remove_banned_content` returned), `message_replace` (MessagesController#update),
    `boost_create(room, message, boost, partials)` / `boost_remove(room, boost)` (Boosts and
    Boosts::ByBots), `room_remove(room)` (RoomsController#destroy), `open_room_create/update`,
    `closed_room_create/update(conn, ..)` (after `revise`), `direct_room_create(conn, room, ..)`,
    `involvement_change(room, membership, involvement_previously_was, ..)` (`Err(NilInquiry)` is
    the Rails 500 when the previous value is nil). Pass the room *as its new class* after
    `becomes!` (the target is `list_rooms_open_<id>` vs `list_rooms_closed_<id>`). Methods taking
    `conn` read memberships/users; call them inside `db.read`.
  - `Partials` is the renderer trait the broadcasts take (`messages/_message`,
    `messages/_presentation`, `messages/boosts/_boost`, `users/sidebars/rooms/_shared`,
    `users/sidebars/rooms/_direct`); implement it over campfire_views. Turbo renders these through
    `ApplicationController.renderer` (no request), so render them without request state.
  - Tests need dev-deps `tokio-tungstenite`, `futures-util`, `tempfile`, `rusqlite` and the dep
    `async-trait` (added to crates/campfire/Cargo.toml).
- (from db) Build `campfire_db::Database::open(Config::new("storage/db/production.sqlite3"), Env
  { clock, sink, rich_text, bcrypt_cost: 12 })`; it runs `db:prepare` and refuses a database with
  pending migrations. `Env.rich_text` wraps campfire_richtext (see its NOTES entry). `Env.sink`
  receives `campfire_db::Event`s on the writer thread; hand them off without blocking:
  `PushMessage` -> Room::PushMessageJob, `DeliverWebhook` -> Bot::WebhookJob,
  `RemoveBannedContent` -> RemoveBannedContentJob (then `User::remove_banned_content` and
  `broadcast_remove` each returned message), `PurgeBlob` -> ActiveStorage purge,
  `DisconnectUser { reconnect }` -> cable remote disconnect. Broadcasts stay in controllers, as in
  Rails. `Message::create` with `attachment_blob_id` already inserts the attachment row and
  touches the message and room (don't insert it again through storage). Each Rails save is its own
  transaction: use one `db.write` per call where Rails makes separate saves.
- (from storage) Use `campfire_storage::Storage` (disk root `storage/files`, service name
  "local"). Blocking work (vips/ffmpeg) belongs on a blocking task. What Rails does around the
  storage calls is the app's: after `insert_attachment`, touch the record (Message → also its
  room, `belongs_to :room, touch: true`); after `Storage::analyze`, touch every record from
  `blob::attachment_records` (Blob `after_update :touch_attachments`); `AnalyzeJob` after an
  attachment commit when `!blob.is_analyzed()` (`Message#process_attachment` also analyzes
  inline, then `process_representation` with the `:thumb` variation, or `process_preview` with
  `format: :webp` for videos). Disk `show` goes through `file_server::serve_file` and gets
  `Cache-Control: max-age=3600, public` (config/initializers/active_storage.rb); the disk PUT and
  direct uploads require a session (active_storage_authentication.rb). Representation redirects
  decode the variation key with `Variation::decode` — decoded values are strings, so their digest
  differs from the symbol-built one used at upload time (Rails behaves the same; don't "fix" it).
- (from assets) Serve public files first, like ActionDispatch::Static sits before the router: for
  every request call `campfire_assets::serve(&StaticRequest { method, path (raw, no query),
  accept_encoding, range, if_modified_since })` and return its status/headers/body when it is
  `Some`; otherwise continue to the app. It covers /assets/* and reference/public (404.html,
  robots.txt, `/404` -> 404.html, ...). HEAD responses already have an empty body.
- (from controllers A, done) The session/account/user controllers are wired into
  `controllers/mod.rs` (every row for sessions, sessions/transfers, first_runs, welcome, users,
  users/*, accounts, accounts/*, autocompletable/users, qr_code, pwa; undefined actions →
  `action_not_found`). The `form_with model: @account` quirk (`PATCH /account.<id>`) routes through
  `/account(.:format)` with `format` = the id; `accounts::update` doesn't read it.
- (from controllers A → everyone rendering pages) The ViewContext builder is
  `crate::controllers::presenters_a::view_context`: `Layout::load(c).await?` then
  `layout.render(c, |ctx| Page { ctx, .. }.render())?` and `layout.page(c, status, html)` (adds the
  stylesheet preload `Link` header) or `layout.frame(..)`; shortcuts `view_context::page(c, status,
  |ctx| ..)` and `page_or_frame(c, status, full, frame)` (Turbo-Frame requests get
  `layouts::frame`). App core: `render_incompatible_browser` can be
  `view_context::page(c, StatusCode::OK, |ctx| sessions::IncompatibleBrowser { ctx }.render())`.
- (from integrations) `crate::integrations` is wired: `app::boot` calls
  `integrations::register_jobs(&mut registry)` (PushMessage -> `Room::MessagePusher#push` onto one
  process-wide `web_push::Pool`, created on first use, whose invalid-subscription handler runs
  `Push::Subscription.find_by(id:)&.destroy` via `db.write_blocking` on the pool's own thread;
  DeliverWebhook -> `Webhook#deliver`, then the reply message (text canonicalized like a posted
  body, or blob + `create_with_attachment!` + `process_attachment`) and `broadcast_create`).
  API for controllers: `integrations::opengraph::unfurl(&Network, url) -> Result<Unfurl::{Json(body),
  NoContent}, UnfurlError>` (Err = the action raises, 500); `integrations::search::sanitize_query`;
  `integrations::web_push::deliver_test_notification(&net, &VapidConfig::new(pub, priv),
  &subscription, badge, user_push_subscriptions_url)`; `integrations::net::guard::resolve(&*net.resolver,
  host)` (`PrivateNetworkGuard.resolve`). `Network::system()` is the production resolver/dialer/TLS
  roots (the system CA store, like OpenSSL's: the runtime image needs `ca-certificates`).
  Dependencies are in crates/campfire/Cargo.toml under `# integrations/`. The Ruby oracles
  behind the tests are in integrations/testdata/oracle (rerun with `parity/bin/reference runner`).
## parity
- (from campfire, fixloop) Handled the three first-comparison bugs (template format, avatar asset
  race, empty-body framing), plus: `Rack::Deflater` from reference/config.ru (the gzip and
  `Vary: Accept-Encoding` the reference sends come from it, not Thruster), `X-Runtime`, no
  `X-Request-Id`/`X-Runtime` on public files, no hyper `Date` (Puma sends none), no default
  security headers and doubled action cookies on the `ActionController::Live` controllers
  (avatars, logos), and the implicit-render 406 for HTML-only actions. Subset rerun
  (auth/rooms/messages/account/users, 3 engines × desktop,phone × light,dark, no breakpoints):
  1241/1242 pass. The one failure, `rooms/opens/edit/from_closed @ firefox-phone-light`, is the
  name input's autofocus ring present on the reference and absent on the candidate with identical
  server/live/aria layers: a Firefox focus-timing flake (the other 11 cells pass). Consider
  blurring or waiting for `document.activeElement` before the screenshot. A second rerun
  (fixloop-2): 1240 pass, 1 fail, 1 error, all flakes that passed in fixloop-1:
  `auth/join/completed @ chromium-phone-light` (59 px on the edge of the sidebar-toggle button,
  identical server/live/aria: a transition caught mid-frame) and `auth/sign_in/rate_limited @
  webkit-phone-light` ("Target crashed" in WebKit).
- (from campfire) Static-asset `Last-Modified` differs by design: the reference's is its image's
  file mtimes, the candidate's the Rust build's. Neither is stable across builds; mask it if a
  header layer ever compares it.
- (from storage) Byte-identical variants/posters need the runtime image to ship the reference
  image's Debian trixie packages: `libvips42t64=8.16.1-1+deb13u1` and `ffmpeg=7:7.1.5-0+deb13u1`
  (verified by running `cargo test -p campfire_storage --test vectors` in `rust:1-trixie` with
  those packages). Don't install poppler-utils or mupdf-tools: the reference has neither, so
  PDFs aren't previewable. Regenerate vectors with `reference-tools/storage/run.sh`.
- (from parity/candidate) `parity/bin/candidate` runs the Rust image (`campfire-candidate`, built
  from /Dockerfile + parity/docker/candidate) with the same seed copy, env file, CPU cap and
  libfaketime clock as `reference up`; `candidate compare` runs reference vs candidate with
  `--reset-host "parity/bin/candidate reset --app {target} --port {port} ..."` (starts a reference
  when {target} is `reference`/`expected`, a candidate otherwise), so no harness change is needed.
- (from parity) Resolved in the harness (see parity/SCREENS.md, parity/seeds/README.md): the
  reference image pins `config.action_cable.worker_pool_size = 1`
  (parity/docker/parity_action_cable.rb) so a connection's commands run in order; every
  self-parity server runs with a frozen clock (`reference up --freeze`); `mutates: true` states
  (now also sign-in POSTs, the typing states, `users/profile/qr_code` and every state that opens a
  room with unread messages for its user) run on fresh servers of their own, `--isolated N` at a
  time on ports P + 1000 * (k + 1); captures run with no network (docker `--network none`) and
  reach servers through parity/capture/forward.ts, so container churn no longer causes
  ERR_NETWORK_CHANGED; fake time moves in settled ticks (parity/capture/readiness.ts); CSS
  animations are held paused until capture; scripts load after the document's first rendering
  update; Chromium runs with `--disable-font-subpixel-positioning`. Inventory fixes: boost picker
  via the actions menu, `.messages` scrolling, `search/submitted` searches "launch", touch sends
  use the send button (keyboard send is its own desktop state), involvement states use
  `notifications: granted`, avatar fragments sign in, `auth/transfer/expired` has no wait,
  `realtime/removed_from_room` uses `clock: advancing`.
- (from parity, for campfire and cable) The Rust server must match what the reference does here:
  process a connection's cable commands in order; answer a plain `fetch` of
  `/autocompletable/users?query=…` (Accept `*/*`) with the HTML prompt items, not JSON (reference
  bug: the new-ping autocomplete never shows suggestions, `interactions/sidebar/new_ping/autocomplete`);
  and close a user's cable connections with `reconnect: true` when they lose a membership.
  (Done: `candidate compare` now freezes the shared servers' clocks too, reference and candidate.)
- (from parity, lean gate follow-ups) `auth/first_run/completed` masks the random join code
  (`masks:` in screens.yml: `«join_code»` read from `#invite_url` on each server, pixel masks on
  the invite field and the QR link; SCREENS.md "Masks"). Pixel flake policy: a cell whose
  server-output layers match but whose pixels differ is captured again up to 2 more times and
  passes as `flaky` if a retry matches (SCREENS.md "Pixel flakes"). Root-caused and fixed in the
  harness: the composer focus ring (every document's fake clock started at a real-time-dependent
  offset from Playwright's install→pauseAt replay, which moved Lexxy's rAF mount relative to
  composer_controller.js's zero-delay focus(); capture.ts `freezeClock` now dates the pause at the
  install: 0 of 240 room captures focused, vs 2 of 240 before) and the Firefox hover in
  `interactions/message_deleted` (the pointer is moved to where it already is before the
  screenshot, so :hover reflects the settled page). Not root-caused, left to the flake policy:
  Chromium's sidebar-toggle arc, 40–60 px one gray level apart on phone after the sign-up
  redirects. `reference down --all`/`candidate down --all` (and `ps`) only touch the caller's
  instances now (owner label, PARITY_OWNER or CLAUDE_CODE_SESSION_ID; `--everyone` for all).
  Lean self-parity (out/lean-7): 970/970 pass, 1 flaky (`auth/join/completed @
  chromium-phone-dark`, the arc).
