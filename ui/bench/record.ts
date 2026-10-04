import { appendFileSync } from 'node:fs'

/** Appends one result to $BENCH_OUT (JSON lines; `scripts/bench-check.mjs` compares them). */
export function record(key: string, value: number, unit: string, samples?: number[]) {
  const line = { key, value: Math.round(value * 10) / 10, unit, ...(samples ? { samples: samples.map((s) => Math.round(s * 10) / 10) } : {}) }
  console.log(`BENCH ${key} = ${line.value} ${unit}${samples ? ` (${line.samples!.join(', ')})` : ''}`)
  if (process.env.BENCH_OUT) appendFileSync(process.env.BENCH_OUT, JSON.stringify(line) + '\n')
}

export const median = (xs: number[]) => {
  const s = [...xs].sort((a, b) => a - b)
  return s.length % 2 ? s[s.length >> 1] : (s[s.length / 2 - 1] + s[s.length / 2]) / 2
}

export const quantile = (xs: number[], q: number) => {
  const s = [...xs].sort((a, b) => a - b)
  return s[Math.min(s.length - 1, Math.floor(q * s.length))]
}

type TraceEvent = { name: string; ph: string; ts: number; dur?: number; pid: number; tid: number }

/** Top-level main-thread tasks (ms, longest first) from trace events: as e2e/smoothness.spec.ts. */
export function mainThreadTasks(ev: TraceEvent[]): number[] {
  const n = new Map<string, number>()
  for (const e of ev) if (e.name === 'WebFrameWidgetImpl::BeginMainFrame') n.set(`${e.pid}:${e.tid}`, (n.get(`${e.pid}:${e.tid}`) ?? 0) + 1)
  const main = [...n].sort((a, b) => b[1] - a[1])[0]?.[0]
  const all = ev.filter((e) => `${e.pid}:${e.tid}` === main && e.ph === 'X' && e.dur && (e.name === 'ThreadControllerImpl::RunTask' || e.name === 'RunTask'))
  const out: number[] = []
  let end = -1
  for (const e of all.sort((a, b) => a.ts - b.ts)) {
    if (e.ts < end) continue
    out.push(e.dur! / 1000)
    end = e.ts + e.dur!
  }
  return out.sort((a, b) => b - a)
}
