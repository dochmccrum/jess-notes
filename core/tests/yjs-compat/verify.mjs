// Applies the yrs-generated updates (../../fixtures/yrs-updates.json) with Yjs.
import * as Y from 'yjs'
import { readFileSync } from 'node:fs'

const cases = JSON.parse(readFileSync(new URL('../../fixtures/yrs-updates.json', import.meta.url)))
let failed = 0
for (const c of cases) {
  const d = new Y.Doc()
  for (const u of c.updates) Y.applyUpdate(d, Buffer.from(u, 'hex'))
  const got = d.getText('t').toString()
  if (got !== c.text) { failed++; console.error(`FAIL ${c.name}: ${JSON.stringify(got)} != ${JSON.stringify(c.text)}`) }
  // Rewrites authored by yrs at UTF-16 offsets land where Yjs expects them.
  if (c.rewrite_check && !got.includes(c.rewrite_check)) { failed++; console.error(`FAIL ${c.name}: missing ${c.rewrite_check}`) }
}
if (failed) process.exit(1)
console.log(`yrs → yjs: ${cases.length} cases ok`)
