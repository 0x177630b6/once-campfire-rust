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
- **Cable optimization pass** (4e77f67, 95f2d93, 95e0a3e, a6616a2, a119dab). Measured at 1,000
  clients, protocol output byte-identical:

  | | Before | After |
  |---|---|---|
  | Server CPU per delivery | 62.6 µs | 13.4 µs (4.7×) |
  | Sustained messages per second | 57 | 198 (3.5×, now bound by the load generator) |
  | Delivery latency p50 / p99 | 22.8 / 46.0 ms | 11.0 / 17.3 ms |
  | Memory under saturation | 208 MB | 112 MB |

  How: a 4 KiB read buffer read in its own task, each frame encoded once and shared across
  subscribers, batched writes, and connections reading straight from the hub's ring buffers.
- **Rust vs Rails app fixes** (3b70734, 69bda70, eda40f2, 03218c3). A rerun of every affected
  state passed 114 of 114 cells.
  - Broadcast URLs drop the request's port, as Rails' `ApplicationController.renderer` does.
  - A panicking action now returns the normal 500 page.
  - Empty autocomplete keeps ERB's trailing newline, which gives it Rack's ETag.
  - The front server writes `Date` from the real clock. A frozen `Date` made Chrome treat
    preloaded assets as stale and stall the sign-in pages.
  - What remains is the composer focus-ring timing flake, which the harness agent is fixing.
