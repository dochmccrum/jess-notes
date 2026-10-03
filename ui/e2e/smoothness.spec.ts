import { test, expect, type Page } from '@playwright/test'
import { login, newNote, pasteText, MOD } from './helpers'

// 120 Hz smoothness (DESIGN §23). Each interaction is measured two ways:
//   - real frame deltas (`__jessFrames.frameStats`): the acceptance check, asserted on a
//     high-refresh display (`E2E_HZ=120` with `--headed`: ≤ 1 % late frames), logged otherwise;
//   - main-thread tasks from a Chrome trace (only the `toplevel` and `blink` categories: the
//     timeline ones doubled the tasks' cost): what makes frames late on any display, so it's what
//     headless CI asserts on, by kind of interaction:
//       typing in text: the p95 task fits one 120 Hz frame and the longest fits two (a run has
//       only 50–90 tasks, so a p99 would just be the single slowest keystroke);
//       rendering-heavy ones (typing beside maths, scrolling, PDFs, one-off actions like opening
//       the switcher): headless Chromium paints and rasterises in software, so these cost 2–3×
//       what they do on a real GPU (maths typing: 28 of 106 tasks over 8.3 ms headless, 1 of 73
//       headed). Headless, they only guard against regressions (longest task within six
//       frames); on a real display the late-frame check above is what holds them to 120 Hz.
//     Chromium only: other engines have no trace API, so WebKit logs the heartbeat's main-thread
//     stretches instead.
// Playwright's tracing is off here: its DOM snapshots run in the page on every action.

type Busy = { p99: number; max: number; over120: number; stretches: number[] }
type Frames = { frames: number; interval: number; p99: number; max: number; late: number }

const HZ = Number(process.env.E2E_HZ ?? 0)
const FRAME = 1000 / 120
// Typing: p95 task within one 120 Hz frame, longest within two (at most one frame dropped).
// GitHub's runners are 2–3× slower than a desktop and share their CPU: they get 2.5× headroom
// (CI guards against regressions; the 120 Hz acceptance runs on real hardware, DESIGN §23.1).
const SLACK = process.env.GITHUB_ACTIONS ? 2.5 : 1

test.use({ trace: 'off' })

/** Top-level main-thread tasks in a Chrome trace: durations in ms, longest first. */
function mainThreadTasks(json: Buffer): number[] {
  const t = JSON.parse(json.toString())
  const ev = (t.traceEvents ?? t) as { name: string; ph: string; ts: number; dur?: number; pid: number; tid: number }[]
  // The renderer's main thread: where the frames start.
  const n = new Map<string, number>()
  for (const e of ev) if (e.name === 'WebFrameWidgetImpl::BeginMainFrame') n.set(`${e.pid}:${e.tid}`, (n.get(`${e.pid}:${e.tid}`) ?? 0) + 1)
  const main = [...n].sort((a, b) => b[1] - a[1])[0]?.[0]
  const all = ev.filter((e) => `${e.pid}:${e.tid}` === main && e.ph === 'X' && e.dur && (e.name === 'ThreadControllerImpl::RunTask' || e.name === 'RunTask'))
  // A task nested in another (or the same task under two names) counts once.
  const out: number[] = []
  let end = -1
  for (const e of all.sort((a, b) => a.ts - b.ts)) {
    if (e.ts < end) continue
    out.push(e.dur! / 1000)
    end = e.ts + e.dur!
  }
  return out.sort((a, b) => b - a)
}

async function measured(page: Page, name: string, kind: 'typing' | 'rendering', action: () => Promise<void>) {
  const browser = page.context().browser()!
  const chromium = browser.browserType().name() === 'chromium'
  // The heartbeat keeps the main thread busy with tiny tasks: not while tracing.
  await page.evaluate((heartbeat) => {
    const f = (window as unknown as { __jessFrames: { busyStats(): () => unknown; frameStats(): () => unknown } }).__jessFrames
    const b = heartbeat ? f.busyStats() : () => ({ p99: 0, max: 0, over120: 0, stretches: [] })
    ;(window as unknown as { __stop: () => unknown }).__stop = ((r) => () => ({ busy: b(), frames: r() }))(f.frameStats())
  }, !chromium)
  if (chromium) await browser.startTracing(page, { categories: ['toplevel', 'blink'] })
  await action()
  const tasks = chromium ? mainThreadTasks(await browser.stopTracing()) : []
  const { busy, frames } = (await page.evaluate(() => (window as unknown as { __stop: () => unknown }).__stop())) as { busy: Busy; frames: Frames }
  // Only tasks that did something: the idle scheduler's sub-millisecond tasks would dilute the p99.
  const real = tasks.filter((t) => t >= 1)
  const p99 = real[Math.floor(real.length * 0.01)] ?? 0
  const p95 = real[Math.floor(real.length * 0.05)] ?? 0
  console.log(
    `${name}: ` +
      (chromium ? `tasks p95 ${p95.toFixed(1)}, p99 ${p99.toFixed(1)} ms, longest ${(real[0] ?? 0).toFixed(1)} ms (${real.filter((w) => w > FRAME).length}/${real.length} over 8.3) | ` : '') +
      `frames ${frames.frames} @ ${frames.interval.toFixed(1)} ms, p99 ${frames.p99.toFixed(1)}, max ${frames.max.toFixed(1)}, ${frames.late} late` +
      (chromium ? '' : ` | stretches p99 ${busy.p99.toFixed(1)}, max ${busy.max.toFixed(1)}`),
  )
  if (chromium) {
    expect(tasks.length, `${name}: tasks in the trace`).toBeGreaterThan(5)
    if (kind === 'typing') {
      expect.soft(p95, `${name}: p95 main-thread task`).toBeLessThanOrEqual(FRAME * SLACK)
      expect.soft(real[0] ?? 0, `${name}: longest main-thread task`).toBeLessThanOrEqual(2 * FRAME * SLACK)
    } else {
      expect.soft(real[0] ?? 0, `${name}: longest main-thread task`).toBeLessThanOrEqual(6 * FRAME * SLACK)
    }
  }
  if (HZ) {
    // On a real high-refresh display: the app keeps up with it.
    expect.soft(frames.interval, `${name}: refresh interval`).toBeLessThan(1000 / HZ + 0.5)
    expect.soft(frames.late / Math.max(1, frames.frames), `${name}: share of late frames`).toBeLessThanOrEqual(0.01)
  }
}

/** Markdown with the usual mix: headings, links, tags, lists, code. About `kb` KB. */
function bulk(kb: number) {
  const para = (i: number) =>
    `## Section ${i}\n\nSome text with a [[Link ${i % 40}]] and a #tag${i % 7}, **bold**, _italic_ and \`code\`. ` +
    `More words follow so the line wraps across the editor width like a real paragraph would.\n\n- item one\n- item [[two]]\n\n`
  let s = ''
  for (let i = 0; s.length < kb * 1024; i++) s += para(i)
  return s
}

test.setTimeout(180_000)

test('@frames typing in a large note', async ({ page }) => {
  await login(page)
  await newNote(page, `Large ${Date.now()}`)
  await pasteText(page, bulk(1024)) // 1 MB (SPEC: no dropped frames in a 1 MB note)
  await page.keyboard.press(`${MOD}+Home`)
  await expect(page.getByTestId('sync-status')).toHaveText(/Synced/, { timeout: 60_000 })
  await page.waitForTimeout(500)
  await measured(page, 'type 1 MB note', 'typing', () => page.keyboard.type('the quick brown fox jumps over the lazy dog '.repeat(3), { delay: 25 }))
  await page.keyboard.press(`${MOD}+End`)
  await page.waitForTimeout(300)
  // At the end of a 1 MB note each keystroke also pays the markdown parser's reuse of every block
  // above it (DESIGN §23.3): ~5 ms here, 38 ms p95 on CI runners. On a real 120 Hz display it has
  // no late frames, so headless only guards it against regressions (the `E2E_HZ` check still holds).
  await measured(page, 'type at end of 1 MB note', 'rendering', () => page.keyboard.type(' and then some more words at the end', { delay: 25 }))
})

test('@frames typing next to rendered maths', async ({ page }) => {
  await login(page)
  await newNote(page, `Maths ${Date.now()}`)
  let s = ''
  for (let i = 0; i < 150; i++) s += `Line ${i}: $e^{i\\pi} + ${i} = \\sum_{k=0}^{n} \\frac{x^k}{k!}$ and $$\\int_0^1 f(x)\\,dx = ${i}$$\n\n`
  await pasteText(page, s)
  await page.keyboard.press(`${MOD}+Home`)
  await expect(page.locator('.cm-content .katex').first()).toBeVisible({ timeout: 10_000 })
  await page.waitForTimeout(500)
  await measured(page, 'type in maths note', 'rendering', () => page.keyboard.type('typing between formulas is smooth ', { delay: 25 }))
})

test('@frames scrolling a long note', async ({ page }) => {
  await login(page)
  await newNote(page, `Scroll ${Date.now()}`)
  await pasteText(page, bulk(300))
  await page.keyboard.press(`${MOD}+Home`)
  await page.waitForTimeout(500)
  const box = (await page.locator('.cm-scroller').boundingBox())!
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2)
  await measured(page, 'scroll long note', 'rendering', async () => {
    for (let i = 0; i < 90; i++) {
      await page.mouse.wheel(0, 120)
      await page.waitForTimeout(8)
    }
  })
})

test('@frames sidebar toggle and quick switcher', async ({ page }) => {
  await login(page)
  for (let i = 0; i < 30; i++) await newNote(page, `Switch ${i} ${Date.now()}`)
  await page.waitForTimeout(500)
  await measured(page, 'sidebar toggle', 'rendering', async () => {
    for (let i = 0; i < 6; i++) {
      await page.getByRole('button', { name: 'Toggle sidebar' }).first().click()
      await page.waitForTimeout(250) // the 150 ms slide plus settle
    }
  })
  await measured(page, 'quick switcher', 'rendering', async () => {
    await page.keyboard.press(`${MOD}+o`)
    await expect(page.getByRole('dialog')).toBeVisible()
    await page.keyboard.type('Switch 1', { delay: 40 })
    await page.keyboard.press('Escape')
  })
})

test('@frames scrolling a PDF', async ({ page }) => {
  const { execFileSync } = await import('node:child_process')
  const { mkdtempSync, readFileSync } = await import('node:fs')
  const { tmpdir } = await import('node:os')
  const { join } = await import('node:path')
  await login(page)
  const pdfPath = join(mkdtempSync(join(tmpdir(), 'jess-pdf-')), `Scroll paper ${Date.now()}.pdf`)
  execFileSync('python3', [new URL('../../tests/fixtures/make_pdf.py', import.meta.url).pathname, pdfPath, '40', '5'])
  const name = pdfPath.split('/').pop()!
  await newNote(page, `PDF holder ${Date.now()}`)
  await page.locator('.cm-content').evaluate(
    async (el, [bytes, n]) => {
      const dt = new DataTransfer()
      dt.items.add(new File([new Uint8Array(bytes as number[])], n as string, { type: 'application/pdf' }))
      el.dispatchEvent(new ClipboardEvent('paste', { clipboardData: dt, bubbles: true, cancelable: true }))
    },
    [[...readFileSync(pdfPath)], name],
  )
  await page.keyboard.press(`${MOD}+o`)
  await page.keyboard.type(name.slice(0, -6))
  await expect(page.getByRole('option').first()).toContainText(name, { timeout: 30_000 })
  await page.keyboard.press('Enter')
  await expect.poll(() => page.evaluate(() => performance.getEntriesByName('pdf-first-page').length), { timeout: 20_000 }).toBe(1)
  await page.waitForTimeout(1000)
  const box = (await page.getByTestId('pdf-pane').boundingBox())!
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2)
  await measured(page, 'scroll PDF', 'rendering', async () => {
    for (let i = 0; i < 90; i++) {
      await page.mouse.wheel(0, 150)
      await page.waitForTimeout(8)
    }
  })
})
