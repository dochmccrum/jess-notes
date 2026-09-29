import { test, expect } from '@playwright/test'
import { login, importFixtureVault, MOD } from './helpers'

test.beforeAll(async ({ request }) => {
  await importFixtureVault(request)
})

test('standalone PDF: pages, find, zoom, remembered page', async ({ page }) => {
  await login(page)
  await page.getByTestId('tree').getByRole('treeitem', { name: 'Docs' }).click()
  await page.getByTestId('tree').getByRole('treeitem', { name: 'Paper.pdf' }).click()
  const pane = page.getByTestId('pdf-pane')
  await expect(pane).toBeVisible()
  await expect(pane.getByText('/ 4')).toBeVisible({ timeout: 15_000 })
  await expect(pane.locator('.jess-pdf-page canvas').first()).toBeVisible()
  await expect(pane.locator('.textLayer').first()).toContainText('Paper page 1')
  // Find → jumps to the page and highlights.
  await pane.locator('[data-testid=pdf-scroll]').click()
  await page.keyboard.press(`${MOD}+f`)
  await page.getByTestId('pdf-find').fill('page 3')
  await page.keyboard.press('Enter')
  await expect(page.getByTestId('pdf-found')).toHaveText('1 of 1 pages')
  await expect(page.getByTestId('pdf-page')).toHaveValue('3')
  await expect(pane.locator('.jess-found').first()).toBeVisible()
  // Zoom in, then reload: page and zoom are remembered on this device.
  await pane.getByTitle('Zoom in').click()
  await expect(pane.getByTitle('Fit width')).not.toHaveText('Fit')
  await page.waitForTimeout(500)
  await page.reload()
  await expect(page.getByTestId('pdf-page')).toHaveValue('3', { timeout: 15_000 })
  await expect(page.getByTestId('pdf-pane').getByTitle('Fit width')).not.toHaveText('Fit')
})

test('embedded PDFs: thumbnail then live viewer at the requested page', async ({ page }) => {
  await login(page)
  await page.getByTestId('tree').getByRole('treeitem', { name: 'Welcome' }).click()
  const embeds = page.locator('.cm-content .jess-pdf-embed')
  await expect(embeds.first()).toBeVisible({ timeout: 15_000 })
  // `#page=3&height=600` → a 600 px box showing page 3.
  const tall = embeds.filter({ hasText: 'page 3' }).nth(1)
  await expect(tall).toHaveCSS('height', '600px')
  await tall.scrollIntoViewIfNeeded()
  await expect(tall.locator('.textLayer').first()).toContainText(/Paper page/, { timeout: 15_000 })
  // Never more than 2 live embedded viewers.
  await page.evaluate(() => document.querySelector('.cm-scroller')!.scrollTo(0, 0))
  expect(await page.locator('.jess-pdf-embed-scroll').count()).toBeLessThanOrEqual(2)
  // "Open" goes to the standalone viewer at that page.
  await tall.getByRole('button', { name: 'Open' }).click()
  await expect(page.getByTestId('pdf-page')).toHaveValue('3', { timeout: 15_000 })
})
