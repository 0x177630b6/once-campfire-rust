// Test-only, injected by the harness with addInitScript into every document of both servers.
//
// Math.random is replaced with a seeded PRNG so client-generated ids (Lexxy's link input ids,
// composer_controller.js client message ids) are the same on every run. One global sequence isn't
// enough: modules evaluate in network order, so which caller drew first varied. Each call site
// (its script, with asset digests stripped, line and column) gets its own sequence instead.
;(() => {
  const counters = new Map()
  const hash = (text) => {
    let h = 0x811c9dc5
    for (let i = 0; i < text.length; i++) h = Math.imul(h ^ text.charCodeAt(i), 0x01000193)
    return h >>> 0
  }
  const mulberry32 = (seed) => {
    let t = (seed + 0x6d2b79f5) | 0
    t = Math.imul(t ^ (t >>> 15), 1 | t)
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296
  }
  const callSite = () => {
    const lines = (new Error().stack || "").split("\n").filter((l) => l.trim() && !/^Error/.test(l))
    // [0] is callSite, [1] is random, [2] is the caller (V8, SpiderMonkey and JSC agree on this).
    return (lines[2] || "").replace(/-[0-9a-f]{7,64}(\.[a-z]+)/g, "$1").replace(/^\s*at\s+/, "").trim()
  }
  Math.random = function random() {
    const site = callSite()
    const n = (counters.get(site) || 0) + 1
    counters.set(site, n)
    return mulberry32(hash(`${site}#${n}`))
  }
})()

// Notification permission is reported as denied on every engine (headless engines disagree on
// the default), which is also what the notification-help states need (parity/screens.yml).
;(() => {
  if (typeof Notification === "undefined") return
  Object.defineProperty(Notification, "permission", { configurable: true, get: () => "denied" })
  Notification.requestPermission = () => Promise.resolve("denied")
})()
