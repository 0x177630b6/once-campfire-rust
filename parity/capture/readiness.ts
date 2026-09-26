// Waits for real readiness signals (plans/rust-conversion.md, "Determinism"): every data-controller
// element has connected controller instances, every Action Cable subscription in the consumer is
// confirmed by the server, fonts are loaded, images and posters decoded, Turbo idle, the network
// quiet, and the DOM stable for a short window.
import path from "node:path"
import { fileURLToPath } from "node:url"
import type { Page, Request, Route, WebSocket } from "playwright"

export const READINESS_SCRIPT = path.join(path.dirname(fileURLToPath(import.meta.url)), "readiness.js")
export const DETERMINISM_SCRIPT = path.join(path.dirname(fileURLToPath(import.meta.url)), "determinism.js")

const QUIET_MS = 250
const POLL_MS = 40
// Fake time advanced per poll once the network is idle (see freezeClock in capture.ts).
const ADVANCE_MS = 50

// Excluded from "network idle": long-lived by design, and covered by their own signals.
const LONG_LIVED = new Set(["websocket", "eventsource", "media"])

export class PageTracker {
  readonly page: Page
  inflight = new Set<Request>()
  held = new Set<Request>()
  heldRoutes: Route[] = []
  lastNetworkActivity = Date.now()
  socket?: WebSocket
  confirmed = new Set<string>()
  rejected = new Set<string>()
  // subscribe commands sent minus confirmations/rejections received, per identifier, since the
  // last welcome: a resubscription (turbo-cable-stream-source reconnected by a frame load) needs
  // its own confirmation, not the previous one's.
  outstanding = new Map<string, number>()
  errors: string[] = []
  networkErrors: string[] = []
  cableLog: string[] = [] // subscribe/unsubscribe/confirm/reject frames, for diagnosing readiness
  console: string[] = []

  constructor(page: Page) {
    this.page = page
    page.on("request", (request) => {
      if (LONG_LIVED.has(request.resourceType()) || this.held.has(request)) return
      this.inflight.add(request)
      this.lastNetworkActivity = Date.now()
    })
    const settle = (request: Request) => {
      if (this.inflight.delete(request)) this.lastNetworkActivity = Date.now()
    }
    page.on("requestfinished", settle)
    page.on("requestfailed", (request) => {
      settle(request)
      // Requests the harness refused (external hosts) or cut (held requests) are expected; any
      // other failure (Chromium's ERR_NETWORK_CHANGED when Docker adds an interface on the host)
      // leaves a broken image on screen, so the capture fails and is retried.
      const error = request.failure()?.errorText ?? ""
      if (!this.held.has(request) && !/BLOCKED_BY_CLIENT|blocked|NS_ERROR_ABORT|cancelled|aborted/i.test(error)) {
        const origin = new URL(page.url()).origin
        if (request.url().startsWith(origin)) this.networkErrors.push(`${error} ${request.url()}`)
      }
    })
    page.on("websocket", (socket) => this.watchSocket(socket))
    page.on("pageerror", (error) => this.errors.push(String(error?.stack ?? error)))
    page.on("console", (message) => {
      if (message.type() === "error" || message.type() === "warning") this.console.push(`${message.type()}: ${message.text()}`)
    })
    page.on("response", (response) => {
      if (response.status() >= 400) this.console.push(`http: ${response.status()} ${response.request().method()} ${response.url()}`)
    })
    page.on("framenavigated", (frame) => {
      if (frame === page.mainFrame()) this.resetCable()
    })
  }

  private resetCable() {
    this.socket = undefined
    this.confirmed.clear()
    this.rejected.clear()
    this.outstanding.clear()
  }

  // Action Cable keeps no confirmed flag on a Subscription, so confirmation is read off the wire.
  private watchSocket(socket: WebSocket) {
    this.socket = socket
    this.confirmed.clear()
    this.rejected.clear()
    const short = (identifier: string) => {
      try {
        const id = JSON.parse(identifier)
        return `${id.channel}${id.room_id ? `:${id.room_id}` : ""}${id.signed_stream_name ? `:${id.signed_stream_name.slice(0, 12)}` : ""}`
      } catch {
        return identifier
      }
    }
    socket.on("framesent", ({ payload }) => {
      if (typeof payload !== "string") return
      try {
        const message = JSON.parse(payload)
        if (message.command) this.cableLog.push(`> ${message.command} ${short(message.identifier)}`)
        if (message.command === "subscribe" && socket === this.socket) {
          this.outstanding.set(message.identifier, (this.outstanding.get(message.identifier) ?? 0) + 1)
        }
      } catch {}
    })
    socket.on("framereceived", ({ payload }) => {
      if (socket !== this.socket || typeof payload !== "string") return
      let message: any
      try {
        message = JSON.parse(payload)
      } catch {
        return
      }
      if (message.type && message.type !== "ping") this.cableLog.push(`< ${message.type} ${message.identifier ? short(message.identifier) : ""}`)
      if (message.type === "welcome") {
        this.confirmed.clear()
        this.outstanding.clear()
      } else if (message.type === "confirm_subscription" || message.type === "reject_subscription") {
        ;(message.type === "confirm_subscription" ? this.confirmed : this.rejected).add(message.identifier)
        this.outstanding.set(message.identifier, (this.outstanding.get(message.identifier) ?? 0) - 1)
      }
    })
    socket.on("close", () => {
      if (socket === this.socket) this.confirmed.clear()
    })
  }

  // A request deliberately left pending (hold_requests) isn't something to wait for.
  hold(request: Request, route: Route) {
    this.held.add(request)
    this.heldRoutes.push(route)
    if (this.inflight.delete(request)) this.lastNetworkActivity = Date.now()
  }

  // Held requests must never reach the server: closing a context with a route still pending can
  // let the request through (an upload from interactions/composer/upload_in_progress created a
  // message that later captures of the room showed). Abort them before the context closes.
  async abortHeld() {
    const routes = this.heldRoutes.splice(0)
    await Promise.all(routes.map((r) => r.abort("aborted").catch(() => {})))
  }

  networkIdleFor(): number {
    return this.inflight.size ? 0 : Date.now() - this.lastNetworkActivity
  }
}

export interface ReadinessResult {
  elapsedMs: number
  controllers: number
  subscriptions: string[]
  unknownControllers: string[]
}

export async function waitForReady(trackers: PageTracker[], timeoutMs: number, time: number): Promise<ReadinessResult[]> {
  return Promise.all(trackers.map((t) => waitForPage(t, timeoutMs, time)))
}

async function waitForPage(tracker: PageTracker, timeoutMs: number, time: number): Promise<ReadinessResult> {
  const { page } = tracker
  const started = Date.now()
  if (tracker.networkErrors.length) throw new Error(`network failure: ${tracker.networkErrors[0]}`)
  let lastFingerprint = ""
  let stableSince = Date.now()
  let reasons: string[] = []
  let snapshot: any
  while (true) {
    try {
      snapshot = await page.evaluate((socketSeen) => (window as any).__parity?.snapshot(socketSeen) ?? null, !!tracker.socket)
    } catch (error) {
      // The page navigated mid-evaluation; poll again on the new document.
      snapshot = null
      reasons = [`evaluate: ${String(error).split("\n")[0]}`]
    }
    if (snapshot) {
      reasons = unreadyReasons(snapshot, tracker)
      if (snapshot.fingerprint !== lastFingerprint) {
        lastFingerprint = snapshot.fingerprint
        stableSince = Date.now()
      }
      const idle = tracker.networkIdleFor()
      if (idle < QUIET_MS) reasons.push(`network: ${tracker.inflight.size} in flight`)
      // Time only moves once every module has loaded and every controller has connected, so the
      // timers they schedule while connecting all exist before any of them fires.
      else if (!loading(snapshot)) await advanceClock(page, time)
      if (Date.now() - stableSince < QUIET_MS) reasons.push("dom changing")
      if (tracker.networkErrors.length) throw new Error(`network failure: ${tracker.networkErrors[0]}`)
      if (!reasons.length) {
        return {
          elapsedMs: Date.now() - started,
          controllers: snapshot.stimulus.connected ?? 0,
          subscriptions: snapshot.cable.identifiers ?? [],
          unknownControllers: snapshot.stimulus.unknown ?? [],
        }
      }
    } else if (!reasons.length) {
      reasons = ["readiness script not installed"]
    }
    if (Date.now() - started > timeoutMs) {
      const inflight = [...tracker.inflight].map((r) => `${r.method()} ${r.url()}`)
      throw new Error(`not ready after ${timeoutMs}ms at ${page.url()}: ${reasons.join("; ")}${inflight.length ? ` [${inflight.join(", ")}]` : ""}`)
    }
    await new Promise((resolve) => setTimeout(resolve, POLL_MS))
  }
}

// Fire the timers due in the next ADVANCE_MS, then put Date back to the frozen instant.
export async function advanceClock(page: Page, time: number) {
  try {
    await page.clock.runFor(ADVANCE_MS)
    await page.clock.setSystemTime(time)
  } catch {
    // navigated mid-advance; the next poll sees the new document
  }
}

function loading(s: any): boolean {
  return s.readyState !== "complete" || s.undefinedElements.length > 0 || (!s.stimulus.present && s.stimulus.expectsApp) ||
    s.stimulus.unregistered?.length > 0 || s.stimulus.missing?.length > 0
}

function unreadyReasons(s: any, tracker: PageTracker): string[] {
  const reasons: string[] = []
  if (s.readyState !== "complete") reasons.push(`document ${s.readyState}`)
  if (s.fonts !== "loaded") reasons.push(`fonts ${s.fonts}`)
  if (!s.stimulus.present && s.stimulus.expectsApp) reasons.push("Stimulus application not started")
  if (s.stimulus.unregistered?.length) reasons.push(`controllers not registered: ${s.stimulus.unregistered.join(", ")}`)
  if (s.stimulus.missing?.length) reasons.push(`controllers not connected: ${s.stimulus.missing.slice(0, 5).join(", ")}`)
  const cable = s.cable
  if (cable.unsubscribedSources) reasons.push(`${cable.unsubscribedSources} turbo-cable-stream-source without subscription`)
  if (cable.streamSources && !tracker.socket) reasons.push("stream sources but no socket yet")
  if (tracker.socket) {
    if (cable.error) reasons.push(`cable: ${cable.error}`)
    else if (!cable.open) reasons.push("cable connection not open")
    for (const identifier of cable.identifiers ?? []) {
      if ((!tracker.confirmed.has(identifier) && !tracker.rejected.has(identifier)) || (tracker.outstanding.get(identifier) ?? 0) > 0) {
        reasons.push(`subscription unconfirmed: ${identifier}`)
      }
    }
  }
  if (s.media.pendingImages.length) reasons.push(`images: ${s.media.pendingImages.slice(0, 3).join(", ")}`)
  if (s.media.pendingVideos.length) reasons.push(`video: ${s.media.pendingVideos.slice(0, 3).join(", ")}`)
  if (s.turboBusy.length) reasons.push(`turbo busy: ${s.turboBusy.join(", ")}`)
  if (s.undefinedElements.length) reasons.push(`custom elements not defined: ${s.undefinedElements.join(", ")}`)
  return reasons
}
