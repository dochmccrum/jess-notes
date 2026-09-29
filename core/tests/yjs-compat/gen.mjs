// Generates Yjs updates that the Rust core (yrs, OffsetKind::Utf16) must apply identically.
// Writes ../../fixtures/yjs-updates.json. Deterministic (fixed clientIDs, seeded RNG).
import * as Y from 'yjs'
import { writeFileSync } from 'node:fs'

const hex = (u8) => Buffer.from(u8).toString('hex')
let seed = 42
const rnd = (n) => { seed = (seed * 1103515245 + 12345) % 2147483648; return seed % n }

function doc (id) { const d = new Y.Doc(); d.clientID = id; return d }
function capture (d) { const ups = []; d.on('update', (u) => ups.push(hex(u))); return ups }

const cases = []

{ // basic + emoji + CRLF + BOM
  const d = doc(1); const ups = capture(d); const t = d.getText('t')
  t.insert(0, '﻿Hello 😀 world\r\n')
  t.insert(9, '🎉') // right after the emoji (UTF-16 offset 9; 😀 occupies 7..9)
  t.delete(0, 1) // remove BOM
  t.insert(t.length, 'line2\r\nline3\n')
  t.delete(6, 2) // delete the 😀 surrogate pair
  cases.push({ name: 'emoji-crlf', updates: ups, text: t.toString() })
}

{ // concurrent edits merged
  const a = doc(10); const b = doc(20)
  const ua = capture(a); const ub = capture(b)
  a.getText('t').insert(0, 'base [[Link]] text')
  Y.applyUpdate(b, Y.encodeStateAsUpdate(a))
  a.getText('t').insert(7, 'Old')
  b.getText('t').delete(7, 4)
  b.getText('t').insert(7, 'New')
  Y.applyUpdate(a, Y.encodeStateAsUpdate(b))
  Y.applyUpdate(b, Y.encodeStateAsUpdate(a))
  cases.push({ name: 'concurrent', updates: [...ua, ...ub], text: a.getText('t').toString() })
  cases.push({ name: 'merged', updates: [hex(Y.mergeUpdates([...ua, ...ub].map((h) => Buffer.from(h, 'hex'))))], text: a.getText('t').toString() })
  cases.push({ name: 'state-as-update', updates: [hex(Y.encodeStateAsUpdate(a))], text: a.getText('t').toString() })
}

{ // random edit fuzz with astral chars
  const d = doc(7); const ups = capture(d); const t = d.getText('t')
  const alphabet = ['a', 'é', '😀', '\r\n', '\n', '[[x]]', '日本', ' ']
  for (let i = 0; i < 300; i++) {
    const s = t.toString()
    const len = s.length
    if (len > 0 && rnd(3) === 0) {
      let at = rnd(len); let n = 1 + rnd(4)
      // never split a surrogate pair
      if (at > 0 && s.charCodeAt(at) >= 0xDC00 && s.charCodeAt(at) < 0xE000) at--
      let end = Math.min(len, at + n)
      if (end < len && s.charCodeAt(end) >= 0xDC00 && s.charCodeAt(end) < 0xE000) end++
      t.delete(at, end - at)
    } else {
      let at = rnd(len + 1)
      if (at > 0 && at < len && s.charCodeAt(at) >= 0xDC00 && s.charCodeAt(at) < 0xE000) at--
      t.insert(at, alphabet[rnd(alphabet.length)])
    }
  }
  cases.push({ name: 'fuzz', updates: ups, text: t.toString() })
}

writeFileSync(new URL('../../fixtures/yjs-updates.json', import.meta.url), JSON.stringify(cases, null, 1) + '\n')
console.log(`wrote ${cases.length} cases`)
