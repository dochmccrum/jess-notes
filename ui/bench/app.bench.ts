import { test, expect, chromium, devices, type Page, type BrowserContext } from '@playwright/test'
import { execFileSync } from 'node:child_process'
import { createHash } from 'node:crypto'
import { mkdtempSync, readFileSync, writeFileSync, mkdirSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { record, median, quantile, mainThreadTasks } from './record'
import { PASSWORD } from './bench.config'

// DESIGN §18 in the browser, against the generated vault (tools/vaultgen): one device that has
// synced the whole vault (warm SW and IndexedDB, every attachment local, the default on desktop),
// plus a second device for sync latency and catch-up.

test.describe.configure({ mode: 'serial' })

let ctx: BrowserContext
let page: Page

const status = (p: Page) => p.getByTestId('sync-status')
const footer = (p: Page) => p.locator('footer.status')

async function login(p: Page) {
  p.on('console', (m) => m.text().startsWith('long frame') && console.log(m.text()))
  await p.goto('/')
  await p.getByTestId('password').fill(PASSWORD)
  await p.getByRole('button', { name: 'Sign in' }).click()
  await expect(status(p)).toHaveText(/Synced/, { timeout: 10 * 60_000 })
}

/** Opens a note or PDF through the quick switcher (the top result for `name`). For a note, waits
 *  until its editor is up (a new `open-note` measure). */
async function open(p: Page, name: string) {
  const path = p.getByRole('navigation', { name: 'Path' })
  if ((await path.count()) && (await path.textContent())?.trim().endsWith(name.replace(/\.md$/, ''))) return
  const opened = await p.evaluate(() => performance.getEntriesByName('open-note').length)
  await p.keyboard.press('Control+o')
  await p.keyboard.insertText(name)
  await expect(p.getByRole('option').first()).toContainText(name)
  await p.keyboard.press('Enter')
  await expect(p.getByRole('navigation', { name: 'Path' })).toContainText(name.replace(/\.md$/, ''))
  if (!/\.pdf$/i.test(name)) await expect.poll(() => p.evaluate(() => performance.getEntriesByName('open-note').length)).toBeGreaterThan(opened)
}

/**
 * Milliseconds from the input event (its timestamp) to the first frame painted after `done`
 * holds in the page: a MutationObserver notices the DOM change, then rAF + a task lands after
 * that frame's paint.
 */
async function inputToPaint(p: Page, act: () => Promise<void>, done: (arg: string) => boolean, arg: string): Promise<number> {
  await p.evaluate(() => {
    const w = window as unknown as { __in: number }
    w.__in = 0
    const note = (e: Event) => (w.__in = e.timeStamp)
    for (const t of ['input', 'click', 'keydown']) document.addEventListener(t, note, { capture: true, once: false })
  })
  const waiting = p.evaluate(
    ({ src, arg }) =>
      new Promise<number>((resolve) => {
        const check = new Function('arg', `return (${src})(arg)`) as (a: string) => boolean
        const finish = () => requestAnimationFrame(() => setTimeout(() => resolve(performance.now()), 0))
        if (check(arg)) return finish()
        const mo = new MutationObserver(() => {
          if (check(arg)) {
            mo.disconnect()
            finish()
          }
        })
        mo.observe(document.body, { subtree: true, childList: true, characterData: true, attributes: true })
      }),
    { src: done.toString(), arg },
  )
  await act()
  const end = await waiting
  const start = await p.evaluate(() => (window as unknown as { __in: number }).__in)
  return end - start
}

/** Main-thread tasks of 1 ms or more while `action` runs, from a Chrome trace (`toplevel`,
 *  `blink`: as e2e/smoothness.spec.ts; the idle scheduler's tiny tasks would dilute a p99). */
async function traceTasks(p: Page, action: () => Promise<void>): Promise<number[]> {
  const cdp = await ctx.newCDPSession(p)
  const events: Parameters<typeof mainThreadTasks>[0] = []
  cdp.on('Tracing.dataCollected', (d) => events.push(...(d.value as unknown as typeof events)))
  const traced = new Promise((r) => cdp.once('Tracing.tracingComplete', r))
  await cdp.send('Tracing.start', { categories: 'toplevel,blink', transferMode: 'ReportEvents' })
  await action()
  await cdp.send('Tracing.end')
  await traced
  await cdp.detach()
  return mainThreadTasks(events).filter((t) => t >= 1)
}

test('setup: sign in, sync the vault, download every attachment', async ({ baseURL }) => {
  // A persistent profile on disk: an incognito context keeps IndexedDB in memory with a small
  // quota, which the vault's ~1 GB of attachments exceeds (the app then falls back to on-demand).
  // Normalised: Chrome's IndexedDB can't open its backing store under a path with `..` in it.
  const dir = resolve(process.env.BENCH_PROFILE ?? join(tmpdir(), 'jess-bench-profile'))
  rmSync(dir, { recursive: true, force: true })
  ctx = await chromium.launchPersistentContext(dir, { ...devices['Desktop Chrome'], baseURL })
  page = ctx.pages()[0] ?? (await ctx.newPage())
  const t0 = Date.now()
  await login(page)
  record('initial_sync_s', (Date.now() - t0) / 1000, 's')
  // Downloads start a few seconds after the sync (the offline policy runs on a timer): wait for
  // them to start, then until none has been pending for 10 s.
  await expect(footer(page)).toContainText('Downloading', { timeout: 60_000 })
  await expect
    .poll(
      async () => {
        for (let i = 0; i < 10; i++) {
          if ((await footer(page).textContent())?.includes('Downloading')) return false
          await page.waitForTimeout(1000)
        }
        return true
      },
      { timeout: 30 * 60_000, intervals: [5000] },
    )
    .toBe(true)
  await expect(page.getByText(/out of storage space/)).toHaveCount(0)
  // (IndexedDB compresses values, so this is under the ~1 GB of attachments.)
  record('initial_download_stored_mb', (await page.evaluate(async () => (await navigator.storage.estimate()).usage ?? 0)) / 1048576, 'MB')
  record('initial_download_s', (Date.now() - t0) / 1000, 's')
  // Let the search index catch up (it runs in the worker after the sync).
  await expect.poll(() => page.evaluate(() => (window as unknown as { __jess: { indexReady(): boolean } }).__jess.indexReady()), { timeout: 10 * 60_000, intervals: [1000] }).toBe(true)
})

test('cold start → note visible (10k vault)', async () => {
  await open(page, 'Small note')
  await page.waitForTimeout(2500) // the boot record is written 2 s after the last change
  const run = async () => {
    const xs: number[] = []
    for (let i = 0; i < 7; i++) {
      await page.reload()
      await expect(page.locator('.cm-content')).toContainText('A short note to type into.')
      xs.push(await page.evaluate(() => performance.getEntriesByName('note-visible')[0]?.startTime ?? -1))
    }
    return xs.slice(2) // the first reloads warm the service worker and the HTTP cache
  }
  const xs = await run()
  record('cold_start_ms', median(xs), 'ms', xs)
  const cdp = await ctx.newCDPSession(page)
  await cdp.send('Emulation.setCPUThrottlingRate', { rate: 4 })
  const slow = await run()
  await cdp.send('Emulation.setCPUThrottlingRate', { rate: 1 })
  record('cold_start_cpu4x_ms', median(slow), 'ms', slow)
})

test('open note; a note with 50 images (no layout shift)', async () => {
  const names = ['Small note', 'Maths', 'Item 00042', 'Item 03000', 'Large note']
  const opens: number[] = []
  for (let i = 0; i < 10; i++) {
    await open(page, names[i % names.length])
    opens.push(await page.evaluate(() => (performance.getEntriesByName('open-note').at(-1) as PerformanceMeasure).duration))
  }
  record('open_note_ms', median(opens), 'ms', opens)
  const gallery: number[] = []
  let cls = 0
  for (let i = 0; i < 5; i++) {
    await open(page, 'Small note')
    await page.evaluate(() => {
      const w = window as unknown as { __cls: number; __clsObs?: PerformanceObserver }
      w.__cls = 0
      w.__clsObs?.disconnect()
      w.__clsObs = new PerformanceObserver((l) => {
        for (const e of l.getEntries() as (PerformanceEntry & { value: number; hadRecentInput: boolean })[]) if (!e.hadRecentInput) w.__cls += e.value
      })
      w.__clsObs.observe({ type: 'layout-shift' })
    })
    await open(page, 'Gallery')
    await expect(page.locator('.cm-content .jess-img img').first()).toBeVisible()
    gallery.push(await page.evaluate(() => (performance.getEntriesByName('open-note').at(-1) as PerformanceMeasure).duration))
    await page.waitForTimeout(1500)
    cls = Math.max(cls, await page.evaluate(() => (window as unknown as { __cls: number }).__cls))
  }
  record('open_note_50_images_ms', median(gallery), 'ms', gallery)
  record('open_note_50_images_cls', cls, 'CLS')
})

test('switcher, autocomplete and search at 10k notes and 20k attachments', async () => {
  const targets = ['Item 04217', 'Item 01234', 'Item 02999', 'Item 00777', 'Item 04888']
  const sw: number[] = []
  for (const t of targets) {
    await page.keyboard.press('Control+o')
    await expect(page.getByRole('dialog')).toBeVisible()
    sw.push(
      await inputToPaint(
        page,
        () => page.keyboard.insertText(t),
        (t) => !!document.querySelector('[role=option]')?.textContent?.includes(t),
        t,
      ),
    )
    await page.keyboard.press('Escape')
  }
  record('switcher_ms', median(sw), 'ms', sw)

  await open(page, 'Small note')
  await page.locator('.cm-content').click()
  await page.keyboard.press('Control+End')
  const ac: number[] = []
  for (const t of targets) {
    await page.keyboard.type('\n[[')
    ac.push(
      await inputToPaint(
        page,
        () => page.keyboard.insertText(t),
        (t) => !!document.querySelector('.cm-tooltip-autocomplete li')?.textContent?.includes(t),
        t,
      ),
    )
    await page.keyboard.press('Escape')
  }
  record('autocomplete_ms', median(ac), 'ms', ac)
  await page.keyboard.press('Control+z') // keep the note small for the typing test

  await page.getByRole('tab', { name: 'Search' }).click()
  const words = ['harbour', 'matrix lemma', 'garden energy', 'proof', 'journey station']
  const se: number[] = []
  for (const q of words) {
    await page.getByTestId('search-input').fill('')
    await expect(page.getByTestId('search-results').locator('li')).toHaveCount(0)
    await page.getByTestId('search-input').focus()
    se.push(
      await inputToPaint(
        page,
        () => page.keyboard.insertText(q),
        () => document.querySelectorAll('[data-testid=search-results] li').length > 0,
        q,
      ),
    )
  }
  record('search_ms', median(se), 'ms', se)
  await page.getByTestId('search-input').fill('')
  await page.getByRole('tab', { name: 'Files' }).click()
})

test('typing latency in a 1 MB note, a maths note, and one with 30 images and 3 PDFs', async () => {
  for (const [name, key] of [
    ['Large note', 'large'],
    ['Maths', 'maths'],
    ['Mixed media', 'media'],
  ] as const) {
    await open(page, name)
    await page.locator('.cm-content').click()
    await page.keyboard.press('Control+Home')
    await page.keyboard.press('End')
    const n = 200
    const tasks = await traceTasks(page, async () => {
      // Observed from here: starting a trace can itself make a long frame.
      await page.evaluate(() => {
        const w = window as unknown as { __ev: Map<number, number>; __loaf: number[]; __obs: PerformanceObserver[] }
        w.__ev = new Map()
        w.__loaf = []
        w.__obs?.forEach((o) => o.disconnect())
        const ev = new PerformanceObserver((l) => {
          for (const e of l.getEntries() as (PerformanceEntry & { interactionId: number })[]) if (e.interactionId) w.__ev.set(e.interactionId, Math.max(w.__ev.get(e.interactionId) ?? 0, e.duration))
        })
        ev.observe({ type: 'event', durationThreshold: 16, buffered: false } as PerformanceObserverInit)
        const lo = new PerformanceObserver((l) => {
          for (const e of l.getEntries() as (PerformanceEntry & { blockingDuration: number; scripts: { invoker: string; duration: number; sourceURL: string; sourceFunctionName: string }[] })[]) {
            w.__loaf.push(e.duration)
            if (e.duration > 50) console.log(`long frame ${Math.round(e.duration)} ms (blocking ${Math.round(e.blockingDuration)}): ${e.scripts.map((s) => `${s.invoker} ${Math.round(s.duration)} ms ${s.sourceFunctionName}@${s.sourceURL.split('/').pop()}`).join('; ') || 'no scripts'}`)
          }
        })
        try {
          lo.observe({ type: 'long-animation-frame', buffered: false })
        } catch {
          /* no LoAF */
        }
        w.__obs = [ev, lo]
      })
      await page.keyboard.type(' typing latency probe text '.repeat(8).slice(0, n), { delay: 25 })
      await page.waitForTimeout(500)
    })
    const { slow, loafs } = await page.evaluate(() => {
      const w = window as unknown as { __ev: Map<number, number>; __loaf: number[] }
      return { slow: [...w.__ev.values()], loafs: w.__loaf }
    })
    // What the app does per keystroke; the §18 target (p99 < 16 ms) applies to these.
    record(`typing_p99_${key}_ms`, quantile(tasks.length ? tasks : [0], 0.99), 'ms')
    // Event Timing: input to the next paint, frame wait included. Headless Chromium presents a
    // frame 1–2 frames late even for a one-line note (DESIGN §22 item 71), so it's tracked here
    // and held to its target by the 120 Hz acceptance run on real hardware (DESIGN §23).
    // Interactions under 16 ms aren't reported: they count as 8 ms (Event Timing's granularity).
    const all = [...slow, ...Array(Math.max(0, n - slow.length)).fill(8)]
    record(`typing_presented_p99_${key}_ms`, quantile(all, 0.99), 'ms')
    record(`typing_loaf_over_50ms_${key}`, loafs.filter((d) => d > 50).length, 'frames')
    await page.keyboard.press('Control+z')
    await page.keyboard.press('Control+z')
  }
})

test('tree: expand a folder with 5,000 children', async () => {
  await page.getByRole('tab', { name: 'Files' }).click()
  const big = page.getByTestId('tree').getByRole('treeitem', { name: 'Big folder', exact: true })
  await big.scrollIntoViewIfNeeded()
  const xs: number[] = []
  for (let i = 0; i < 6; i++) {
    const expanded = (await big.getAttribute('aria-expanded')) === 'true'
    if (expanded) {
      await big.click()
      await expect(big).toHaveAttribute('aria-expanded', 'false')
    }
    xs.push(
      await inputToPaint(
        page,
        () => big.click(),
        () => !!document.querySelector('[role=treeitem][aria-expanded=true]') && [...document.querySelectorAll('[role=treeitem]')].some((e) => e.textContent?.includes('Item 00001')),
        '',
      ),
    )
  }
  record('tree_expand_5000_ms', median(xs.slice(1)), 'ms', xs)
  // Scrolling through the 5,000 (the tree is virtualised): wheel steps at 60 Hz for 2 s.
  if ((await big.getAttribute('aria-expanded')) !== 'true') await big.click()
  const tree = page.getByTestId('tree')
  const tb = (await tree.boundingBox())!
  await page.mouse.move(tb.x + tb.width / 2, tb.y + tb.height / 2)
  const tasks = await traceTasks(page, async () => {
    for (let i = 0; i < 120; i++) {
      await page.mouse.wheel(0, 120)
      await page.waitForTimeout(16)
    }
  })
  record('tree_scroll_p99_ms', quantile(tasks.length ? tasks : [0], 0.99), 'ms')
  await tree.evaluate((t) => (t.scrollTop = 0))
})

test('PDF first page: 5 MB (local) and 100 MB / 500 pages', async () => {
  const xs: number[] = []
  for (let i = 0; i < 4; i++) {
    await open(page, 'Small note')
    await open(page, 'Five MB.pdf')
    await expect.poll(() => page.evaluate(() => performance.getEntriesByName('pdf-first-page').length), { timeout: 30_000 }).toBeGreaterThan(i)
    xs.push(await page.evaluate(() => (performance.getEntriesByName('pdf-first-page').at(-1) as PerformanceMeasure).duration))
  }
  record('pdf_first_page_ms', median(xs.slice(1)), 'ms', xs)

  await open(page, 'Small note')
  const before = await page.evaluate(() => {
    performance.clearMarks('pdf-range')
    return performance.getEntriesByName('pdf-first-page').length
  })
  await open(page, 'Large.pdf')
  await expect.poll(() => page.evaluate(() => performance.getEntriesByName('pdf-first-page').length), { timeout: 60_000 }).toBeGreaterThan(before)
  const r = await page.evaluate(() => {
    const first = performance.getEntriesByName('pdf-first-page').at(-1) as PerformanceMeasure
    const end = first.startTime + first.duration
    const bytes = (performance.getEntriesByName('pdf-range') as PerformanceMark[]).filter((m) => m.startTime <= end).reduce((s, m) => s + (m.detail as number), 0)
    return { ms: first.duration, bytes }
  })
  const size = 100 << 20
  record('large_pdf_first_page_ms', r.ms, 'ms')
  record('large_pdf_read_before_first_page_pct', (r.bytes / size) * 100, '%')
  // Memory after paging through: JS heap (CDP); canvases are bounded by the 2-live-pages rule.
  for (let i = 0; i < 30; i++) await page.keyboard.press('PageDown')
  await page.waitForTimeout(1500)
  const cdp = await ctx.newCDPSession(page)
  await cdp.send('Performance.enable')
  const { metrics } = await cdp.send('Performance.getMetrics')
  record('large_pdf_js_heap_mb', (metrics.find((m) => m.name === 'JSHeapUsedSize')?.value ?? 0) / 1048576, 'MB')
})

test('sync latency between two devices; catch-up after 200 remote changes', async ({ browser, request }) => {
  // The second device keeps attachments on demand (its own download isn't what's measured).
  const ctxB = await browser.newContext()
  await ctxB.addInitScript(() => localStorage.setItem('jess.device', JSON.stringify({ offlineAttachments: 'on-demand' })))
  const b = await ctxB.newPage()
  await login(b)
  // Steady state: B's search index builds for ~40 s after a first sync.
  await expect.poll(() => b.evaluate(() => (window as unknown as { __jess: { indexReady(): boolean } }).__jess.indexReady()), { timeout: 10 * 60_000, intervals: [1000] }).toBe(true)
  await open(page, 'Small note')
  await open(b, 'Small note')
  await expect(b.locator('.cm-content')).toContainText('A short note')
  await page.locator('.cm-content').click()
  await page.keyboard.press('Control+End')
  const tag = Date.now().toString(36)
  await b.evaluate((tag) => {
    const w = window as unknown as { __seen: Map<string, number> }
    w.__seen = new Map()
    const ed = document.querySelector('.cm-content')!
    new MutationObserver(() => {
      for (const m of ed.textContent!.matchAll(new RegExp(`q${tag}_(\\d+)z`, 'g'))) if (!w.__seen.has(m[1])) w.__seen.set(m[1], performance.timeOrigin + performance.now())
    }).observe(ed, { subtree: true, childList: true, characterData: true })
  }, tag)
  await page.evaluate(() => {
    const w = window as unknown as { __sent: number }
    document.addEventListener('keydown', (e) => (w.__sent = performance.timeOrigin + e.timeStamp), { capture: true })
  })
  const lat: number[] = []
  for (let i = 0; i < 40; i++) {
    await page.keyboard.type(` q${tag}_${i}`)
    await page.keyboard.type('z')
    const zSent = await page.evaluate(() => (window as unknown as { __sent: number }).__sent)
    await expect.poll(() => b.evaluate((i) => (window as unknown as { __seen: Map<string, number> }).__seen.get(String(i)) ?? 0, i), { timeout: 10_000, intervals: [20] }).toBeGreaterThan(0)
    const seen = await b.evaluate((i) => (window as unknown as { __seen: Map<string, number> }).__seen.get(String(i))!, i)
    lat.push(seen - zSent)
    await page.waitForTimeout(100)
  }
  record('sync_latency_p50_ms', quantile(lat, 0.5), 'ms')
  record('sync_latency_p99_ms', quantile(lat, 0.99), 'ms')

  // Catch-up: device B is away while 200 notes arrive (an import elsewhere: a create and a text
  // update each), then comes back.
  await ctxB.setOffline(true)
  await b.evaluate(() => window.dispatchEvent(new Event('offline')))
  await expect(status(b)).toHaveText(/Offline/, { timeout: 15_000 })
  const dir = mkdtempSync(join(tmpdir(), 'jess-bench-'))
  const batch = Date.now()
  // Inside a vault folder: the importer takes a zip's single top-level folder as the vault itself.
  mkdirSync(join(dir, 'vault', `Catch up ${batch}`), { recursive: true })
  for (let i = 0; i < 200; i++) writeFileSync(join(dir, 'vault', `Catch up ${batch}`, `Arrived ${i}.md`), `# Arrived ${i}\n\nFrom the other device. [[Small note]]\n`)
  const zip = join(dir, 'v.zip')
  execFileSync('zip', ['-qr', zip, 'vault'], { cwd: dir })
  const bytes = readFileSync(zip)
  const hash = createHash('sha256').update(bytes).digest('hex')
  const token = await page.evaluate(
    () =>
      new Promise<string>((res) => {
        const r = indexedDB.open('jess')
        r.onsuccess = () => {
          const g = r.result.transaction('meta').objectStore('meta').get('token')
          g.onsuccess = () => res(g.result as string)
        }
      }),
  )
  const auth = { authorization: `Bearer ${token}` }
  const up = await (await request.post(`/api/blobs/${hash}/uploads`, { headers: auth, data: { size: bytes.length } })).json()
  if (!up.present) {
    const chunk = bytes.subarray(0, bytes.length)
    await request.put(`/api/blobs/${hash}/uploads/${up.upload_id}/chunks/0`, { headers: { ...auth, 'x-chunk-sha256': createHash('sha256').update(chunk).digest('hex') }, data: Buffer.from(chunk) })
    await request.post(`/api/blobs/${hash}/uploads/${up.upload_id}/complete`, { headers: auth })
  }
  const imp = await request.post('/api/admin/import', { headers: auth, data: { zip_hash: hash } })
  expect(imp.ok()).toBe(true)
  expect((await imp.json()).notes, 'notes imported').toBe(200)
  rmSync(dir, { recursive: true, force: true })
  await page.waitForTimeout(1000)
  await ctxB.setOffline(false)
  // Timed in the page: from "back online" to the new folder in the tree and the status Synced.
  const ms = await b.evaluate(
    (folder) =>
      new Promise<number>((resolve, reject) => {
        const t0 = performance.now()
        setTimeout(() => {
          const items = [...document.querySelectorAll('[role=treeitem]')].slice(0, 20).map((e) => e.textContent?.trim())
          reject(new Error(`catch-up: no ${folder} after 30 s; status ${document.querySelector('[data-testid=sync-status]')?.textContent}; tree ${JSON.stringify(items)}`))
        }, 30_000)
        const done = () =>
          [...document.querySelectorAll('[role=treeitem]')].some((e) => e.textContent?.trim().endsWith(folder)) && /Synced/.test(document.querySelector('[data-testid=sync-status]')?.textContent ?? '')
        const mo = new MutationObserver(() => {
          if (done()) {
            mo.disconnect()
            requestAnimationFrame(() => setTimeout(() => resolve(performance.now() - t0), 0))
          }
        })
        mo.observe(document.body, { subtree: true, childList: true, characterData: true })
        window.dispatchEvent(new Event('online'))
      }),
    `Catch up ${batch}`,
  )
  record('catch_up_200_ms', ms, 'ms')
  await ctxB.close()
})

test.afterAll(async () => {
  await ctx?.close()
})
