// Writes src/pdf/vendor/pdf.worker.min.mjs: PDF.js's legacy worker with one change (DESIGN §22,
// item 70). Before a document is reported, PDF.js loads its last page to check /Count, and walking
// a flat /Pages tree to it (as scanners and img2pdf write them) fetches every page object first. It
// also prefetches every kid of the top-level /Pages node in parallel. With page objects interleaved
// with their images, that read a 100 MB scan in full before the first page. The change: when a
// /Pages node has exactly /Count kids, they are all leaves, so page i is Kids[i]. Only that kid is
// fetched, and the shortcut falls back to the usual walk unless it really is a page.
//
// A copy, not a patch in place: pnpm hard-links node_modules into its global store.
import { readFileSync, writeFileSync, mkdirSync, existsSync } from 'node:fs'
import { createRequire } from 'node:module'
import { dirname, join } from 'node:path'

const require = createRequire(import.meta.url)
const src = require.resolve('pdfjs-dist/legacy/build/pdf.worker.min.mjs')
const out = new URL('../src/pdf/vendor/pdf.worker.min.mjs', import.meta.url).pathname
const code = readFileSync(src, 'utf8')

const anchor = 'for(let e=h.length-1;e>=0;e--){const n=h[e];t.push(n);a===this.toplevelPagesDict&&n instanceof Ref&&!o.has(n)&&o.put(n,r.fetchAsync(n))}'
const shortcut =
  'if(Number.isInteger(c)&&c===h.length&&e>=l&&e-l<h.length&&h[e-l] instanceof Ref){const k=h[e-l],d=await(o.get(k)||r.fetchAsync(k));if(d instanceof Dict){let y=d.getRaw("Type");y instanceof Ref&&(y=await r.fetchAsync(y));if(isName(y,"Page")||!d.has("Kids")){s.has(k)||s.put(k,1);i.has(k)||i.put(k,e);return[d,k]}}}'
const at = code.indexOf(anchor)
if (at < 0 || code.indexOf(anchor, at + 1) >= 0 || !code.includes('async getPageDict(e){const t=[this.toplevelPagesDict]')) {
  console.error('patch-pdfjs: pdfjs-dist changed; update scripts/patch-pdfjs.mjs (or drop the patch if PDF.js no longer walks flat page trees)')
  process.exit(1)
}
const patched = code.slice(0, at) + shortcut + code.slice(at)
if (!existsSync(out) || readFileSync(out, 'utf8') !== patched) {
  mkdirSync(dirname(out), { recursive: true })
  writeFileSync(out, patched)
  writeFileSync(join(dirname(out), '.gitignore'), '*\n')
}
