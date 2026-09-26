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
