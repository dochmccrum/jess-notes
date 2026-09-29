// Injects the precache list (every built file except sw.js) into dist/sw.js.
import { readFileSync, writeFileSync, readdirSync, statSync } from 'node:fs'
import { createHash } from 'node:crypto'
import { join, relative } from 'node:path'

const dist = new URL('../dist/', import.meta.url).pathname
const files = []
const walk = (d) => {
  for (const n of readdirSync(d)) {
    const p = join(d, n)
    if (statSync(p).isDirectory()) walk(p)
    else files.push(relative(dist, p).split('\\').join('/'))
  }
}
walk(dist)
// Large lazily-loaded chunks (KaTeX fonts etc.) are still precached: offline must work fully.
const list = files.filter((f) => f !== 'sw.js' && f !== 'index.html' && !f.endsWith('.map')).sort()
const h = createHash('sha256')
for (const f of list) h.update(f)
h.update(readFileSync(join(dist, 'index.html')))
const version = h.digest('hex').slice(0, 12)
const swPath = join(dist, 'sw.js')
const sw = readFileSync(swPath, 'utf8')
// A JS string literal holding the JSON (the minifier may have switched the quote style).
const literal = JSON.stringify(JSON.stringify({ version, files: list }))
const marker = /(["'`])__JESS_PRECACHE__\1/g
if (!marker.test(sw)) throw new Error('sw.js: precache marker not found')
writeFileSync(swPath, sw.replace(marker, () => literal))
console.log(`sw.js: ${list.length} files precached (version ${version})`)
