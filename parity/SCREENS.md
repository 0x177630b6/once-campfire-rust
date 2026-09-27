# Screen inventory format (`parity/screens.yml`)

The inventory lists *states*. The capture engine (`parity/capture`) renders every state against
both servers across the support matrix, and compares the results.

```yaml
- id: rooms/show/busy                 # unique, path-like; used for artifact names
  as: david                           # fixture user label to sign in as; omit for signed out
  path: /rooms/{{rooms.designers}}    # {{table.label}} interpolates fixture IDs
  seed: default                       # named seed variant from parity/seeds/ (default: default)
  matrix:                             # optional narrowing of the support matrix
    viewports: [desktop, phone]       # desktop 1440x900, laptop 1280x800, tablet 834x1194, phone 390x844
    schemes: [light, dark]
    engines: [chromium, firefox, webkit]
  steps:                              # optional interactions after the page is ready
    - click: ".message__actions-btn"
    - fill: { selector: "#message_body", text: "Hello" }
    - hover: ".boost"
    - press: "Enter"
    - wait_for: ".autocomplete__list"
    - pause_animations_at: 500        # ms; pauses all document.getAnimations() at this time
  actors:                             # multi-user states; steps may then carry `actor:`
    a: david
    b: kevin
  capture: b                          # which actor's page is captured (multi-user only)
  covers: [rooms/show, rooms/show/_composer]   # templates this state is meant to exercise
```

Steps run in order. Any step may take `actor: <name>` in multi-user states. The engine waits for
readiness (Stimulus controllers connected, cable subscriptions confirmed, fonts, images and network
idle) after navigation and after every step.

Fake time moves only in fixed ticks (50ms of the browser's paused clock), and only while the page
has settled in real time: network idle, nothing left that real time resolves (cable confirmations,
images, zero-delay timeouts), and the DOM unchanged since the previous tick. A page is ready after
8 ticks in a row that changed nothing, and a `wait_for` ticks until its element shows. So the fake
time a capture spends depends only on the app's own timers, never on the machine's speed
(`parity/capture/readiness.ts`).

`mutates: true` states (see the top of `screens.yml` for what counts) never share a server: with a
reset hook (`--reset`, which `parity/bin/compare --self-parity` sets up), each of their captures
gets freshly started servers of its own, `--isolated N` (default 3) at a time, alongside the other
states. Slot k of a server on port P listens on P + 1000 × (k + 1), and the reset command is run
with that `{port}`.

## Matrix

`parity/bin/compare` (and `capture`) take `--matrix lean|full`; lean is the default
(plans/rust-conversion.md, decision 4). The frontend is byte-identical and the browsers are pinned,
so a port can only change pixels by sending different bytes: the gate is server output, and pixels
back it up.

Every page capture records five text layers besides the screenshot, and every one is compared:

| Layer | File | What |
|---|---|---|
| server | `.server.norm.html` | the main document's response, normalized (typed placeholders for tokens and times) |
| live | `.live.norm.html` | `body.outerHTML` at capture time, normalized |
| aria | `.aria.yml` | the accessibility tree |
| network | `.network.txt` | every response from the server to the page (and navigation): method, path, status, header shape (names, plus the values of `content-type`, `location`, `cache-control`, `content-disposition`, `vary`, and each cookie's name and attributes), and the sha256 of the normalized body (media ranges: the total size). Sorted, since requests run in parallel. |
| cable | `.cable.txt` | every Action Cable frame except pings, per subscription in order, payloads normalized |

The **lean** matrix (`parity/capture/inventory.ts`, `LEAN_*`):

- every state on Chromium, desktop and phone, light and dark (a state narrowed to other viewports
  keeps its first one);
- the smoke states (`realtime/**`, `auth/sign_in`, `rooms/show/designers`,
  `interactions/composer/with_text`, `interactions/lightbox`,
  `interactions/mention_autocomplete/results`) also on Firefox and WebKit, desktop and phone, light;
- the breakpoint sweep on Chromium;
- fragments once, as always.

The **full** matrix is every engine × every viewport × both schemes plus the sweep on every
engine: keep it for release checks. `--self-parity` runs once with the lean matrix (twice, plus a
run-1-vs-run-2 comparison, with the full one), all seeds in parallel.
