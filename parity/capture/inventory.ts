// Loads parity/screens.yml (format in parity/SCREENS.md) and expands it over the support matrix.
import fs from "node:fs"
import path from "node:path"
import YAML from "yaml"
import { ENGINES, SCHEMES, VIEWPORTS, VIEWPORT_NAMES, SEED_DIR, breakpointViewport } from "./config.ts"
import type { Engine, Scheme, Viewport } from "./config.ts"

export type Step =
  | { click: string; actor?: string }
  | { fill: { selector: string; text: string }; actor?: string }
  | { hover: string; actor?: string }
  | { press: string | { selector?: string; key: string }; actor?: string }
  | { wait_for: string; actor?: string }
  | { pause_animations_at: number; actor?: string }
  | { goto: string; actor?: string }
  | { type: { selector: string; text: string }; actor?: string }
  | { upload: { selector: string; files: string[] }; actor?: string }
  | { scroll: { selector: string; to: "top" | "bottom" }; actor?: string }
  | { hold_requests: string; actor?: string }

export interface State {
  id: string
  as?: string
  path: string | Record<string, string>
  seed: string
  matrix?: { viewports?: string[]; schemes?: Scheme[]; engines?: Engine[] }
  steps: Step[]
  actors?: Record<string, string>
  capture?: string
  covers?: string[]
  // Extensions beyond SCREENS.md (documented at the top of parity/screens.yml):
  kind?: "page" | "fragment" // fragment: compare the response only (status, type, normalized body)
  request?: { method?: string; body?: string; headers?: Record<string, string> } // fragments only
  headers?: Record<string, string> // extra headers on every request of the state
  user_agent?: string // key of parity/seeds/user_agents.yml, or a literal UA string
  accept_dialogs?: boolean // accept window.confirm (turbo_confirm) instead of dismissing
  expect_status?: number // HTTP status of the main document (default 200)
  mutates?: boolean // changes the database: runs serially, each capture on a freshly reset server
  breakpoints?: boolean // include in the breakpoint sweep (besides DEFAULT_BREAKPOINT_STATES)
}

export interface Cell {
  engine: Engine
  viewport: Viewport
  scheme: Scheme
}

export interface Job {
  state: State
  cell: Cell
}

export function cellId(cell: Cell): string {
  return `${cell.engine}-${cell.viewport.name}-${cell.scheme}`
}

export function jobId(job: Job): string {
  return `${job.state.id} @ ${cellId(job.cell)}`
}

export function loadInventory(file: string): State[] {
  const raw = YAML.parse(fs.readFileSync(file, "utf8"))
  const list = Array.isArray(raw) ? raw : raw?.states
  if (!Array.isArray(list)) throw new Error(`${file}: expected a list of states`)
  const seen = new Set<string>()
  return list.map((entry: any, index: number) => {
    const state = validateState(entry, `${file}[${index}]`)
    if (seen.has(state.id)) throw new Error(`${file}: duplicate state id ${state.id}`)
    seen.add(state.id)
    return state
  })
}

function validateState(entry: any, where: string): State {
  if (!entry || typeof entry !== "object") throw new Error(`${where}: not a mapping`)
  if (typeof entry.id !== "string" || !entry.id) throw new Error(`${where}: missing id`)
  if (!/^[A-Za-z0-9_\-\/.]+$/.test(entry.id)) throw new Error(`${where}: id ${entry.id} must be path-like`)
  if (typeof entry.path !== "string" && (typeof entry.path !== "object" || !entry.path)) {
    throw new Error(`${entry.id}: missing path`)
  }
  const steps = entry.steps ?? []
  if (!Array.isArray(steps)) throw new Error(`${entry.id}: steps must be a list`)
  if (entry.actors) {
    if (!entry.capture || !entry.actors[entry.capture]) {
      throw new Error(`${entry.id}: multi-user states need capture: <actor>`)
    }
    for (const step of steps) {
      if (step.actor && !entry.actors[step.actor]) throw new Error(`${entry.id}: unknown actor ${step.actor}`)
    }
  }
  for (const vp of entry.matrix?.viewports ?? []) {
    if (!VIEWPORTS[vp]) throw new Error(`${entry.id}: unknown viewport ${vp}`)
  }
  for (const e of entry.matrix?.engines ?? []) {
    if (!ENGINES.includes(e)) throw new Error(`${entry.id}: unknown engine ${e}`)
  }
  for (const s of entry.matrix?.schemes ?? []) {
    if (!SCHEMES.includes(s)) throw new Error(`${entry.id}: unknown scheme ${s}`)
  }
  if (entry.kind && !["page", "fragment"].includes(entry.kind)) throw new Error(`${entry.id}: unknown kind ${entry.kind}`)
  return { seed: "default", ...entry, steps }
}

export interface MatrixFilter {
  engines?: Engine[]
  viewports?: string[]
  schemes?: Scheme[]
  only?: string[] // state id globs
  breakpoints?: "include" | "only" | "exclude"
  breakpointStates?: string[] // state id globs swept across breakpoints (default DEFAULT_BREAKPOINT_STATES)
}

// Representative layouts swept 1px either side of every width breakpoint: signed out, a room
// (group, direct, composer in use), settings forms, profile, search, and the empty state.
export const DEFAULT_BREAKPOINT_STATES = [
  "auth/sign_in",
  "rooms/show/designers",
  "rooms/show/direct",
  "interactions/composer/with_text",
  "interactions/actions_menu",
  "account/edit/admin",
  "rooms/opens/new",
  "users/profile",
  "users/show/self",
  "search/results",
  "welcome/no_rooms",
]

export function isFragment(state: State): boolean {
  return state.kind === "fragment"
}

export function expandJobs(states: State[], filter: MatrixFilter, breakpointWidths: Record<Engine, number[]>): Job[] {
  const jobs: Job[] = []
  const onlyRes = filter.only?.map(globToRegExp)
  const sweepRes = (filter.breakpointStates ?? DEFAULT_BREAKPOINT_STATES).map(globToRegExp)
  for (const state of states) {
    if (onlyRes && !onlyRes.some((re) => re.test(state.id))) continue
    const engines = intersect(state.matrix?.engines ?? ENGINES, filter.engines)
    const schemes = intersect(state.matrix?.schemes ?? SCHEMES, filter.schemes)
    if (isFragment(state)) {
      // A response, not a rendering: one capture, whatever the matrix.
      if (filter.breakpoints !== "only" && engines.length) {
        jobs.push({ state, cell: { engine: engines[0], viewport: VIEWPORTS.desktop, scheme: "light" } })
      }
      continue
    }
    if (filter.breakpoints !== "only") {
      const viewports = intersect(state.matrix?.viewports ?? VIEWPORT_NAMES, filter.viewports)
      for (const engine of engines) for (const vp of viewports) for (const scheme of schemes) {
        jobs.push({ state, cell: { engine, viewport: VIEWPORTS[vp], scheme } })
      }
    }
    const swept = state.breakpoints || sweepRes.some((re) => re.test(state.id))
    if (swept && filter.breakpoints !== "exclude") {
      for (const engine of engines) for (const width of breakpointWidths[engine] ?? []) for (const scheme of schemes) {
        jobs.push({ state, cell: { engine, viewport: breakpointViewport(width), scheme } })
      }
    }
  }
  return jobs
}

function intersect<T>(values: readonly T[], filter?: readonly T[]): T[] {
  return filter ? values.filter((v) => filter.includes(v)) : [...values]
}

export function globToRegExp(glob: string): RegExp {
  const escaped = glob.replace(/[.+^${}()|[\]\\]/g, "\\$&").replace(/\*\*/g, "\u0000").replace(/\*/g, "[^/]*").replace(/\?/g, ".")
  return new RegExp(`^${escaped.replace(/\u0000/g, ".*")}$`)
}

// {{table.label}} interpolation from parity/.seed/<seed>/labels.json, written by parity/bin/seed
// as a flat map ({"rooms.designers": 3, "clock.now": "2026-…"}); a nested map works too.
export type Labels = Record<string, any>

export function label(labels: Labels, table: string, name: string): string | number | undefined {
  return labels[`${table}.${name}`] ?? labels[table]?.[name]
}

const labelCache = new Map<string, Labels>()

export function loadLabels(seed: string, seedDir = SEED_DIR): Labels {
  if (!labelCache.has(seed)) {
    const file = path.join(seedDir, seed, "labels.json")
    labelCache.set(seed, fs.existsSync(file) ? JSON.parse(fs.readFileSync(file, "utf8")) : {})
  }
  return labelCache.get(seed)!
}

export function interpolate(template: string, labels: Labels, where: string): string {
  return template.replace(/\{\{\s*([A-Za-z0-9_]+)\.([A-Za-z0-9_]+)\s*\}\}/g, (_, table, name) => {
    const value = label(labels, table, name)
    if (value === undefined) throw new Error(`${where}: no fixture label {{${table}.${name}}}`)
    return String(value)
  })
}

// Deep-interpolates every string in a step.
export function interpolateStep<T>(step: T, labels: Labels, where: string): T {
  const walk = (value: any): any =>
    typeof value === "string" ? interpolate(value, labels, where)
    : Array.isArray(value) ? value.map(walk)
    : value && typeof value === "object" ? Object.fromEntries(Object.entries(value).map(([k, v]) => [k, walk(v)]))
    : value
  return walk(step)
}

// Seed metadata: the instant both servers' clocks are frozen at, if the seed records it.
export function seedTime(seed: string, seedDir = SEED_DIR): string | undefined {
  for (const name of ["time", "clock", "time.txt"]) {
    const file = path.join(seedDir, seed, name)
    if (fs.existsSync(file)) return fs.readFileSync(file, "utf8").trim()
  }
  const meta = path.join(seedDir, seed, "meta.json")
  if (fs.existsSync(meta)) {
    const json = JSON.parse(fs.readFileSync(meta, "utf8"))
    return json.time ?? json.clock ?? json.frozen_at
  }
  const now = label(loadLabels(seed, seedDir), "clock", "now")
  return typeof now === "string" ? now : undefined
}
