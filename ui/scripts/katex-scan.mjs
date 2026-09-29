// KaTeX compatibility scan (DESIGN §20.6): renders every maths span in a vault with the same
// options as the editor and reports what fails. Maths spans come from jess-core's extractor, so
// currency, code and escapes are excluded exactly as in the app.
//
//   node scripts/katex-scan.mjs /path/to/vault [--json]
import { readFileSync, readdirSync, statSync } from 'node:fs'
import { join, relative } from 'node:path'
import katex from 'katex'
import init, { extract } from '../src/wasm/core.js'

const root = process.argv[2]
if (!root) {
  console.error('usage: node scripts/katex-scan.mjs <vault> [--json]')
  process.exit(2)
}
await init({ module_or_path: readFileSync(new URL('../src/wasm/core_bg.wasm', import.meta.url)) })

const skip = /^(\.obsidian|\.git|\.trash|node_modules)$/
function* notes(d) {
  for (const n of readdirSync(d)) {
    if (skip.test(n)) continue
    const p = join(d, n)
    if (statSync(p).isDirectory()) yield* notes(p)
    else if (n.toLowerCase().endsWith('.md')) yield p
  }
}

const opts = { throwOnError: true, trust: false, strict: 'ignore', maxSize: 50, maxExpand: 1000, output: 'htmlAndMathml' }
let files = 0
let spans = 0
const failures = []
const macros = new Map()
for (const f of notes(root)) {
  files++
  let text
  try {
    text = new TextDecoder('utf-8', { fatal: true }).decode(readFileSync(f))
  } catch {
    continue
  }
  const ex = JSON.parse(extract(text))
  for (const m of ex.math ?? []) {
    spans++
    // Ranges are UTF-16 offsets, which is what JS strings use.
    const raw = text.slice(m.range[0], m.range[1])
    const src = m.display ? raw.replace(/^\$\$/, '').replace(/\$\$$/, '') : raw.replace(/^\$/, '').replace(/\$$/, '')
    try {
      katex.renderToString(src, { ...opts, displayMode: m.display })
    } catch (e) {
      const msg = String(e.message ?? e).replace(/^KaTeX parse error: /, '')
      const cmd = /Undefined control sequence: (\\\w+)/.exec(msg)?.[1]
      if (cmd) macros.set(cmd, (macros.get(cmd) ?? 0) + 1)
      const line = text.slice(0, m.range[0]).split('\n').length
      failures.push({ file: relative(root, f), line, display: m.display, src: src.length > 120 ? src.slice(0, 117) + '…' : src, error: msg })
    }
  }
}

if (process.argv.includes('--json')) {
  console.log(JSON.stringify({ files, spans, failures }, null, 2))
} else {
  console.log(`${files} notes, ${spans} maths spans, ${failures.length} failed (${spans ? ((100 * (spans - failures.length)) / spans).toFixed(2) : '100.00'}% render)`)
  if (macros.size) {
    console.log('\nUnsupported commands (count):')
    for (const [c, n] of [...macros].sort((a, b) => b[1] - a[1])) console.log(`  ${c}  ${n}`)
  }
  for (const x of failures.slice(0, 200)) console.log(`\n${x.file}:${x.line} ${x.display ? '[display]' : '[inline]'}\n  ${x.src}\n  → ${x.error}`)
}
process.exit(0)
