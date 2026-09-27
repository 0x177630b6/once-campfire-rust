# Overnight report

Running log; the final summary gets written at the end.

## Log

- **Thruster replaced** (6b0d797). `campfire server` now does TLS with automatic ACME (tested
  against Pebble), HTTP/2, Thruster's response cache and zstd/gzip, and reads every Thruster env
  var. Certificates are stored in `/rails/storage/thruster`, so existing installs keep theirs.
  Thruster 0.1.23 serves a certificate the new binary wrote. HTTP-01 is only unit-tested.
- **Header-shape parity** (a0f5914). `reference-tools/http_shape/sweep.py` finds 0 differences
  across about 70 requests (pages, streams, JSON, assets, avatars, blobs, 304s, HEAD, errors).
- **Performance attribution** (96d10ea, `plans/perf-attribution.md`). gzip is 62% of room-show
  CPU. Cached message fragments were still fully rebuilt before the cache lookup. Cable zero-filled
  a 128 KiB buffer on every read. The POST p99 stall is SQLite's WAL checkpoint fsyncing on the
  writer thread.

## Incidents

- While fetching the mimalloc source, the profiling agent sent the user's email address in the
  User-Agent header of one crates.io API request. It didn't happen again, and nothing else left
  the machine.

## Log, continued

- **Lean gate committed** (4797f84). Ruby vs Ruby: 963 of 970 cells identical; crowd, custom_styles
  and restricted are fully clean. What remains is a random join code on first run (4 cells) plus 3
  rare pixel flakes.
- **Harness determinism fixes that landed with it:**
  - one Action Cable command worker in the reference;
  - frozen clocks for every server;
  - a capture container with no network of its own;
  - fixed-tick readiness;
  - scripts delivered in request order;
  - pinned Chromium font and Skia flags.
- **Reference bug found:** new-ping autocomplete never shows suggestions, because a plain fetch
  requests JSON and gets HTML back.
- **Rust vs Rails, first lean run:**

  | Seed | Pass | Of |
  |---|---|---|
  | crowd | 25 | 25 |
  | custom_styles | 33 | 33 |
  | restricted | 8 | 8 |
  | first_run | 12 | 16 (the join-code cells) |
  | default | 822 | 888 (54 fail, 12 error) |

- **Coordinator decisions:**
  - Mask the random join code, since seeding Ruby's RNG can't make Rust match.
  - A pixel-only difference whose server output is identical gets up to 2 re-captures; if one
    matches, the cell passes but is flagged flaky. Server-output differences are never retried.
