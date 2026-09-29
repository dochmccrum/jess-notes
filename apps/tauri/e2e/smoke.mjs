// Smoke test of the real Linux app on WebKitGTK through tauri-driver (WebDriver):
// sign in, write a note, paste an image (served back through `jess-blob://`), check the server
// has both, and find the note by full-text search.
//
//   Xvfb/xvfb-run, WebKitWebDriver (webkit2gtk-driver) and `cargo install tauri-driver` needed.
//   node apps/tauri/e2e/smoke.mjs [path/to/jess-notes-app] [path/to/jess]
import { spawn } from 'node:child_process'
import { mkdtempSync, readFileSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { createServer } from 'node:net'
import { inflateRawSync } from 'node:zlib'

const root = resolve(new URL('../../..', import.meta.url).pathname)
const app = resolve(process.argv[2] ?? join(root, 'target/debug/jess-notes-app'))
const jess = resolve(process.argv[3] ?? join(root, 'target/debug/jess'))
const PASSWORD = 'correct horse battery'
const tmp = mkdtempSync(join(tmpdir(), 'jess-app-e2e-'))
const children = []

const sleep = (ms) => new Promise((r) => setTimeout(r, ms))
const freePort = () =>
  new Promise((r) => {
    const s = createServer().listen(0, () => {
      const p = s.address().port
      s.close(() => r(p))
    })
  })

async function waitFor(what, fn, ms = 20_000) {
  const t = Date.now()
  let last
  while (Date.now() - t < ms) {
    try {
      const v = await fn()
      if (v) return v
    } catch (e) {
      last = e
    }
    await sleep(200)
  }
  throw new Error(`timed out: ${what}${last ? ` (${last.message})` : ''}`)
}

/** Entries of a (non-ZIP64) zip, via its central directory: name → bytes. */
function unzip(buf) {
  let e = buf.length - 22
  while (e >= 0 && buf.readUInt32LE(e) !== 0x06054b50) e--
  const count = buf.readUInt16LE(e + 10)
  let p = buf.readUInt32LE(e + 16)
  const out = new Map()
  for (let i = 0; i < count; i++) {
    const method = buf.readUInt16LE(p + 10)
    const csize = buf.readUInt32LE(p + 20)
    const nlen = buf.readUInt16LE(p + 28)
    const xlen = buf.readUInt16LE(p + 30)
    const clen = buf.readUInt16LE(p + 32)
    const local = buf.readUInt32LE(p + 42)
    const name = buf.subarray(p + 46, p + 46 + nlen).toString('utf8')
    const start = local + 30 + buf.readUInt16LE(local + 26) + buf.readUInt16LE(local + 28)
    const data = buf.subarray(start, start + csize)
    out.set(name, method === 8 ? inflateRawSync(data) : data)
    p += 46 + nlen + xlen + clen
  }
  return out
}

// ------------------------------------------------------------------ WebDriver (raw HTTP)
let wd, sid
async function cmd(method, path, body) {
  const r = await fetch(`${wd}/session${sid ? `/${sid}` : ''}${path}`, {
    method,
    headers: { 'content-type': 'application/json' },
    body: body ? JSON.stringify(body) : undefined,
  })
  const j = await r.json()
  if (!r.ok) throw new Error(`${method} ${path}: ${JSON.stringify(j.value)}`)
  return j.value
}
const EL = 'element-6066-11e4-a52e-4f735466cecf'
const exec = (script, args = []) => cmd('POST', '/execute/sync', { script, args })
const find = async (css) => (await cmd('POST', '/element', { using: 'css selector', value: css }))[EL]
const execAsync = (script, args = []) => cmd('POST', '/execute/async', { script, args })
const type = async (css, text) => cmd('POST', `/element/${await find(css)}/value`, { text })
const click = async (css) => cmd('POST', `/element/${await find(css)}/click`, {})
const text = async (css) => cmd('GET', `/element/${await find(css)}/text`)

async function main() {
  // Server.
  const port = await freePort()
  const srv = spawn(jess, ['serve'], {
    env: { ...process.env, JESS_DATA_DIR: join(tmp, 'server'), PORT: String(port), JESS_ADMIN_PASSWORD: PASSWORD, JESS_UI_DIR: join(tmp, 'no-ui'), RUST_LOG: 'warn' },
    stdio: 'inherit',
  })
  children.push(srv)
  const base = `http://127.0.0.1:${port}`
  await waitFor('server', async () => (await fetch(`${base}/healthz`)).ok)

  // Driver; the app keeps its data under $XDG_DATA_HOME.
  const wport = await freePort()
  const drv = spawn('tauri-driver', ['--port', String(wport)], { env: { ...process.env, XDG_DATA_HOME: join(tmp, 'xdg') }, stdio: 'inherit' })
  children.push(drv)
  wd = `http://127.0.0.1:${wport}`
  await waitFor('tauri-driver', async () => (await fetch(`${wd}/status`)).ok)
  sid = (await cmd('POST', '', { capabilities: { alwaysMatch: { 'tauri:options': { application: app } } } })).sessionId

  const started = Date.now()
  // First launch: server address, then password.
  await waitFor('server step', () => find('[data-testid=server]'))
  await type('[data-testid=server]', base)
  await click('button[type=submit]')
  await waitFor('password step', () => find('[data-testid=password]'))
  await type('[data-testid=password]', PASSWORD)
  const signIn = Date.now()
  await click('button[type=submit]')
  await waitFor('synced', async () => /Synced/.test(await text('[data-testid=sync-status]')))
  console.log(`launch → synced ${Date.now() - started} ms (sign-in click → synced ${Date.now() - signIn} ms)`)

  // A note.
  await click('[data-testid=new-note]')
  await waitFor('editor', () => find('.cm-content'))
  await click('.cm-content')
  const word = `webkitsmoke${Date.now()}`
  await type('.cm-content', `Hello from WebKitGTK ${word}`)
  // An image, pasted: it's ingested natively and rendered from jess-blob://.
  await execAsync(`
    const done = arguments[arguments.length - 1]
    const c = document.createElement('canvas'); c.width = 400; c.height = 300
    const g = c.getContext('2d'); g.fillStyle = 'teal'; g.fillRect(0, 0, 400, 300)
    c.toBlob((b) => {
      const dt = new DataTransfer()
      dt.items.add(new File([b], 'smoke.png', { type: 'image/png' }))
      document.querySelector('.cm-content').dispatchEvent(new ClipboardEvent('paste', { clipboardData: dt, bubbles: true, cancelable: true }))
      done(true)
    }, 'image/png')`)
  const src = await waitFor('image rendered', () =>
    exec(`const i = document.querySelector('.cm-content .jess-img img'); return i && i.complete && i.naturalWidth === 400 ? i.src : null`),
  )
  if (!/^(jess-blob:|http:\/\/jess-blob\.localhost)/.test(src)) throw new Error(`image not served by jess-blob: ${src}`)
  console.log(`image served from ${src.slice(0, 40)}…`)
  // A HEIC file (kept byte-for-byte on non-Apple devices): WebKitGTK can't decode it, so the
  // embed must show the HEIC fallback rather than a broken image.
  await execAsync(`
    const done = arguments[arguments.length - 1]
    const bytes = new Uint8Array([0, 0, 0, 24, 102, 116, 121, 112, 104, 101, 105, 99, 0, 0, 0, 0, 109, 105, 102, 49, 104, 101, 105, 99])
    const dt = new DataTransfer()
    dt.items.add(new File([bytes], 'photo.heic', { type: 'image/heic' }))
    document.querySelector('.cm-content').dispatchEvent(new ClipboardEvent('paste', { clipboardData: dt, bubbles: true, cancelable: true }))
    done(true)`)
  await waitFor('HEIC fallback', () => exec(`return !!document.querySelector('.jess-img-placeholder.unsupported')`))
  console.log('HEIC shows its fallback')
  // A PDF: pasted, then opened in the viewer (PDF.js on WebKitGTK, bytes over the blob channel).
  const pdf = [...readFileSync(join(root, 'tests/fixtures/vault/Docs/Paper.pdf'))]
  await execAsync(`
    const done = arguments[arguments.length - 1]
    const dt = new DataTransfer()
    dt.items.add(new File([new Uint8Array(arguments[0])], 'Smoke paper.pdf', { type: 'application/pdf' }))
    document.querySelector('.cm-content').dispatchEvent(new ClipboardEvent('paste', { clipboardData: dt, bubbles: true, cancelable: true }))
    done(true)`, [pdf])
  await sleep(500)
  await cmd('POST', '/actions', { actions: [{ type: 'key', id: 'kb', actions: [{ type: 'keyDown', value: '\uE009' }, { type: 'keyDown', value: 'o' }, { type: 'keyUp', value: 'o' }, { type: 'keyUp', value: '\uE009' }] }] })
  await waitFor('quick switcher', () => exec(`return document.activeElement?.placeholder?.startsWith('Find or create') ?? false`))
  const input = (await cmd('GET', '/element/active'))[EL]
  await cmd('POST', `/element/${input}/value`, { text: 'Smoke paper' })
  await waitFor('switcher hit', () => exec(`return [...document.querySelectorAll('[role=option]')].some((o) => o.textContent.includes('Smoke paper.pdf'))`))
  await cmd('POST', `/element/${input}/value`, { text: '\uE007' })
  const firstPage = await waitFor('PDF first page', () => exec(`return performance.getEntriesByName('pdf-first-page').at(-1)?.duration ?? null`), 20_000)
  console.log(`PDF first page in ${firstPage.toFixed(0)} ms`)
  await waitFor('synced after edits', async () => /Synced/.test(await text('[data-testid=sync-status]')) && !/Uploading/.test(await text('footer')), 30_000)

  // The server has the note text and the image.
  const tok = (await (await fetch(`${base}/api/auth/login`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ password: PASSWORD, device_name: 'check' }) })).json()).token
  const zip = Buffer.from(await (await fetch(`${base}/api/admin/export.zip`, { headers: { authorization: `Bearer ${tok}` } })).arrayBuffer())
  const files = unzip(zip)
  const note = [...files].find(([name, b]) => name.endsWith('.md') && b.includes(Buffer.from(word)))
  if (!note) throw new Error(`note text not on the server (files: ${[...files.keys()].join(', ')})`)
  if (!files.has('smoke.png') && ![...files.keys()].some((k) => k.endsWith('/smoke.png'))) throw new Error('image not on the server')

  // Full-text search (native SQLite FTS).
  await click('[role=tab]:nth-child(2)')
  await type('[aria-label="Search notes"]', word)
  await waitFor('search hit', async () => (await text('aside')).includes('Untitled'), 10_000)

  // Cold start: relaunch on the same data (the boot record is written 2 s after the last change).
  await click('[role=tab]:nth-child(1)')
  // Back to the note (the PDF was opened last), so the relaunch restores the note.
  await exec(`[...document.querySelectorAll('[data-testid=tree] [data-id]')].find((r) => r.textContent.trim() === 'Untitled')?.click()`)
  await waitFor('note reopened', async () => (await text('.cm-content')).includes(word))
  await sleep(2500)
  await cmd('DELETE', '')
  sid = null
  const samples = []
  for (let i = 0; i < 3; i++) {
    sid = (await cmd('POST', '', { capabilities: { alwaysMatch: { 'tauri:options': { application: app } } } })).sessionId
    await waitFor('editor after relaunch', async () => (await text('.cm-content')).includes(word))
    samples.push(await exec(`return performance.getEntriesByName('note-visible')[0]?.startTime ?? -1`))
    if (process.env.SMOKE_TIMINGS && i === 2) {
      const t = await exec(`const n = performance.getEntriesByType('navigation')[0]; return JSON.stringify({ nav: n && { resEnd: n.responseEnd, domInteractive: n.domInteractive, dcl: n.domContentLoadedEventEnd }, marks: performance.getEntriesByType('mark').map((m) => [m.name, Math.round(m.startTime)]), measures: performance.getEntriesByType('measure').map((m) => [m.name, Math.round(m.startTime), Math.round(m.duration)]), res: performance.getEntriesByType('resource').map((r) => [r.name.split('/').pop(), Math.round(r.startTime), Math.round(r.responseEnd)]) })`)
      console.log(t)
    }
    if (i < 2) {
      await cmd('DELETE', '')
      sid = null
    }
  }
  console.log(`cold start → note visible: ${samples.map((x) => x.toFixed(0)).join(', ')} ms (WebKitGTK, ${/release/.test(app) ? 'release' : 'debug'} build)`)
  if (Math.min(...samples) <= 0) throw new Error('no note-visible mark after relaunch')

  const png = await cmd('GET', '/screenshot')
  const out = join(tmp, 'app.png')
  writeFileSync(out, Buffer.from(png, 'base64'))
  console.log(`OK — screenshot: ${out}`)
  await cmd('DELETE', '')
}

main()
  .then(() => (process.exitCode = 0))
  .catch(async (e) => {
    console.error('FAILED:', e.message)
    try {
      writeFileSync(join(tmp, 'fail.png'), Buffer.from(await cmd('GET', '/screenshot'), 'base64'))
      console.error(`screenshot: ${join(tmp, 'fail.png')}`)
    } catch {}
    process.exitCode = 1
  })
  .finally(() => {
    for (const c of children) c.kill()
  })
