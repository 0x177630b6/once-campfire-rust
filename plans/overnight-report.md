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
- **PARITY MILESTONE** (9ae1c15, candidate built from a clean archive of e89cc43):

  | Gate | Result |
  |---|---|
  | Ruby vs Ruby, lean, all seeds | 970/970 pass, 1 flaky |
  | Rust vs Rails, lean, all seeds | 970/970 pass, 2 flaky, 0 allowlisted |

  The flaky cells are all the same issue: one gray level on the phone sidebar toggle's arc after
  the sign-up redirect. Their server output is identical. How the gate got here:
  - The random join code is masked on the first-run page (typed text placeholder, plus pixel masks
    over the invite field and QR code).
  - The composer focus race was root-caused and fixed: Playwright's per-document clock offset
    decided whether Lexxy's rAF mount ran before the composer's zero-delay focus.
  - `reference`/`candidate` `down --all` now only stop instances the caller owns.
- **HTTP optimization pass** (399e035, b9d269e, 2741830, 12197b5, 4dd3ff3, e89cc43, b93306d,
  786a74d). Each commit passed the full test suite and the header-shape sweep; the later commits
  also passed the HTTP-heavy lean parity subset.

  | Change | Effect |
  |---|---|
  | gzip on zlib-rs | room show 662 → 955 req/s |
  | Look up the fragment before building the view | room show 989 → 1,664; messages page 1,116 → 2,234 |
  | WAL checkpoints on their own thread | POST p99 at c=1: 12.5 → 1.7 ms |
  | Prepared statements cached everywhere | POST 4,257 → 4,693 |
  | Fragments shared, not copied; sidebar LIMIT inlined | messages page 2,233 → 2,666 |
  | Stylesheet tags built once per process | 6–10% less CPU per request |
  | Fat LTO + codegen-units=1 + jemalloc | 5–14% faster per route; idle RSS 11 → 45 MB; builds about 2× slower |

  Measured natively on 4 pinned cores; see the commits for conditions. What's left on hot pages is
  gzip level 6 (55–70% of CPU) and Rack's SHA-256 ETag, both required for parity with Rails.
- **Final lean gate** (candidate `campfire-candidate:head-5181aa4`, built from a clean `git archive`
  of HEAD plus the pinned `reference` submodule, with the cargo build stage uncached so the cached
  target dir couldn't stand in for the sources):

  | Gate | Result |
  |---|---|
  | Ruby vs Ruby, lean, all seeds (out/lean-final-self) | 970/970 pass, 0 flaky |
  | Rust vs Rails, lean, all seeds (out/rust-lean-final) | 970/970 pass, 1 flaky, 0 allowlisted |

  The flaky cell is the known sidebar-toggle arc (`auth/join/completed @ chromium-phone-light`).
  Before the gate ran, the benchmark's no-gzip runs turned up one real difference, and it was fixed
  first (fa1deb9). The front server's response cache never stored a streamed body of declared
  length: hyper stops polling after the last `Content-Length` byte, so the end of stream never
  arrived. As a result, avatars (`send_file`) requested with `Accept-Encoding: identity` always
  answered `X-Cache: miss`, where Thruster answers `hit`, and always went back to the app.
  Browsers always send gzip, so no parity cell could see it, and the header-shape sweep ignores
  `x-cache`.
- **Final benchmark** (`bench/results/final-20260927/report.md`). Production images, `5181aa4`
  against the reference, host networking, a quiet host, 5 interleaved reps. Rust vs Rails:

  | Measurement | Rust vs Rails |
  |---|---|
  | Pages, throughput at c=16 | 9–19× |
  | Message POSTs, throughput | 19× |
  | Cable deliveries/s | 22–26× |
  | Cold start | 10.6× faster |
  | Idle memory | 6.3× less |
  | App process at 1,000 cable clients | 2.6–3.0× less |

  Without gzip, Rust's pages serve 1.8–4.2× more requests again.

  One surprise: after the HTTP suite the Rust process holds 1.37 GB. That is the fragment store
  filled to its 50,000-entry cap by the ~100k messages the POST suite creates (Rails creates ~5.8k
  in the same time). It needs a byte bound.
