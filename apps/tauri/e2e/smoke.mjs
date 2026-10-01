// Smoke test of the real Linux app (CEF runtime, DESIGN §23) over the DevTools protocol:
// sign in, write a note, paste an image (served back through `jess-blob://`), check the server
// has both, find the note by full-text search, check the frame rate, and time cold starts.
//
//   A display (Xvfb/xvfb-run on CI) and the UI's node_modules (Playwright) needed.
//   node apps/tauri/e2e/smoke.mjs [path/to/jess-notes-app] [path/to/jess]
import { spawn } from 'node:child_process'
import { createRequire } from 'node:module'
import { mkdtempSync, readFileSync } from 'node:fs'
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
const { chromium } = createRequire(join(root, 'ui/package.json'))('@playwright/test')

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

// ------------------------------------------------------------------ the app, over CDP
let proc, browser, page
/** Launches the app on our data dirs (CEF's cache too) and attaches to its page. */
async function launch() {
  const cdp = await freePort()
  // Its own process group: CEF's helper processes go with it, and CEF ignores SIGTERM.
  proc = spawn(app, [], {
    detached: true,
    stdio: 'ignore',
    env: { ...process.env, JESS_CDP_PORT: String(cdp), XDG_DATA_HOME: join(tmp, 'xdg'), XDG_CACHE_HOME: join(tmp, 'cache'), XDG_CONFIG_HOME: join(tmp, 'config') },
  })
  browser = await waitFor('DevTools endpoint', () => chromium.connectOverCDP(`http://127.0.0.1:${cdp}`))
  page = await waitFor('app page', () => browser.contexts()[0]?.pages().find((p) => p.url().startsWith('tauri://')))
}
/** Closes the window (the app flushes and exits), then makes sure nothing is left. */
async function quit() {
  const exited = new Promise((r) => proc.once('exit', r))
  await page?.close().catch(() => {})
  await Promise.race([exited, sleep(5000)])
  try {
    process.kill(-proc.pid, 'SIGKILL')
  } catch {}
  await browser?.close().catch(() => {})
  proc = browser = page = null
}
const text = (css) => page.locator(css).first().innerText()
const exec = (fn, arg) => page.evaluate(fn, arg)

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

  const started = Date.now()
  await launch()
  console.log(`engine: ${await exec(() => navigator.userAgent.match(/Chrome\/[\d.]+/)?.[0])}`)
  // First launch: server address, then password.
  await page.fill('[data-testid=server]', base)
  await page.click('button[type=submit]')
  await page.fill('[data-testid=password]', PASSWORD)
  const signIn = Date.now()
  await page.click('button[type=submit]')
  await waitFor('synced', async () => /Synced/.test(await text('[data-testid=sync-status]')))
  console.log(`launch → synced ${Date.now() - started} ms (sign-in click → synced ${Date.now() - signIn} ms)`)

  // Frame pacing: rAF runs at the display's rate (120 Hz panels must get 120, not WebKitGTK's 60).
  const fr = await exec(
    () =>
      new Promise((res) => {
        const d = []
        let last = 0
        let t0 = 0
        const f = (t) => {
          if (last) d.push(t - last)
          last = t
          t0 ||= t
          if (t - t0 < 2000) requestAnimationFrame(f)
          else res({ fps: d.length / ((t - t0) / 1000), median: d.sort((a, b) => a - b)[d.length >> 1] })
        }
        requestAnimationFrame(f)
      }),
  )
  const hz = Number(process.env.SMOKE_HZ ?? 0)
  console.log(`frames: ${fr.fps.toFixed(1)} fps (median ${fr.median.toFixed(2)} ms)${hz ? `, display ${hz} Hz` : ''}`)
  if (hz && fr.fps < hz * 0.95) throw new Error(`rAF at ${fr.fps.toFixed(1)} fps on a ${hz} Hz display`)

  // A note.
  await page.click('[data-testid=new-note]')
  await page.waitForSelector('.cm-content')
  await page.click('.cm-content')
  const word = `cefsmoke${Date.now()}`
  await page.keyboard.type(`Hello from CEF ${word}`)
  // An image, pasted: it's ingested natively and rendered from jess-blob://.
  await exec(
    () =>
      new Promise((done) => {
        const c = document.createElement('canvas')
        c.width = 400
        c.height = 300
        const g = c.getContext('2d')
        g.fillStyle = 'teal'
        g.fillRect(0, 0, 400, 300)
        window.__smokeCanvas = c
        c.toBlob((b) => {
          const dt = new DataTransfer()
          dt.items.add(new File([b], 'smoke.png', { type: 'image/png' }))
          document.querySelector('.cm-content').dispatchEvent(new ClipboardEvent('paste', { clipboardData: dt, bubbles: true, cancelable: true }))
          done(true)
        }, 'image/png')
      }),
  )
  const src = await waitFor('image rendered', () =>
    exec(() => {
      const i = document.querySelector('.cm-content .jess-img img')
      return i && i.complete && i.naturalWidth === 400 ? i.src : null
    }),
  )
  if (!/^(jess-blob:|http:\/\/jess-blob\.localhost)/.test(src)) throw new Error(`image not served by jess-blob: ${src}`)
  console.log(`image served from ${src.slice(0, 40)}…`)
  // A HEIC file (kept byte-for-byte on non-Apple devices): Chromium can't decode it, so the embed
  // must show the HEIC fallback rather than a broken image.
  await exec(() => {
    const bytes = new Uint8Array([0, 0, 0, 24, 102, 116, 121, 112, 104, 101, 105, 99, 0, 0, 0, 0, 109, 105, 102, 49, 104, 101, 105, 99])
    const dt = new DataTransfer()
    dt.items.add(new File([bytes], 'photo.heic', { type: 'image/heic' }))
    document.querySelector('.cm-content').dispatchEvent(new ClipboardEvent('paste', { clipboardData: dt, bubbles: true, cancelable: true }))
  })
  await waitFor('HEIC fallback', () => exec(() => !!document.querySelector('.jess-img-placeholder.unsupported')))
  console.log('HEIC shows its fallback')
  // A PDF: pasted, then opened in the viewer (PDF.js, bytes over the blob channel).
  const pdf = [...readFileSync(join(root, 'tests/fixtures/vault/Docs/Paper.pdf'))]
  await exec((bytes) => {
    const dt = new DataTransfer()
    dt.items.add(new File([new Uint8Array(bytes)], 'Smoke paper.pdf', { type: 'application/pdf' }))
    document.querySelector('.cm-content').dispatchEvent(new ClipboardEvent('paste', { clipboardData: dt, bubbles: true, cancelable: true }))
  }, pdf)
  await sleep(500)
  await page.keyboard.press('Control+o')
  await waitFor('quick switcher', () => exec(() => document.activeElement?.placeholder?.startsWith('Find or create') ?? false))
  await page.keyboard.type('Smoke paper')
  await waitFor('switcher hit', () => exec(() => [...document.querySelectorAll('[role=option]')].some((o) => o.textContent.includes('Smoke paper.pdf'))))
  await page.keyboard.press('Enter')
  const firstPage = await waitFor('PDF first page', () => exec(() => performance.getEntriesByName('pdf-first-page').at(-1)?.duration ?? null), 20_000)
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
  await page.click('[role=tab]:nth-child(2)')
  await page.fill('[aria-label="Search notes"]', word)
  await waitFor('search hit', async () => (await text('aside')).includes('Untitled'), 10_000)

  // Cold start: relaunch on the same data (the boot record is written 2 s after the last change).
  await page.click('[role=tab]:nth-child(1)')
  // Back to the note (the PDF was opened last), so the relaunch restores the note.
  await exec(() => [...document.querySelectorAll('[data-testid=tree] [data-id]')].find((r) => r.textContent.trim() === 'Untitled')?.click())
  await waitFor('note reopened', async () => (await text('.cm-content')).includes(word))
  await sleep(2500)
  await quit()
  const samples = []
  for (let i = 0; i < 3; i++) {
    await launch()
    await waitFor('editor after relaunch', async () => (await text('.cm-content')).includes(word))
    samples.push(await exec(() => performance.getEntriesByName('note-visible')[0]?.startTime ?? -1))
    if (process.env.SMOKE_TIMINGS && i === 2) {
      const t = await exec(() => {
        const n = performance.getEntriesByType('navigation')[0]
        return JSON.stringify({ nav: n && { resEnd: n.responseEnd, domInteractive: n.domInteractive, dcl: n.domContentLoadedEventEnd }, marks: performance.getEntriesByType('mark').map((m) => [m.name, Math.round(m.startTime)]), measures: performance.getEntriesByType('measure').map((m) => [m.name, Math.round(m.startTime), Math.round(m.duration)]), res: performance.getEntriesByType('resource').map((r) => [r.name.split('/').pop(), Math.round(r.startTime), Math.round(r.responseEnd)]) })
      })
      console.log(t)
    }
    if (i < 2) await quit()
  }
  console.log(`cold start → note visible: ${samples.map((x) => x.toFixed(0)).join(', ')} ms (CEF, ${/target\/debug\//.test(app) ? 'debug' : 'release'} build)`)
  if (Math.min(...samples) <= 0) throw new Error('no note-visible mark after relaunch')

  const out = join(tmp, 'app.png')
  await page.screenshot({ path: out })
  console.log(`OK — screenshot: ${out}`)
  await quit()
}

main()
  .then(() => (process.exitCode = 0))
  .catch(async (e) => {
    console.error('FAILED:', e.message)
    try {
      await page.screenshot({ path: join(tmp, 'fail.png') })
      console.error(`screenshot: ${join(tmp, 'fail.png')}`)
    } catch {}
    process.exitCode = 1
  })
  .finally(async () => {
    if (proc) await quit()
    for (const c of children) c.kill()
  })
