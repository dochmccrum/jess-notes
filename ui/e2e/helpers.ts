import { expect, type Page } from '@playwright/test'

export const PASSWORD = 'correct horse battery'
export const MOD = process.platform === 'darwin' ? 'Meta' : 'Control'

export async function login(page: Page) {
  await page.goto('/')
  await page.getByTestId('password').fill(PASSWORD)
  await page.getByRole('button', { name: 'Sign in' }).click()
  await expect(page.getByTestId('sync-status')).toHaveText(/Synced/, { timeout: 15_000 })
}

export async function newNote(page: Page, name: string) {
  const before = await page.evaluate(() => location.hash)
  await page.getByTestId('new-note').click()
  // Wait for the *new* note to be open (not the previous one's editor).
  await expect.poll(() => page.evaluate(() => location.hash)).not.toBe(before)
  await expect(page.getByRole('navigation', { name: 'Path' })).toHaveText(/Untitled/)
  const editor = page.locator('.cm-content')
  await expect(editor).toBeVisible()
  await page.keyboard.press('F2')
  await page.getByTestId('prompt-input').fill(name)
  await page.keyboard.press('Enter')
  await expect(page.getByTestId('tree').getByText(name, { exact: true })).toBeVisible()
  await editor.click()
}

export async function editorText(page: Page) {
  return page.locator('.cm-content').innerText()
}

/** Imports tests/fixtures/vault server-side (admin API). Idempotent: identical files are skipped. */
export async function importFixtureVault(request: import('@playwright/test').APIRequestContext) {
  const { execFileSync } = await import('node:child_process')
  const { mkdtempSync, readFileSync } = await import('node:fs')
  const { tmpdir } = await import('node:os')
  const { join } = await import('node:path')
  const { createHash } = await import('node:crypto')
  const vault = new URL('../../tests/fixtures/vault', import.meta.url).pathname
  const zip = join(mkdtempSync(join(tmpdir(), 'jess-fx-')), 'v.zip')
  execFileSync('python3', ['-c', `
import os, sys, zipfile
root, out = sys.argv[1], sys.argv[2]
with zipfile.ZipFile(out, 'w', zipfile.ZIP_DEFLATED) as z:
    for d, _, fs in os.walk(root):
        for f in fs:
            p = os.path.join(d, f)
            z.write(p, os.path.relpath(p, root))
`, vault, zip])
  const bytes = readFileSync(zip)
  const hash = createHash('sha256').update(bytes).digest('hex')
  const tok = (await (await request.post('/api/auth/login', { data: { password: PASSWORD, device_name: 'e2e-import' } })).json()).token as string
  const auth = { authorization: `Bearer ${tok}` }
  const begin = await (await request.post(`/api/blobs/${hash}/uploads`, { headers: auth, data: { size: bytes.length } })).json()
  if (!begin.present) {
    const cs = begin.chunk_size as number
    for (let i = 0; i * cs < bytes.length; i++) {
      const c = bytes.subarray(i * cs, (i + 1) * cs)
      const r = await request.put(`/api/blobs/${hash}/uploads/${begin.upload_id}/chunks/${i}`, { headers: { ...auth, 'x-chunk-sha256': createHash('sha256').update(c).digest('hex') }, data: c })
      if (!r.ok()) throw new Error(`chunk ${r.status()}`)
    }
    const r = await request.post(`/api/blobs/${hash}/uploads/${begin.upload_id}/complete`, { headers: auth })
    if (!r.ok()) throw new Error(`complete ${r.status()}`)
  }
  const r = await request.post('/api/admin/import', { headers: auth, data: { zip_hash: hash } })
  if (!r.ok()) throw new Error(`import ${r.status()} ${await r.text()}`)
}

/**
 * Puts `text` into the editor as a paste, the way a user brings in a big note. (Playwright's
 * `insertText` goes through Blink's contenteditable editing, which handles every newline as its
 * own paragraph insert: quadratic, 80 s for 200 KB, while CodeMirror takes a 1 MB paste in 30 ms.)
 */
export async function pasteText(page: Page, text: string) {
  await page.locator('.cm-content').evaluate((el, t) => {
    const dt = new DataTransfer()
    dt.setData('text/plain', t)
    el.dispatchEvent(new ClipboardEvent('paste', { clipboardData: dt, bubbles: true, cancelable: true }))
  }, text)
}
