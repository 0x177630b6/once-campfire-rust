// Captures one state in one matrix cell against one server: screenshot, server HTML, live DOM and
// accessibility tree, all taken after real readiness. Fragment states capture the response only.
import fs from "node:fs"
import path from "node:path"
import YAML from "yaml"
import type { BrowserContext, BrowserContextOptions, Page, Response } from "playwright"
import { freezeAnimatedImages } from "./animated_images.ts"
import { contextOptions } from "./browsers.ts"
import type { BrowserPool } from "./browsers.ts"
import { PARITY_DIR, REPO_DIR } from "./config.ts"
import { cellId, interpolate, interpolateStep, isFragment, loadLabels } from "./inventory.ts"
import type { Job, Labels, State } from "./inventory.ts"
import { maskText, normalizeResponse, normalizeDocument } from "./normalize.ts"
import { DETERMINISM_SCRIPT, PageTracker, READINESS_SCRIPT, waitForReady } from "./readiness.ts"
import type { SessionCache } from "./session.ts"
import { runStep } from "./steps.ts"
import type { StepContext } from "./steps.ts"

export interface Target {
  name: string // "expected" | "actual" | ...
  url: string // the real server
  origin: string // what the browser sees (shared by all targets; see proxy.ts)
  proxy?: string // this target's forward proxy, set by run()
}

export interface CaptureEnv {
  pool: BrowserPool
  sessions: SessionCache
  outDir: string // run directory; artifacts go under <outDir>/<target.name>/
  time: string // ISO instant the browser clock is frozen at (the seed's clock.now)
  timeoutMs: number
  seedDir?: string
}

export interface CellMeta {
  state: string
  cell: string
  target: string
  url: string
  kind: "page" | "fragment"
  status?: number
  contentType?: string
  finalUrl?: string
  error?: string
  readiness?: unknown
  animations?: string[]
  focus?: string
  retriedAfter?: string
  cable?: Record<string, string[]>
  pageErrors: string[]
  console: string[]
  durationMs: number
  browserVersion?: string
}

export const ARTIFACTS = [".png", ".server.html", ".server.norm.html", ".live.norm.html", ".aria.yml", ".json"]

// Seeded opengraph embeds point at external images; the harness serves them (no network).
const EXTERNAL_FIXTURES: [string, string][] = [
  ["https://example.com/og/**", "reference/test/fixtures/files/moon.jpg"],
  ["https://pbs.twimg.com/profile_images/**", "reference/test/fixtures/files/moon.jpg"],
]

export function artifactBase(outDir: string, target: string, job: Job): string {
  return path.join(outDir, target, job.state.id, cellId(job.cell))
}

export async function captureCell(job: Job, target: Target, env: CaptureEnv): Promise<CellMeta> {
  const started = Date.now()
  const { state, cell } = job
  const base = artifactBase(env.outDir, target.name, job)
  fs.mkdirSync(path.dirname(base), { recursive: true })
  for (const ext of ARTIFACTS) fs.rmSync(base + ext, { force: true })

  const meta: CellMeta = {
    state: state.id, cell: cellId(cell), target: target.name, url: target.url, kind: isFragment(state) ? "fragment" : "page",
    pageErrors: [], console: [], durationMs: 0,
  }
  const { browser, release } = await env.pool.acquire(cell.engine)
  meta.browserVersion = browser.version()
  const contexts: BrowserContext[] = []
  const beforeClose: (() => Promise<void>)[] = []
  const newContext = async (user: string | undefined, labels: Labels) => {
    const options = contextOptions(cell, browser.version())
    if (state.user_agent) options.userAgent = userAgent(state.user_agent)
    if (state.headers) options.extraHTTPHeaders = interpolateStep(state.headers as any, labels, state.id) as any
    options.proxy = proxyOptions(target, cell.engine)
    if (user) options.storageState = await env.sessions.get(browser, target, options.proxy, user, labels)
    const context = await browser.newContext(options)
    contexts.push(context)
    context.setDefaultTimeout(env.timeoutMs)
    await isolateNetwork(context, target.origin)
    await freezeAnimatedImages(context, target.origin)
    return context
  }
  try {
    const labels = loadLabels(state.seed, env.seedDir)
    if (isFragment(state)) await captureFragment(state, target, env, base, meta, labels, await newContext(state.as, labels))
    else await capturePage(state, target, env, base, meta, labels, newContext, cell.viewport.touch, beforeClose)
  } catch (error: any) {
    meta.error = String(error?.message ?? error).split("\n").slice(0, 6).join("\n")
  } finally {
    await Promise.all(beforeClose.map((f) => f().catch(() => {})))
    await Promise.all(contexts.map((c) => c.close().catch(() => {})))
    await release()
    meta.durationMs = Date.now() - started
    fs.writeFileSync(base + ".json", JSON.stringify(meta, null, 2) + "\n")
  }
  return meta
}

async function capturePage(
  state: State, target: Target, env: CaptureEnv, base: string, meta: CellMeta, labels: Labels,
  newContext: (user: string | undefined, labels: Labels) => Promise<BrowserContext>, touch: boolean,
  beforeClose: (() => Promise<void>)[],
) {
  const actors: Record<string, string | undefined> = state.actors ?? { main: state.as }
  const captureActor = state.actors ? state.capture! : "main"
  const pages: Record<string, Page> = {}
  const trackers: Record<string, PageTracker> = {}
  let serverResponse: Response | null = null

  // Contexts are set up one actor at a time so multi-user states connect in a fixed order.
  for (const [actor, user] of Object.entries(actors)) {
    const context = await newContext(user, labels)
    await freezeClock(context, env.time)
    await context.addInitScript({ path: DETERMINISM_SCRIPT })
    await context.addInitScript({ path: READINESS_SCRIPT })
    const page = await context.newPage()
    page.on("dialog", (dialog) => (state.accept_dialogs ? dialog.accept() : dialog.dismiss()).catch(() => {}))
    pages[actor] = page
    trackers[actor] = new PageTracker(page)
    beforeClose.push(() => trackers[actor].abortHeld())
    const route = typeof state.path === "string" ? state.path : state.path[actor] ?? state.path[captureActor]
    const url = new URL(interpolate(route, labels, state.id), target.origin).href
    const response = await page.goto(url, { waitUntil: "load", timeout: env.timeoutMs })
    if (actor === captureActor) serverResponse = response
    await waitForReady([trackers[actor]], env.timeoutMs, Date.parse(env.time))
  }

  const capturePage = pages[captureActor]
  capturePage.on("response", (response) => {
    if (response.request().isNavigationRequest() && response.frame() === capturePage.mainFrame()) serverResponse = response
  })
  const stepContext: StepContext = { pages, trackers, defaultActor: captureActor, touch, baseUrl: target.origin, timeoutMs: env.timeoutMs, time: Date.parse(env.time) }
  for (const step of state.steps) {
    await runStep(interpolateStep(step, labels, state.id), stepContext)
    meta.readiness = await waitForReady(Object.values(trackers), env.timeoutMs, Date.parse(env.time))
  }
  if (!state.steps.length) meta.readiness = await waitForReady([trackers[captureActor]], env.timeoutMs, Date.parse(env.time))

  const response = serverResponse as Response | null
  meta.status = response?.status()
  meta.finalUrl = capturePage.url()
  const seedTime = Date.parse(env.time)
  if (response) {
    const html = await response.text().catch(() => "")
    fs.writeFileSync(base + ".server.html", html)
    fs.writeFileSync(base + ".server.norm.html", normalizeDocument(html, { seedTime }))
  }

  if (Object.keys(pages).length > 1) {
    // The last actor to act holds window focus; give it back to the captured page.
    await capturePage.bringToFront()
    await waitForReady([trackers[captureActor]], env.timeoutMs, Date.parse(env.time))
  }
  meta.animations = await capturePage.evaluate((at) => (window as any).__parity.pauseAnimations(at), stepContext.pauseAnimationsAt ?? null)
  meta.focus = await capturePage.evaluate(() => {
    const el = document.activeElement
    return `${document.hasFocus() ? "window focused" : "window blurred"}; active ${el ? el.tagName.toLowerCase() + (el.id ? "#" + el.id : "") + (el.className && typeof el.className === "string" ? "." + el.className.trim().split(/\s+/).join(".") : "") : "none"}`
  })
  const shot = await stableScreenshot(capturePage, env.timeoutMs)
  fs.writeFileSync(base + ".png", shot.png)
  if (!shot.stable) meta.console.push("screenshot never stabilized (two consecutive frames always differed)")
  const live = await capturePage.evaluate(() => document.body?.outerHTML ?? "")
  fs.writeFileSync(base + ".live.norm.html", normalizeDocument(`<!DOCTYPE html><html><head></head>${live}</html>`, { seedTime }))
  const aria = await capturePage.locator("body").ariaSnapshot({ timeout: env.timeoutMs })
  fs.writeFileSync(base + ".aria.yml", maskText(aria, { seedTime }) + "\n")

  meta.cable = Object.fromEntries(Object.entries(trackers).map(([actor, t]) => [actor, t.cableLog]))
  meta.pageErrors = Object.values(trackers).flatMap((t) => t.errors)
  meta.console.push(...Object.values(trackers).flatMap((t) => t.console))
  checkStatus(state, meta)
}

async function captureFragment(state: State, target: Target, env: CaptureEnv, base: string, meta: CellMeta, labels: Labels, context: BrowserContext) {
  const request = state.request ? (interpolateStep(state.request as any, labels, state.id) as any) : {}
  const url = new URL(interpolate(state.path as string, labels, state.id), target.origin).href
  const response = await context.request.fetch(url, {
    method: request.method ?? "GET",
    headers: request.headers,
    data: request.body,
    maxRedirects: 0,
    timeout: env.timeoutMs,
  })
  const body = await response.body()
  meta.status = response.status()
  meta.contentType = response.headers()["content-type"]
  fs.writeFileSync(base + ".server.html", body)
  const location = response.headers()["location"]
  const head = [`HTTP ${meta.status}`, `content-type: ${meta.contentType ?? ""}`, ...(location ? [`location: ${location}`] : [])]
  fs.writeFileSync(base + ".server.norm.html", `${head.join("\n")}\n\n${normalizeResponse(body, meta.contentType ?? "", { seedTime: Date.parse(env.time) })}`)
  checkStatus(state, meta)
}

// Date is frozen at the seed's instant (plans/rust-conversion.md, "Determinism"), and timers and
// requestAnimationFrame run on Playwright's fake clock, paused there. Zero-delay timeouts still
// fire at once; everything else waits for the readiness loop to advance the clock, which it only
// does with the network idle and every module and controller loaded (readiness.ts), and it puts
// Date back to the seed instant after each step. Pending callbacks then fire in due-time order
// however fast modules and responses arrived. With real timers, composer_controller.js's
// setTimeout(0) focus() raced Lexxy's requestAnimationFrame mount of the editor root, and the
// composer had its focus ring in some captures and not in others.
async function freezeClock(context: BrowserContext, time: string) {
  const instant = new Date(time)
  await context.clock.install({ time: new Date(instant.getTime() - 1000) })
  await context.clock.pauseAt(instant)
}

// What the Web Animations API can't pause (UA shadow DOM like Chromium's media-controls loading
// spinner, Chromium re-rasterizing a large downscaled image a few hundred ms after it appears in
// the lightbox) settles on its own: take frames until they have been identical for a full second.
const STABLE_FOR_MS = 1000
const FRAME_INTERVAL_MS = 250

async function stableScreenshot(page: Page, timeoutMs: number): Promise<{ png: Buffer; stable: boolean }> {
  const shoot = () => page.screenshot({ animations: "allow", caret: "hide", scale: "device", timeout: timeoutMs })
  const deadline = Date.now() + Math.min(timeoutMs, 15_000)
  let previous = await shoot()
  let since = Date.now()
  while (Date.now() < deadline) {
    await page.waitForTimeout(FRAME_INTERVAL_MS)
    const next = await shoot()
    if (!next.equals(previous)) {
      previous = next
      since = Date.now()
    } else if (Date.now() - since >= STABLE_FOR_MS) {
      return { png: next, stable: true }
    }
  }
  return { png: previous, stable: false }
}

function checkStatus(state: State, meta: CellMeta) {
  const expected = state.expect_status ?? 200
  if (meta.status !== expected) throw new Error(`expected HTTP ${expected}, got ${meta.status}`)
}

// Only the server under test is reachable; seeded external images are served from fixtures, and
// anything else external is refused, so no capture depends on the internet.
export function proxyOptions(target: Target, engine: string): BrowserContextOptions["proxy"] {
  if (!target.proxy) return undefined
  // Chromium never proxies loopback hosts unless told to; Firefox is told with a launch pref.
  return { server: target.proxy, bypass: engine === "chromium" ? "<-loopback>" : undefined }
}

async function isolateNetwork(context: BrowserContext, originUrl: string) {
  const origin = new URL(originUrl).origin
  await context.route((url) => url.origin !== origin && /^https?:$/.test(url.protocol), (route) => route.abort("blockedbyclient"))
  for (const [glob, file] of EXTERNAL_FIXTURES) {
    await context.route(glob, (route) => route.fulfill({ path: path.join(REPO_DIR, file) }))
  }
}

let userAgents: Record<string, string> | undefined
function userAgent(name: string): string {
  userAgents ??= YAML.parse(fs.readFileSync(path.join(PARITY_DIR, "seeds/user_agents.yml"), "utf8")) ?? {}
  return userAgents![name] ?? name
}
