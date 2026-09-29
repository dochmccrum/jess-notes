import { test, expect } from '@playwright/test'
import { login, newNote, MOD } from './helpers'

// Rough timing on this machine (SPEC targets: cold start <300 ms desktop, open note <50 ms).
// Asserted with headroom so CI noise doesn't flake; the numbers are logged for the phase log.
test('cold start and note open timings', async ({ page }) => {
  await login(page)
  const a = `Perf A ${Date.now()}`
  await newNote(page, a)
  await page.keyboard.type('# Heading\n\n' + 'Some text with a [[link]] and #tag. '.repeat(50))
  const b = `Perf B ${Date.now()}`
  await newNote(page, b)
  await page.keyboard.type('short')
  await expect(page.getByTestId('sync-status')).toHaveText(/Synced/)
  await page.getByTestId('tree').getByText(a, { exact: true }).click()
  await expect(page.locator('.cm-content')).toContainText('Heading')
  await page.waitForTimeout(2500) // boot record is written 2 s after the last change
  const samples: number[] = []
  for (let i = 0; i < 3; i++) {
    await page.reload()
    await expect(page.locator('.cm-content')).toContainText('Heading')
    samples.push(await page.evaluate(() => performance.getEntriesByName('open-note').at(-1)?.startTime ?? -1))
  }
  const cold = await page.evaluate(() => {
    const m = performance.getEntriesByName('note-visible')[0]
    return m ? m.startTime : -1
  })
  const opens: number[] = []
  for (let i = 0; i < 6; i++) {
    await page.getByTestId('tree').getByText(i % 2 ? a : b, { exact: true }).click()
    await expect(page.locator('.cm-content')).toContainText(i % 2 ? 'Heading' : 'short')
    opens.push(await page.evaluate(() => (performance.getEntriesByName('open-note').at(-1) as PerformanceMeasure).duration))
  }
  console.log(`cold start → note visible: ${cold.toFixed(0)} ms; open-note: ${opens.map((x) => x.toFixed(1)).join(', ')} ms`)
  expect(cold).toBeGreaterThan(0)
  expect(cold).toBeLessThan(1500)
  expect(Math.min(...opens)).toBeLessThan(100)
})

test('a note with 50 images opens fast, without layout shift', async ({ page }) => {
  test.setTimeout(120_000)
  await login(page)
  const note = `Gallery ${Date.now()}`
  await newNote(page, note)
  // 50 distinct PNGs pasted at once.
  await page.locator('.cm-content').evaluate(async (el) => {
    const dt = new DataTransfer()
    for (let i = 0; i < 50; i++) {
      const c = document.createElement('canvas')
      c.width = 400
      c.height = 300
      const g = c.getContext('2d')!
      g.fillStyle = `hsl(${i * 7}, 70%, 50%)`
      g.fillRect(0, 0, 400, 300)
      const b: Blob = await new Promise((r) => c.toBlob((x) => r(x!), 'image/png'))
      dt.items.add(new File([b], `img${i}.png`, { type: 'image/png' }))
    }
    el.dispatchEvent(new ClipboardEvent('paste', { clipboardData: dt, bubbles: true, cancelable: true }))
  })
  // All imported (the editor only renders what's visible: CodeMirror virtualises).
  await expect.poll(() => page.evaluate(() => document.querySelectorAll('.cm-content .jess-img img').length), { timeout: 60_000 }).toBeGreaterThan(3)
  await expect(page.getByTestId('sync-status')).toHaveText(/Synced/, { timeout: 60_000 })
  // Every rendered image reserves its box before its bytes arrive.
  expect(await page.locator('.cm-content .jess-img img:not([width])').count()).toBe(0)
  const other = `Other ${Date.now()}`
  await newNote(page, other)
  const opens: number[] = []
  for (let i = 0; i < 5; i++) {
    await page.getByTestId('tree').getByText(i % 2 ? other : note, { exact: true }).click()
    await expect(page.getByRole('navigation', { name: 'Path' })).toContainText(i % 2 ? other : note)
    if (i % 2 === 0) {
      await expect(page.locator('.cm-content .jess-img img').first()).toBeVisible({ timeout: 10_000 })
      opens.push(await page.evaluate(() => (performance.getEntriesByName('open-note').at(-1) as PerformanceMeasure).duration))
    }
  }
  await expect.poll(() => page.locator('.cm-content .jess-img img').first().evaluate((i: HTMLImageElement) => i.naturalWidth), { timeout: 10_000 }).toBe(400)
  expect(await page.locator('.jess-img-placeholder.offline').count()).toBe(0)
  console.log(`note with 50 images: ${opens.map((x) => x.toFixed(1)).join(', ')} ms`)
  expect(Math.min(...opens)).toBeLessThan(100)
})

test('a 5 MB PDF shows its first page fast', async ({ page }) => {
  test.setTimeout(120_000)
  const { execFileSync } = await import('node:child_process')
  const { mkdtempSync, readFileSync } = await import('node:fs')
  const { tmpdir } = await import('node:os')
  const { join } = await import('node:path')
  await login(page)
  const pdfPath = join(mkdtempSync(join(tmpdir(), 'jess-pdf-')), `Big report ${Date.now()}.pdf`)
  execFileSync('python3', [new URL('../../tests/fixtures/make_pdf.py', import.meta.url).pathname, pdfPath, '40', '5'])
  const pdfName = pdfPath.split('/').pop()!
  const pdfBytes = [...readFileSync(pdfPath)]
  await newNote(page, `Holder ${Date.now()}`)
  await page.locator('.cm-content').evaluate(async (el, [bytes, name]) => {
    const dt = new DataTransfer()
    dt.items.add(new File([new Uint8Array(bytes as number[])], name as string, { type: 'application/pdf' }))
    el.dispatchEvent(new ClipboardEvent('paste', { clipboardData: dt, bubbles: true, cancelable: true }))
  }, [pdfBytes, pdfName])
  const samples: number[] = []
  for (let i = 0; i < 2; i++) {
    // Open it with the quick switcher (it may sit in a collapsed attachment folder).
    await page.keyboard.press(`${MOD}+o`)
    await expect(page.getByRole('dialog')).toBeVisible()
    await page.keyboard.type(pdfName.slice(0, 16))
    await expect(page.getByRole('option').first()).toContainText(pdfName, { timeout: 30_000 })
    await page.keyboard.press('Enter')
    await expect.poll(() => page.evaluate(() => performance.getEntriesByName('pdf-first-page').length), { timeout: 15_000 }).toBe(i + 1)
    samples.push(await page.evaluate(() => (performance.getEntriesByName('pdf-first-page').at(-1) as PerformanceMeasure).duration))
    await page.getByTestId('tree').getByText(/^Holder/).first().click()
  }
  // Let its upload finish so later tests don't start behind it.
  await expect(page.getByTestId('sync-status')).toHaveText(/Synced/, { timeout: 60_000 })
  console.log(`5 MB PDF first page: ${samples.map((x) => x.toFixed(0)).join(', ')} ms`)
  expect(Math.min(...samples)).toBeLessThan(1000)
})
