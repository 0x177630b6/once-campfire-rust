# once-campfire-rust

Campfire in Rust. It started as a port that had to be indistinguishable from the Rails app in
`reference/` (a submodule pinned to the SHA it was matched against), and that parity is done: see
`README.md` and `plans/rust-conversion.md`. The port may now diverge from Rails where that makes it
faster or better.

- The Rails app is still the reference for anything not deliberately changed: when in doubt about
  existing behavior, read the Ruby.
- Stay compatible with existing installs unless told otherwise: the SQLite schema, the storage
  layout, and signed/encrypted cookies (so people stay signed in across an upgrade).
- List every deliberate divergence under "Known differences" in `README.md`, and update the tests
  and parity masks it affects.
- Port-owned frontend changes go in `crates/assets/overrides/`, which shadows the reference's assets
  by logical path. Don't edit `reference/`.

## Layout and ownership

| Path | Package | What |
|---|---|---|
| `crates/rails_compat` | `rails_compat` | Rails signing/encryption/serialization contracts, verified by `vectors/` |
| `crates/kit` | `campfire_kit` | Axum adapter, `Ctx`, params, cookies, session, forgery protection, flash, formats, responses, gzip |
| `crates/routes` | `campfire_routes` | Path helpers mirroring `config/routes.rb` |
| `crates/db` | `campfire_db` | rusqlite over the existing schema, models, queries, fixtures loader |
| `crates/richtext` | `campfire_richtext` | Action Text content pipeline: sanitize, attachments, autolink, plain text |
| `crates/storage` | `campfire_storage` | Active Storage-compatible blobs, disk service, variants (libvips), previews (ffmpeg) |
| `crates/cable` | `campfire_cable` | Action Cable protocol server and in-process pub/sub |
| `crates/assets` | `campfire_assets` | Propshaft-compatible digesting, importmap, vendored JS/CSS, port-owned overrides |
| `crates/views` | `campfire_views` | Askama templates (one per ERB file, same relative path) and view helpers |
| `crates/campfire` | `campfire` (bin) | Controllers, router wiring, channels, jobs, integrations |
| `parity/` | — | Playwright parity harness, screen inventory, reference Docker setup |
| `reference-tools/` | — | Ruby scripts run inside the reference container to produce `vectors/` |

Each agent owns the paths it was assigned. Don't edit another owner's crate. If you need
something from it, write the request in `NOTES.md` under that crate's heading and code against
the interface you need behind a local trait or adapter until it lands.

## Working rules

- Rust comes from mise if it isn't on the PATH: `mise exec rust@1.98.1 -- cargo ...` (the version
  in `Dockerfile`). The `reference/` submodule must be checked out for `crates/assets` to build.
- Build with your own target dir so parallel agents don't block on the cargo lock:
  `CARGO_TARGET_DIR=target/<your-crate> cargo test -p <package>`.
- Put shared dependency versions in the root `[workspace.dependencies]`, and reference them with
  `foo.workspace = true`.
- Don't commit. The coordinator commits after each wave.
- When matching existing behavior, read the reference's source. When it depends on Rails or gem
  internals, read the gem source inside the reference image
  (`docker run --rm campfire-reference bundle show <gem>`), not docs or memory.
- Write code that reads like the surrounding code: small, clearly named functions, and comments
  only where the behavior is non-obvious. Cite the reference file (`reference/app/...`) when
  matching Rails, and say why when deliberately diverging from it.
- Tests live beside the code. Golden-vector tests read `vectors/*.json`.

## Reference container

`parity/` builds the reference image as `campfire-reference` from `reference/Dockerfile` and runs it
in production mode with a fixed `SECRET_KEY_BASE` (see `parity/.env.reference`) so that golden
vectors, seeds and screenshots are reproducible.
