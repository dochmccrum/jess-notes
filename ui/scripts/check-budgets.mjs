// Bundle budgets (DESIGN §11.8): gzipped size of the critical path (everything index.html loads
// eagerly, CodeMirror excluded) must stay within bench/budgets.json. Other groups are tracked.
import { readFileSync, readdirSync } from 'node:fs'
import { gzipSync } from 'node:zlib'
import { join } from 'node:path'

const dist = new URL('../dist/', import.meta.url).pathname
const budgets = JSON.parse(readFileSync(new URL('../../bench/budgets.json', import.meta.url), 'utf8'))
const gz = (f) => gzipSync(readFileSync(join(dist, f)), { level: 9 }).length
const html = readFileSync(join(dist, 'index.html'), 'utf8')
const eager = new Set()
for (const m of html.matchAll(/(?:src|href)="\/?(assets\/[^"]+\.(?:js|css))"/g)) eager.add(m[1])
// Static imports of eager chunks are eager too.
const queue = [...eager].filter((f) => f.endsWith('.js'))
while (queue.length) {
  const f = queue.pop()
  const src = readFileSync(join(dist, f), 'utf8')
  for (const m of src.matchAll(/(?:^|[;\s}])import\s*(?:[\w${},\s*]+from\s*)?["']\.\/([^"']+\.js)["']/g)) {
    const dep = `assets/${m[1]}`
    if (!eager.has(dep)) {
      eager.add(dep)
      queue.push(dep)
    }
  }
}
const assets = readdirSync(join(dist, 'assets')).map((f) => `assets/${f}`)
const groups = { main: 0, codemirror: 0, 'worker+wasm': 0, sqlite: 0 }
const lines = []
for (const f of eager) {
  const size = gz(f)
  const g = /\/codemirror-/.test(f) ? 'codemirror' : 'main'
  groups[g] += size
  lines.push(`  ${g.padEnd(12)} ${(size / 1024).toFixed(1).padStart(7)} KB  ${f}`)
}
for (const f of assets) {
  if (/sqlite3/.test(f)) groups.sqlite += gz(f)
  else if (/sync\.worker-|core_bg-/.test(f)) groups['worker+wasm'] += gz(f)
}
console.log('critical path (gzip):')
console.log(lines.join('\n'))
let failed = false
for (const [g, size] of Object.entries(groups)) {
  const limit = budgets.enforce[g] ?? budgets.track[g]
  const enforced = g in budgets.enforce
  const over = limit != null && size > limit
  if (over && enforced) failed = true
  console.log(`${g.padEnd(12)} ${(size / 1024).toFixed(1).padStart(7)} KB / ${limit ? (limit / 1024).toFixed(0) : '-'} KB ${over ? (enforced ? 'OVER BUDGET' : '(over, tracked)') : 'ok'}`)
}
// Benchmark results (scripts/bench.sh): one line per group.
if (process.env.BENCH_OUT) {
  const { appendFileSync } = await import('node:fs')
  for (const [g, size] of Object.entries(groups)) appendFileSync(process.env.BENCH_OUT, JSON.stringify({ key: `bundle_${g.replace('+', '_')}_kb`, value: Math.round(size / 102.4) / 10, unit: 'KB' }) + '\n')
}
if (failed) {
  console.error('bundle budget exceeded')
  process.exit(1)
}
