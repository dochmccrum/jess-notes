import { test, expect, type Page } from '@playwright/test'
import { execFileSync } from 'node:child_process'
import { mkdtempSync, readFileSync, readdirSync, statSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join, relative } from 'node:path'
import { login, newNote, MOD } from './helpers'

const treeItem = (p: Page, name: string) => p.getByTestId('tree').getByRole('treeitem', { name, exact: true })

test('rename rewrites links on every device', async ({ browser }) => {
  const a = await (await browser.newContext()).newPage()
  const b = await (await browser.newContext()).newPage()
  await login(a)
  await login(b)
  const t = `Planet ${Date.now()}`
  await newNote(a, t)
  const src = `Orbit ${Date.now()}`
  await newNote(a, src)
  await a.keyboard.type(`Link to [[${t}]] here`)
  await expect(a.getByTestId('sync-status')).toHaveText(/Synced/)
  await treeItem(b, src).click()
  await expect(b.locator('.cm-content')).toContainText(`[[${t}]]`)
  // Rename the target on device A from the tree.
  await treeItem(a, t).click({ button: 'right' })
  await a.getByRole('menuitem', { name: 'Rename…' }).click()
  await a.getByTestId('prompt-input').fill(`${t} renamed`)
  await a.keyboard.press('Enter')
  await expect(b.locator('.cm-content')).toContainText(`[[${t} renamed]]`, { timeout: 5000 })
  await expect(treeItem(b, `${t} renamed`)).toBeVisible()
})

test('offline edits sync after reconnect', async ({ browser }) => {
  const ctxA = await browser.newContext()
  const a = await ctxA.newPage()
  const b = await (await browser.newContext()).newPage()
  await login(a)
  await login(b)
  const n = `Offline ${Date.now()}`
  await newNote(a, n)
  await a.keyboard.type('online part.')
  await expect(a.getByTestId('sync-status')).toHaveText(/Synced/)
  await treeItem(b, n).click()
  await expect(b.locator('.cm-content')).toContainText('online part.')
  await ctxA.setOffline(true)
  await a.evaluate(() => window.dispatchEvent(new Event('offline')))
  await expect(a.getByTestId('sync-status')).toHaveText(/Offline|Syncing/, { timeout: 15_000 })
  await a.keyboard.type(' offline part.')
  await b.locator('.cm-content').click()
  await b.keyboard.press(`${MOD}+End`)
  await b.keyboard.type(' concurrent b.')
  await a.waitForTimeout(500)
  await ctxA.setOffline(false)
  await a.evaluate(() => window.dispatchEvent(new Event('online')))
  await expect(a.locator('.cm-content')).toContainText('concurrent b.', { timeout: 15_000 })
  await expect(b.locator('.cm-content')).toContainText('offline part.', { timeout: 15_000 })
  expect(await a.locator('.cm-content').innerText()).toBe(await b.locator('.cm-content').innerText())
})

test('trash and restore', async ({ page }) => {
  await login(page)
  const n = `Doomed ${Date.now()}`
  await newNote(page, n)
  await page.keyboard.type('keep me')
  await treeItem(page, n).click({ button: 'right' })
  await page.getByRole('menuitem', { name: 'Delete' }).click()
  await expect(treeItem(page, n)).toHaveCount(0)
  await page.keyboard.press(`${MOD}+p`)
  await expect(page.getByRole('dialog')).toBeVisible()
  await page.keyboard.type('open trash')
  await page.keyboard.press('Enter')
  const dlg = page.getByRole('dialog', { name: 'Trash' })
  await dlg.getByRole('button', { name: 'Restore' }).first().click()
  await dlg.getByRole('button', { name: 'Close' }).click()
  await treeItem(page, n).click()
  await expect(page.locator('.cm-content')).toContainText('keep me')
})

test('full-text search', async ({ page }) => {
  await login(page)
  const word = `quux${Date.now()}`
  await newNote(page, `Searchable ${Date.now()}`)
  await page.keyboard.type(`some ${word} text`)
  await page.keyboard.press(`${MOD}+Shift+f`)
  await page.getByTestId('search-input').fill(word)
  await expect(page.getByTestId('search-results').locator('li').first()).toContainText(word, { timeout: 10_000 })
})

test('sidebar modes: shortcut and hover-reveal', async ({ page }) => {
  await login(page)
  await page.getByTestId('open-settings').click()
  await page.getByTestId('sidebar-mode').selectOption('hover')
  await page.keyboard.press('Escape')
  const sb = page.getByTestId('sidebar')
  // The shortcut toggles it in every mode.
  await page.keyboard.press(`${MOD}+\\`)
  const box = () => sb.boundingBox()
  await expect.poll(async () => (await box())!.x + (await box())!.width).toBeLessThanOrEqual(1)
  // Brief pass through the hot zone doesn't open it (100 ms intent).
  await page.mouse.move(400, 300)
  await page.mouse.move(2, 300)
  await page.mouse.move(400, 300)
  await page.waitForTimeout(250)
  expect((await box())!.x + (await box())!.width).toBeLessThanOrEqual(1)
  // Dwelling opens it.
  await page.mouse.move(2, 300)
  await expect.poll(async () => (await box())!.x, { timeout: 2000 }).toBe(0)
  // Leaving it closes after the grace period, not before.
  await page.mouse.move(100, 300)
  await page.mouse.move(900, 300)
  await page.waitForTimeout(120)
  expect((await box())!.x).toBe(0)
  await expect.poll(async () => (await box())!.x + (await box())!.width, { timeout: 2000 }).toBeLessThanOrEqual(1)
})

test('import a vault zip, export it: byte-for-byte round trip', async ({ page }) => {
  test.setTimeout(120_000)
  const vault = new URL('../../tests/fixtures/vault', import.meta.url).pathname
  const tmp = mkdtempSync(join(tmpdir(), 'jess-rt-'))
  const zip = join(tmp, 'vault.zip')
  execFileSync('python3', ['-c', `
import os, sys, zipfile
root, out = sys.argv[1], sys.argv[2]
with zipfile.ZipFile(out, 'w', zipfile.ZIP_DEFLATED) as z:
    for d, _, fs in os.walk(root):
        for f in fs:
            p = os.path.join(d, f)
            z.write(p, os.path.join('vault', os.path.relpath(p, root)))
`, vault, zip])
  await page.addInitScript(() => {
    ;(window as unknown as Record<string, unknown>).showSaveFilePicker = undefined
  })
  await login(page)
  await page.getByTestId('open-settings').click()
  await page.getByRole('button', { name: 'Import / export…' }).click()
  await page.getByTestId('import-zip').setInputFiles(zip)
  await expect(page.getByTestId('import-report')).toBeVisible({ timeout: 30_000 })
  await page.getByTestId('import-run').click()
  await expect(page.getByRole('dialog').getByText('Import finished.')).toBeVisible({ timeout: 60_000 })
  await expect(page.getByTestId('sync-status')).toHaveText(/Synced/, { timeout: 60_000 })
  const dl = page.waitForEvent('download', { timeout: 60_000 })
  await page.getByTestId('export-zip').click()
  const file = join(tmp, 'export.zip')
  await (await dl).saveAs(file)
  const outDir = join(tmp, 'out')
  execFileSync('python3', ['-c', 'import sys, zipfile; zipfile.ZipFile(sys.argv[1]).extractall(sys.argv[2])', file, outDir])
  const walk = (d: string, base = d): string[] => readdirSync(d).flatMap((n) => (statSync(join(d, n)).isDirectory() ? walk(join(d, n), base) : [relative(base, join(d, n))]))
  const skipped = (p: string) => /(^|\/)(\.obsidian|\.git|\.trash)(\/|$)|(^|\/)\.DS_Store$/.test(p)
  const want = walk(vault).filter((p) => !skipped(p)).sort()
  // Other tests share this server, so the export may hold their notes too.
  const got = new Set(walk(outDir))
  expect(want.filter((p) => !got.has(p))).toEqual([])
  const differ = want.filter((p) => !readFileSync(join(outDir, p)).equals(readFileSync(join(vault, p))))
  expect(differ).toEqual([])
})
