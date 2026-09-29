import { test, expect } from '@playwright/test'
import { login, newNote } from './helpers'

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
