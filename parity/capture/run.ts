// Orchestrates a run: expand jobs, capture each against every target with a bounded pool of
// workers, compare, and write the report.
import os from "node:os"
import fs from "node:fs"
import path from "node:path"
import { execSync } from "node:child_process"
import { Allowlist } from "./allowlist.ts"
import { BrowserPool } from "./browsers.ts"
import { resolveBreakpointWidths, scanWidthConditions } from "./breakpoints.ts"
import { artifactBase, captureCell } from "./capture.ts"
import type { CaptureEnv, CellMeta, Target } from "./capture.ts"
import { compareJob } from "./compare.ts"
import type { CellComparison } from "./compare.ts"
import { ENGINES } from "./config.ts"
import type { Engine } from "./config.ts"
import { expandJobs, jobId } from "./inventory.ts"
import type { Job, MatrixFilter, State } from "./inventory.ts"
import { summarize, writeReport } from "./report.ts"
import { SessionCache } from "./session.ts"
import { startProxy } from "./proxy.ts"

// Each worker drives one browser context, roughly a core of renderer work, and the servers under
// test need room too. The box is shared, so the default is modest; PARITY_WORKERS or --workers
// raise it.
export function defaultWorkers(): number {
  const fromEnv = Number(process.env.PARITY_WORKERS)
  if (fromEnv > 0) return fromEnv
  return Math.max(2, Math.min(8, Math.floor(os.availableParallelism() / 4)))
}

export interface RunOptions {
  states: State[]
  filter: MatrixFilter
  targets: Target[]
  outDir: string
  time: string
  workers: number
  timeoutMs: number
  seedDir?: string
  allowlist?: Allowlist
  reset?: (target: Target) => void // restores a server's seed before a mutating job
  quiet?: boolean
  reportName?: string
}

export interface RunResult {
  jobs: Job[]
  metas: CellMeta[]
  comparisons: CellComparison[]
  reportFile?: string
  durationMs: number
}

export async function breakpointWidths(pool: BrowserPool, engines: readonly Engine[]): Promise<Record<Engine, number[]>> {
  const conditions = scanWidthConditions()
  const widths = {} as Record<Engine, number[]>
  for (const engine of engines) {
    const { browser, release } = await pool.acquire(engine)
    try {
      widths[engine] = await resolveBreakpointWidths(browser, conditions)
    } finally {
      await release()
    }
  }
  return widths
}

export async function run(options: RunOptions): Promise<RunResult> {
  const started = Date.now()
  const pool = new BrowserPool()
  const sessions = new SessionCache()
  const env: CaptureEnv = { pool, sessions, outDir: options.outDir, time: options.time, timeoutMs: options.timeoutMs, seedDir: options.seedDir }
  fs.mkdirSync(options.outDir, { recursive: true })
  const proxies = await Promise.all(options.targets.map((t) => startProxy(t.url)))
  options.targets.forEach((t, i) => (t.proxy = proxies[i].server))
  try {
    const wantsBreakpoints = options.filter.breakpoints !== "exclude"
    const engines = (options.filter.engines ?? ENGINES).filter((e) => ENGINES.includes(e))
    const widths = wantsBreakpoints ? await breakpointWidths(pool, engines) : ({} as Record<Engine, number[]>)
    if (wantsBreakpoints && !options.quiet) console.log(`breakpoint widths: ${JSON.stringify(widths)}`)
    const jobs = expandJobs(options.states, options.filter, widths)
    const parallel = interleave(jobs.filter((j) => !j.state.mutates))
    const serial = jobs.filter((j) => j.state.mutates)
    const metas: CellMeta[] = []
    const comparisons: CellComparison[] = []
    let done = 0

    const reset = (target: Target) => {
      options.reset!(target)
      sessions.forget(target)
    }

    const runJob = async (job: Job) => {
      const captured: CellMeta[] = []
      for (const target of options.targets) {
        if (job.state.mutates && options.reset) reset(target)
        let meta = await captureCell(job, target, env)
        if (meta.error) {
          // One retry for infrastructure flakes (a crashed renderer, a page whose modules never
          // ran); the retry is recorded, and a deterministic failure fails again.
          const first = meta.error
          if (job.state.mutates && options.reset) reset(target)
          meta = await captureCell(job, target, env)
          meta.retriedAfter = first
          fs.writeFileSync(artifactBase(options.outDir, target.name, job) + ".json", JSON.stringify(meta, null, 2) + "\n")
        }
        captured.push(meta)
      }
      metas.push(...captured)
      const error = captured.find((m) => m.error)?.error
      let status = error ? "error" : "captured"
      if (options.targets.length === 2 && options.allowlist) {
        const comparison = compareJob(job, options.outDir, options.targets[0].name, options.targets[1].name, options.allowlist)
        comparisons.push(comparison)
        status = comparison.status
      }
      done++
      if (!options.quiet && (status !== "pass" || done % 25 === 0 || done === jobs.length)) {
        console.log(`[${done}/${jobs.length}] ${jobId(job)} ${status}${error ? `: ${error.split("\n")[0]}` : ""}`)
      }
    }

    if (!options.quiet) console.log(`${jobs.length} cells (${parallel.length} parallel, ${serial.length} serial) × ${options.targets.length} targets, ${options.workers} workers`)
    await pool_(parallel, options.workers, runJob)
    for (const job of serial) await runJob(job)

    const result: RunResult = { jobs, metas, comparisons, durationMs: Date.now() - started }
    if (options.targets.length === 2 && options.allowlist) {
      result.reportFile = writeReport(options.outDir, options.reportName ?? "report", comparisons, {
        title: "Parity report",
        expected: `${options.targets[0].name} ${options.targets[0].url}`,
        actual: `${options.targets[1].name} ${options.targets[1].url}`,
        startedAt: new Date(started).toISOString(),
        durationMs: result.durationMs,
        unusedAllowlist: options.allowlist.unused(),
      })
    }
    return result
  } finally {
    await pool.close()
    await Promise.all(proxies.map((p) => p.close()))
  }
}

async function pool_<T>(items: T[], concurrency: number, fn: (item: T) => Promise<void>) {
  let next = 0
  const worker = async () => {
    while (next < items.length) await fn(items[next++])
  }
  await Promise.all(Array.from({ length: Math.min(concurrency, items.length) }, worker))
}

// Round-robin across engines so concurrent workers spread over all browsers.
export function interleave(jobs: Job[]): Job[] {
  const groups = ENGINES.map((engine) => jobs.filter((j) => j.cell.engine === engine))
  const out: Job[] = []
  for (let k = 0; out.length < jobs.length; k++) for (const group of groups) if (group[k]) out.push(group[k])
  return out
}

export function summaryLine(result: RunResult): string {
  if (!result.comparisons.length) {
    const errors = result.metas.filter((m) => m.error).length
    return `${result.metas.length} captures, ${errors} errors in ${(result.durationMs / 1000).toFixed(1)}s`
  }
  const c = summarize(result.comparisons)
  return `${result.comparisons.length} cells: ${c.pass} pass, ${c.fail} fail, ${c.allowed} allowed, ${c.error} error in ${(result.durationMs / 1000).toFixed(1)}s`
}

export function shell(command: string) {
  execSync(command, { stdio: ["ignore", "inherit", "inherit"] })
}

export function relativeToCwd(file: string) {
  return path.relative(process.cwd(), file) || file
}
