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
