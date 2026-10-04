#!/usr/bin/env node
// Compares benchmark results (JSON lines from scripts/bench.sh) with bench/thresholds.json.
// Prints a table (and a Markdown one to $GITHUB_STEP_SUMMARY on CI); exits 1 if a target is
// exceeded by more than the tolerance, or a thresholded metric is missing.
//
//   node scripts/bench-check.mjs target/bench/results.jsonl
import { readFileSync, appendFileSync } from 'node:fs'

const file = process.argv[2] ?? 'target/bench/results.jsonl'
const cfg = JSON.parse(readFileSync(new URL('../bench/thresholds.json', import.meta.url), 'utf8'))
const results = new Map()
for (const line of readFileSync(file, 'utf8').split('\n')) if (line.trim()) {
  const r = JSON.parse(line)
  results.set(r.key, r) // the last run of a metric wins
}
const tol = cfg.tolerance ?? 0.1
const rows = []
let failed = 0
for (const [key, m] of Object.entries(cfg.metrics)) {
  const r = results.get(key)
  let verdict
  if (!r) verdict = m.max == null ? 'n/a' : 'MISSING'
  else if (m.max == null) verdict = 'tracked'
  else if (r.value <= m.max * (1 + tol) + 1e-9) verdict = r.value <= m.max ? 'ok' : 'ok (noise)'
  else verdict = 'FAIL'
  if (verdict === 'FAIL' || verdict === 'MISSING') failed++
  rows.push([m.label ?? key, r ? `${r.value} ${r.unit}` : '–', m.max == null ? '' : `≤ ${m.max}`, verdict])
}
for (const [key, r] of results) if (!(key in cfg.metrics)) rows.push([key, `${r.value} ${r.unit}`, '', 'unlisted'])
const w = [0, 1, 2].map((i) => Math.max(...rows.map((r) => r[i].length)))
for (const r of rows) console.log(`${r[0].padEnd(w[0])}  ${r[1].padStart(w[1])}  ${r[2].padEnd(w[2])}  ${r[3]}`)
if (process.env.GITHUB_STEP_SUMMARY) {
  const md = ['| metric | result | target | |', '|---|---:|---|---|', ...rows.map((r) => `| ${r.join(' | ')} |`)].join('\n')
  appendFileSync(process.env.GITHUB_STEP_SUMMARY, `## Benchmarks (DESIGN §18)\n\n${md}\n`)
}
if (failed) {
  console.error(`\n${failed} benchmark(s) over target`)
  process.exit(1)
}
console.log('\nall benchmarks within target')
