// Smoke test of the real Android app on an emulator or device, driven over the WebView's DevTools
// socket (Playwright's `_android`, over adb): sign in, write a note with the keyboard up, paste
// an image (served back through http://jess-blob.localhost), open a PDF, search, the back
// gesture, foreground catch-up of 200 remote changes, and cold start → note visible.
//
//   A debug APK installed (`tauri android build --debug --apk`, then `adb install -r …`), one
//   device attached, `target/debug/jess` built and ui dependencies installed.
//   node apps/tauri/e2e/android-smoke.mjs [path/to/jess]
import { execFileSync, spawn } from 'node:child_process'
import { mkdtempSync, readFileSync, writeFileSync } from 'node:fs'
import { createRequire } from 'node:module'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { createServer } from 'node:net'
import { createHash } from 'node:crypto'
import { inflateRawSync } from 'node:zlib'

const root = resolve(new URL('../../..', import.meta.url).pathname)
const { _android } = createRequire(join(root, 'ui/package.json'))('@playwright/test')
const jess = resolve(process.argv[2] ?? join(root, 'target/debug/jess'))
const PKG = 'app.jessnotes.notes'
const PASSWORD = 'correct horse battery'
const tmp = mkdtempSync(join(tmpdir(), 'jess-android-e2e-'))
const children = []
let device

const sleep = (ms) => new Promise((r) => setTimeout(r, ms))
// Synchronous, so it blocks Playwright's own timeouts: a hung adb call (e.g. `am start -W` waiting
// for a first frame a glitched emulator never draws) must fail by itself.
const adb = (...args) => {
  try {
    return execFileSync('adb', args, { encoding: 'utf8', timeout: 60_000 }).trim()
  } catch (e) {
    if (e.code === 'ETIMEDOUT') throw new Error(`adb ${args.join(' ')}: no answer in 60 s`)
    throw e
  }
}
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
    await sleep(100)
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

/** A stored (uncompressed) zip: [name, bytes][] → bytes. */
function zip(files) {
  const crcTable = Array.from({ length: 256 }, (_, n) => {
    let c = n
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1
    return c >>> 0
  })
  const crc = (b) => {
    let c = 0xffffffff
    for (const x of b) c = crcTable[(c ^ x) & 0xff] ^ (c >>> 8)
    return (c ^ 0xffffffff) >>> 0
  }
  const locals = []
  const central = []
  let off = 0
  for (const [name, data] of files) {
    const n = Buffer.from(name)
    const h = Buffer.alloc(30)
    h.writeUInt32LE(0x04034b50, 0)
    h.writeUInt16LE(20, 4)
    h.writeUInt16LE(0x800, 6)
    h.writeUInt32LE(crc(data), 14)
    h.writeUInt32LE(data.length, 18)
    h.writeUInt32LE(data.length, 22)
    h.writeUInt16LE(n.length, 26)
    locals.push(h, n, data)
    const c = Buffer.alloc(46)
    c.writeUInt32LE(0x02014b50, 0)
    c.writeUInt16LE(20, 4)
    c.writeUInt16LE(20, 6)
    c.writeUInt16LE(0x800, 8)
    c.writeUInt32LE(crc(data), 16)
    c.writeUInt32LE(data.length, 20)
    c.writeUInt32LE(data.length, 24)
    c.writeUInt16LE(n.length, 28)
    c.writeUInt32LE(off, 42)
    central.push(c, n)
    off += 30 + n.length + data.length
  }
  const cd = Buffer.concat(central)
  const end = Buffer.alloc(22)
  end.writeUInt32LE(0x06054b50, 0)
  end.writeUInt16LE(files.length, 8)
  end.writeUInt16LE(files.length, 10)
  end.writeUInt32LE(cd.length, 12)
  end.writeUInt32LE(off, 16)
  return Buffer.concat([...locals, cd, end])
}

/** Attaches to the app's WebView (Playwright finds its DevTools socket through adb). */
async function attach() {
  device ??= (await _android.devices())[0]
  if (!device) throw new Error('no Android device')
  const wv = await device.webView({ pkg: PKG }, { timeout: 20_000 })
  return wv.page()
}

/** Launches the activity and returns the device's epoch ms just before (for cold-start timing). */
function launch() {
  const out = adb('shell', `date +%s%3N; am start -W -n ${PKG}/.MainActivity`)
  return { at: Number(out.split('\n')[0]), total: Number(/TotalTime: (\d+)/.exec(out)?.[1] ?? NaN) }
}

/**
 * Runs `action`, which restarts the app into another space (DESIGN §24: MainActivity restarts it
 * from a separate process), then waits for the new process and attaches to its WebView.
 */
async function restarting(action) {
  const pid = () => {
    try {
      return adb('shell', 'pidof', PKG)
    } catch {
      return ''
    }
  }
  const before = pid()
  await action()
  await waitFor('the app to restart', () => {
    const now = pid()
    return now && now !== before
  }, 30_000)
  await detach()
  // Attach only once the new MainActivity is on screen (its WebView's DevTools socket exists by
  // then), so Playwright can't latch onto the old process's socket.
  await waitFor('the restarted activity', () => /ResumedActivity.*app\.jessnotes\.notes\/\.MainActivity/.test(adb('shell', 'dumpsys', 'activity', 'activities')), 30_000)
  const page = await waitFor(
    'the restarted WebView',
    async () => {
      const p = await attach()
      if (await p.evaluate(() => !!document.querySelector('#app > *')).catch(() => false)) return p
      await detach()
      return null
    },
    60_000,
  )
  return page
}

async function detach() {
  await device?.close().catch(() => {})
  device = null
}

/** On a phone the sidebar is a drawer (DESIGN §11.5): open it with the ☰ button. */
async function openDrawer(page) {
  if (await page.$('.sidebar.open')) return
  await page.locator('[aria-label="Toggle sidebar"]:visible').first().click()
  await page.waitForSelector('.sidebar.open')
  await sleep(300) // the slide-in transition
}

async function main() {
  // Server, reachable from the device as 127.0.0.1 through `adb reverse`.
  const port = await freePort()
  const srv = spawn(jess, ['serve'], {
    env: { ...process.env, JESS_DATA_DIR: join(tmp, 'server'), PORT: String(port), JESS_ADMIN_PASSWORD: PASSWORD, JESS_UI_DIR: join(tmp, 'no-ui'), RUST_LOG: 'warn' },
    stdio: 'inherit',
  })
  children.push(srv)
  const base = `http://127.0.0.1:${port}`
  await waitFor('server', async () => (await fetch(`${base}/healthz`)).ok)
  adb('reverse', `tcp:${port}`, `tcp:${port}`)
  const tok = (await (await fetch(`${base}/api/auth/login`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ password: PASSWORD, device_name: 'check' }) })).json()).token
  const api = (path, init = {}) => fetch(`${base}${path}`, { ...init, headers: { authorization: `Bearer ${tok}`, 'content-type': 'application/json', ...init.headers } })

  console.log(`device: Android ${adb('shell', 'getprop', 'ro.build.version.release')}, ${adb('shell', 'getprop', 'ro.product.model')}`)
  // `input tap` counts as a stylus on emulators: Gboard would show its handwriting tutorial.
  adb('shell', 'settings', 'put', 'secure', 'stylus_handwriting_enabled', '0')
  adb('shell', 'pm', 'clear', PKG)
  launch()
  let page = await attach()
  console.log(`WebView: ${await page.evaluate(() => /Chrome\/[\d.]+/.exec(navigator.userAgent)?.[0])}`)
  // Frame pacing (DESIGN §23.4): the WebView follows the display; on a 120 Hz phone MainActivity
  // asks for the fastest mode. `SMOKE_HZ=120` turns the log into a check.
  const fps = await page.evaluate(
    () =>
      new Promise((res) => {
        let n = 0
        let t0 = 0
        const f = (t) => {
          t0 ||= t
          if (t - t0 < 2000) {
            n++
            requestAnimationFrame(f)
          } else res(n / ((t - t0) / 1000))
        }
        requestAnimationFrame(f)
      }),
  )
  console.log(`frames: ${fps.toFixed(1)} fps`)
  const hz = Number(process.env.SMOKE_HZ ?? 0)
  if (hz && fps < hz * 0.95) throw new Error(`rAF at ${fps.toFixed(1)} fps on a ${hz} Hz display`)

  // A fresh install asks where notes live (DESIGN §24). First a local space: the real server,
  // embedded in the app on 127.0.0.1.
  const synced = (p) => p.waitForFunction(() => /Synced/.test(document.querySelector('[data-testid=sync-status]')?.textContent ?? ''), null, { timeout: 30_000 })
  await page.waitForSelector('[data-testid=spaces-welcome]')
  await page.click('[data-testid=choose-local]')
  await page.fill('[data-testid=space-name]', 'Phone notes')
  page = await restarting(() => page.click('[data-testid=space-local-submit]'))
  await synced(page)
  await openDrawer(page)
  await page.click('[data-testid=new-note]')
  await page.waitForSelector('.cm-content')
  await page.click('.cm-content')
  await page.keyboard.type('Written in a local space on Android')
  await synced(page)
  console.log('local space: embedded server running, note committed')

  // Then the test server, added as a remote space.
  await openDrawer(page)
  await page.click('[data-testid=space-switcher]')
  await page.click('[data-testid=add-remote]')
  await page.fill('[data-testid=space-server]', base)
  await page.fill('[data-testid=space-password]', PASSWORD)
  page = await restarting(() => page.click('[data-testid=space-remote-submit]'))
  await synced(page)

  // A note, typed through the on-screen keyboard's input path.
  await openDrawer(page)
  await page.click('[data-testid=new-note]')
  await page.waitForSelector('.cm-content')
  await page.click('.cm-content')
  const word = `androidsmoke${Date.now()}`
  await page.keyboard.type(`Hello from Android ${word}`)
  // A real tap in the editor brings up the soft keyboard: the WebView must shrink (MainActivity
  // pads the content view by the IME inset) so the caret stays visible.
  const full = await page.evaluate(() => screen.height)
  const [w, h] = adb('shell', 'wm', 'size').split(': ').pop().split('x').map(Number)
  // A freshly booted CI emulator sometimes ignores the first tap (its keyboard is still starting).
  for (let tries = 1; ; tries++) {
    adb('shell', 'input', 'tap', String(Math.round(w / 2)), String(Math.round(h * 0.2)))
    try {
      await page.waitForFunction(() => innerHeight < screen.height * 0.7, null, { timeout: 8_000 })
      break
    } catch (e) {
      if (tries === 3) throw e
    }
  }
  await page.keyboard.press('Control+End')
  await page.keyboard.type(' typed with the keyboard up')
  const kb = await page.evaluate(() => ({ h: innerHeight, caret: document.querySelector('.cm-cursor')?.getBoundingClientRect().bottom ?? 0 }))
  console.log(`keyboard up: viewport ${full}px → ${kb.h}px, caret bottom at ${kb.caret.toFixed(0)}px`)
  if (kb.caret > kb.h || kb.caret <= 0) throw new Error('caret is hidden behind the keyboard')

  // An image, pasted: ingested natively and rendered from http://jess-blob.localhost.
  await page.evaluate(
    () =>
      new Promise((done) => {
        const c = document.createElement('canvas')
        c.width = 400
        c.height = 300
        const g = c.getContext('2d')
        g.fillStyle = 'teal'
        g.fillRect(0, 0, 400, 300)
        c.toBlob((b) => {
          const dt = new DataTransfer()
          dt.items.add(new File([b], 'smoke.png', { type: 'image/png' }))
          document.querySelector('.cm-content').dispatchEvent(new ClipboardEvent('paste', { clipboardData: dt, bubbles: true, cancelable: true }))
          done(true)
        }, 'image/png')
      }),
  )
  const src = await page.waitForFunction(() => {
    const i = document.querySelector('.cm-content .jess-img img')
    return i && i.complete && i.naturalWidth === 400 ? i.src : null
  }, null, { timeout: 20_000 })
  if (!(await src.jsonValue()).startsWith('http://jess-blob.localhost/')) throw new Error(`image not served by jess-blob: ${await src.jsonValue()}`)
  console.log('image served from http://jess-blob.localhost')

  // A PDF: pasted, then opened in the viewer (PDF.js, bytes over the blob channel).
  const pdf = [...readFileSync(join(root, 'tests/fixtures/vault/Docs/Paper.pdf'))]
  await page.evaluate((bytes) => {
    const dt = new DataTransfer()
    dt.items.add(new File([new Uint8Array(bytes)], 'Smoke paper.pdf', { type: 'application/pdf' }))
    document.querySelector('.cm-content').dispatchEvent(new ClipboardEvent('paste', { clipboardData: dt, bubbles: true, cancelable: true }))
  }, pdf)
  await sleep(500)
  await page.click('[data-testid=open-palette]')
  await page.keyboard.type('Quick switcher')
  await page.keyboard.press('Enter')
  await page.waitForFunction(() => document.activeElement?.placeholder?.startsWith('Find or create'))
  await page.keyboard.type('Smoke paper')
  await page.waitForFunction(() => [...document.querySelectorAll('[role=option]')].some((o) => o.textContent.includes('Smoke paper.pdf')))
  await page.keyboard.press('Enter')
  const firstPage = await (await page.waitForFunction(() => performance.getEntriesByName('pdf-first-page').at(-1)?.duration ?? null, null, { timeout: 20_000 })).jsonValue()
  console.log(`PDF first page in ${firstPage.toFixed(0)} ms`)

  // Back gesture: first back to the note the PDF was opened from, then the drawer closes, then
  // (nothing left) the app goes to the background without finishing.
  adb('shell', 'input', 'keyevent', 'KEYCODE_BACK')
  await page.waitForFunction((w) => document.querySelector('.cm-content')?.textContent.includes(w), word)
  await openDrawer(page)
  adb('shell', 'input', 'keyevent', 'KEYCODE_BACK')
  await page.waitForFunction(() => !document.querySelector('.sidebar.open'))
  console.log('back: PDF → note, then closes the drawer')
  await page.waitForFunction(() => /Synced/.test(document.querySelector('[data-testid=sync-status]')?.textContent ?? '') && !/Uploading/.test(document.querySelector('footer')?.textContent ?? ''), null, { timeout: 30_000 })

  // The server has the note text and the image.
  const files = unzip(Buffer.from(await (await api('/api/admin/export.zip')).arrayBuffer()))
  if (![...files.values()].some((b) => b.includes(Buffer.from(word)))) throw new Error(`note text not on the server (files: ${[...files.keys()].join(', ')})`)
  if (![...files.keys()].some((k) => k.endsWith('smoke.png'))) throw new Error('image not on the server')

  // Full-text search (native SQLite FTS).
  await openDrawer(page)
  await page.click('[role=tab]:nth-child(2)')
  await page.fill('[aria-label="Search notes"]', word)
  await page.waitForFunction(() => document.querySelector('aside')?.textContent.includes('Untitled'), null, { timeout: 10_000 })
  await page.click('[role=tab]:nth-child(1)')
  await page.evaluate(() => [...document.querySelectorAll('[data-testid=tree] [data-id]')].find((r) => r.textContent.trim() === 'Untitled')?.click())
  await page.waitForFunction((w) => document.querySelector('.cm-content')?.textContent.includes(w), word)
  adb('shell', 'input', 'keyevent', 'KEYCODE_BACK') // closes the drawer if it's still open
  await sleep(2500) // the boot record is written 2 s after the last change

  // Back with nothing open: the app goes to the background and its process stays.
  adb('shell', 'input', 'keyevent', 'KEYCODE_BACK')
  const pid = adb('shell', 'pidof', PKG)
  await waitFor('backgrounded', () => !adb('shell', 'dumpsys', 'activity', 'activities').split('\n').find((l) => /topResumedActivity|mResumedActivity/.test(l))?.includes(PKG))
  if (adb('shell', 'pidof', PKG) !== pid) throw new Error('back at the root finished the app')
  console.log('back with nothing open: app in the background, process kept')

  // Foreground catch-up (DESIGN §18: 200 pending remote changes → applied in <500 ms): another
  // device imports 200 notes while this one is in the background.
  const before = await page.evaluate(() => window.__jess.entryCount())
  const notes = Array.from({ length: 200 }, (_, i) => [`Catchup/Note ${i}.md`, Buffer.from(`# Note ${i}\n\nSee [[Note ${(i + 1) % 200}]].\n`)])
  const z = zip(notes)
  const hash = createHash('sha256').update(z).digest('hex')
  const up = await (await api(`/api/blobs/${hash}/uploads`, { method: 'POST', body: JSON.stringify({ size: z.length }) })).json()
  if (!up.present) {
    await api(`/api/blobs/${hash}/uploads/${up.upload_id}/chunks/0`, { method: 'PUT', body: z, headers: { 'content-type': 'application/octet-stream', 'x-chunk-sha256': hash } })
    await api(`/api/blobs/${hash}/uploads/${up.upload_id}/complete`, { method: 'POST' })
  }
  const imp = await api('/api/admin/import', { method: 'POST', body: JSON.stringify({ zip_hash: hash }) })
  if (!imp.ok) throw new Error(`server import: ${imp.status} ${await imp.text()}`)
  await sleep(3000)
  const fg = launch()
  const hostStart = Date.now()
  // (The zip's single top-level folder is stripped by the importer: 200 entries.)
  await page.waitForFunction((n) => window.__jess.entryCount() >= n, before + 200, { timeout: 30_000, polling: 10 })
  console.log(`catch-up: 200 remote notes applied ${Date.now() - hostStart} ms after the foreground intent returned (am start -W: ${fg.total} ms)`)

  // Cold start: force-stop, launch, measure launch → note-visible on the device's clock.
  const samples = []
  for (let i = 0; i < 3; i++) {
    await detach()
    adb('shell', 'am', 'force-stop', PKG)
    await sleep(1000)
    const l = launch()
    page = await attach()
    await page.waitForFunction((w) => document.querySelector('.cm-content')?.textContent.includes(w), word, { timeout: 20_000 })
    const visible = await page.evaluate(() => performance.timeOrigin + (performance.getEntriesByName('note-visible')[0]?.startTime ?? NaN))
    samples.push({ total: visible - l.at, nav: await page.evaluate(() => performance.getEntriesByName('note-visible')[0]?.startTime ?? NaN), first: l.total })
  }
  console.log(`cold start → note visible: ${samples.map((s) => s.total.toFixed(0)).join(', ')} ms from launch (WebView navigation → note ${samples.map((s) => s.nav.toFixed(0)).join(', ')} ms; first frame ${samples.map((s) => s.first).join(', ')} ms)`)

  const shot = join(tmp, 'app.png')
  writeFileSync(shot, execFileSync('adb', ['exec-out', 'screencap', '-p'], { timeout: 60_000 }))
  console.log(`OK — screenshot: ${shot}`)
}

main()
  .then(() => (process.exitCode = 0))
  .catch((e) => {
    console.error('FAILED:', e.message)
    try {
      writeFileSync(join(tmp, 'fail.png'), execFileSync('adb', ['exec-out', 'screencap', '-p'], { timeout: 60_000 }))
      console.error(`screenshot: ${join(tmp, 'fail.png')}`)
    } catch {}
    process.exitCode = 1
  })
  .finally(async () => {
    await detach()
    for (const c of children) c.kill()
  })
