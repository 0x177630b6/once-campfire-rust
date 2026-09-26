# once-campfire-rust

A Rust port of Campfire that must be indistinguishable from the Rails app in `reference/` (a
submodule pinned to the SHA we're matching). Read `plans/rust-conversion.md` first. The Rails app is
the oracle: when in doubt, read the Ruby, and never "improve" behavior.

## Layout and ownership

| Path | Package | What |
|---|---|---|
| `crates/rails_compat` | `rails_compat` | Rails signing/encryption/serialization contracts, verified by `vectors/` |
| `crates/kit` | `campfire_kit` | Axum adapter, `Ctx`, params, cookies, session, CSRF, flash, formats, responses |
| `crates/routes` | `campfire_routes` | Path helpers mirroring `config/routes.rb` |
| `crates/db` | `campfire_db` | rusqlite over the existing schema, models, queries, fixtures loader |
| `crates/richtext` | `campfire_richtext` | Action Text content pipeline: sanitize, attachments, autolink, plain text |
| `crates/storage` | `campfire_storage` | Active Storage-compatible blobs, disk service, variants (libvips), previews (ffmpeg) |
| `crates/cable` | `campfire_cable` | Action Cable protocol server and in-process pub/sub |
| `crates/assets` | `campfire_assets` | Propshaft-compatible digesting, importmap, vendored JS/CSS |
| `crates/views` | `campfire_views` | Askama templates (one per ERB file, same relative path) and view helpers |
| `crates/campfire` | `campfire` (bin) | Controllers, router wiring, channels, jobs, integrations |
| `parity/` | — | Playwright parity harness, screen inventory, reference Docker setup |
| `reference-tools/` | — | Ruby scripts run inside the reference container to produce `vectors/` |

Each agent owns the paths it was assigned. Don't edit another owner's crate. If you need
something from it, write the request in `NOTES.md` under that crate's heading and code against
the interface you need behind a local trait or adapter until it lands.

## Working rules

- Build with your own target dir so parallel agents don't block on the cargo lock:
  `CARGO_TARGET_DIR=target/<your-crate> cargo test -p <package>`.
- Put shared dependency versions in the root `[workspace.dependencies]`, and reference them with
  `foo.workspace = true`.
- Don't commit. The coordinator commits after each wave.
- Match the reference by reading its source. When behavior depends on Rails or gem internals, read
  the gem source inside the reference image (`docker run --rm campfire-reference bundle show <gem>`),
  not docs or memory.
- Write code that reads like the surrounding code: small, clearly named functions, and comments
  only where the Rails behavior being matched is non-obvious. Cite the reference file
  (`reference/app/...`) in those comments.
- Tests live beside the code. Golden-vector tests read `vectors/*.json`.

## Reference container

`parity/` builds the reference image as `campfire-reference` from `reference/Dockerfile` and runs it
in production mode with a fixed `SECRET_KEY_BASE` (see `parity/.env.reference`) so that golden
vectors, seeds and screenshots are reproducible.
