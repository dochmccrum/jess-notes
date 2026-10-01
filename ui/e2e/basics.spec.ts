import { test, expect } from '@playwright/test'
import { login, newNote, MOD } from './helpers'

test('create a note, type, reload: text persists and syncs', async ({ page }) => {
  await login(page)
  const name = `Alpha ${Date.now()}`
  await newNote(page, name)
  await page.keyboard.type('Hello from e2e')
  await expect(page.getByTestId('sync-status')).toHaveText(/Synced/, { timeout: 5000 })
  await page.reload()
  await expect(page.locator('.cm-content')).toContainText('Hello from e2e')
})

test('edits reach a second device quickly', async ({ browser }) => {
  const a = await (await browser.newContext()).newPage()
  const b = await (await browser.newContext()).newPage()
  await login(a)
  await login(b)
  const name = `Shared ${Date.now()}`
  await newNote(a, name)
  await a.keyboard.type('first line')
  await b.getByTestId('tree').getByText(name, { exact: true }).click()
  await expect(b.locator('.cm-content')).toContainText('first line')
  const t0 = Date.now()
  await a.keyboard.type(' plus more')
  await expect(b.locator('.cm-content')).toContainText('first line plus more', { timeout: 3000 })
  console.log(`propagation ≈ ${Date.now() - t0} ms`)
  await b.keyboard.press(`${MOD}+End`)
  await b.keyboard.type(' and b')
  await expect(a.locator('.cm-content')).toContainText('and b')
})

test('quick switcher and command palette', async ({ page }) => {
  await login(page)
  const name = `Zebra ${Date.now()}`
  await newNote(page, name)
  await page.keyboard.type('zzz')
  await page.getByTestId('new-note').click()
  await page.keyboard.press(`${MOD}+o`)
  await expect(page.getByRole('dialog')).toBeVisible()
  await page.keyboard.type(name.slice(0, -2))
  // Enter acts on the top result: wait for it to be this note, not a result of the partial query.
  await expect(page.getByRole('option').first()).toContainText(name)
  await page.keyboard.press('Enter')
  await expect(page.locator('.cm-content')).toContainText('zzz')
  await page.keyboard.press(`${MOD}+p`)
  await expect(page.getByRole('dialog')).toBeVisible()
  await page.keyboard.type('toggle sidebar')
  await page.keyboard.press('Enter')
  await expect(page.getByTestId('sidebar')).toBeHidden()
})

test('wikilink autocomplete, follow, backlinks', async ({ page }) => {
  await login(page)
  const target = `Target ${Date.now()}`
  await newNote(page, target)
  await page.keyboard.type('I am the target')
  await newNote(page, `Source ${Date.now()}`)
  // A partial name that only this run's note matches (the vault may hold earlier runs' notes).
  await page.keyboard.type(`See [[${target.slice(0, -2)}`)
  await expect(page.locator('.cm-tooltip-autocomplete li').first()).toContainText(target)
  await page.waitForTimeout(100) // CodeMirror ignores Enter for 75 ms after the list changes
  await page.keyboard.press('Enter')
  await expect(page.locator('.cm-content')).toContainText(`[[${target}]]`)
  await page.keyboard.press(`${MOD}+Shift+b`)
  await page.locator('.cm-content').press('End')
  await page.keyboard.type('\n')
  await page.locator('.cm-link-widget').first().click()
  await expect(page.locator('.cm-content')).toContainText('I am the target')
  await expect(page.getByRole('region', { name: 'Backlinks' })).toContainText('Source')
})

test('a second tab is gated', async ({ context }) => {
  const p1 = await context.newPage()
  await login(p1)
  const p2 = await context.newPage()
  await p2.goto('/')
  await expect(p2.getByTestId('blocked')).toBeVisible()
  await p2.getByRole('button', { name: 'Use here' }).click()
  await expect(p2.getByTestId('sync-status')).toBeVisible({ timeout: 15_000 })
  await expect(p1.getByTestId('blocked')).toBeVisible({ timeout: 15_000 })
})

test('erase this device: local data gone, vault intact on the server', async ({ page }) => {
  await login(page)
  const name = `Survivor ${Date.now()}`
  await newNote(page, name)
  await page.keyboard.type('still here')
  await expect(page.getByTestId('sync-status')).toHaveText(/Synced/)
  await page.getByTestId('open-settings').click()
  await page.getByTestId('erase').click()
  await page.getByTestId('erase-confirm').click()
  await expect(page.getByTestId('password')).toBeVisible({ timeout: 15_000 })
  await login(page)
  await page.getByTestId('tree').getByText(name, { exact: true }).click()
  await expect(page.locator('.cm-content')).toContainText('still here')
})
