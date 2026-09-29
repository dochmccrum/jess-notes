import { test, expect } from '@playwright/test'
import { spawn } from 'node:child_process'
import { mkdtempSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { MOD } from './helpers'

// First run against a fresh server with no JESS_ADMIN_PASSWORD: the setup code from the log.
test('first-run setup with the setup code, then maths renders', async ({ page }) => {
  const port = 18900 + Math.floor(Math.random() * 90)
  const bin = process.env.JESS_BIN ?? '../target/debug/jess'
  const child = spawn(bin, ['serve'], {
    env: { ...process.env, JESS_DATA_DIR: mkdtempSync(join(tmpdir(), 'jess-setup-')), PORT: String(port), JESS_UI_DIR: 'dist', RUST_LOG: 'info', JESS_ADMIN_PASSWORD: '' },
  })
  try {
    const code = await new Promise<string>((resolve, reject) => {
      let log = ''
      const on = (d: Buffer) => {
        log += d.toString()
        const m = /SETUP CODE: (\S+)/.exec(log)
        if (m) resolve(m[1])
      }
      child.stdout.on('data', on)
      child.stderr.on('data', on)
      setTimeout(() => reject(new Error(`no setup code in log:\n${log}`)), 15_000)
    })
    const base = `http://127.0.0.1:${port}`
    await page.goto(base)
    await page.getByTestId('setup-code').fill('wrong-code')
    await page.getByTestId('password').fill('a good password')
    await page.getByTestId('confirm').fill('a good password')
    await page.getByRole('button', { name: 'Create vault' }).click()
    await expect(page.getByRole('alert')).toBeVisible()
    await page.getByTestId('setup-code').fill(code)
    await page.getByRole('button', { name: 'Create vault' }).click()
    await expect(page.getByTestId('sync-status')).toHaveText(/Synced/, { timeout: 15_000 })

    // Maths: rendered by KaTeX once the cursor leaves it; currency stays text.
    await page.getByTestId('new-note').click()
    await page.locator('.cm-content').click()
    await page.keyboard.type('Energy $E=mc^2$ costs $5 today.\n\n$$\n\\int_0^1 x\\,dx\n$$\n\nend')
    await expect(page.locator('.cm-content .katex').first()).toBeVisible({ timeout: 10_000 })
    await expect(page.locator('.cm-content .katex-display')).toBeVisible()
    await expect(page.locator('.cm-content')).toContainText('$5 today')
    // With the cursor back inside the inline maths, its source is shown for editing.
    await page.locator('.cm-content .katex').first().click()
    await expect(page.locator('.cm-content')).toContainText('$E=mc^2$')
  } finally {
    child.kill('SIGTERM')
  }
})
