import { test, expect, type Page } from '@playwright/test'
import { login, newNote } from './helpers'

/** A solid-colour PNG of the given size, made in the page. */
async function pastePng(page: Page, w: number, h: number) {
  await page.locator('.cm-content').evaluate(
    async (el, [w, h]) => {
      const c = document.createElement('canvas')
      c.width = w
      c.height = h
      const g = c.getContext('2d')!
      g.fillStyle = '#c0392b'
      g.fillRect(0, 0, w, h)
      const blob: Blob = await new Promise((r) => c.toBlob((b) => r(b!), 'image/png'))
      const dt = new DataTransfer()
      dt.items.add(new File([blob], 'image.png', { type: 'image/png' }))
      el.dispatchEvent(new ClipboardEvent('paste', { clipboardData: dt, bubbles: true, cancelable: true }))
    },
    [w, h],
  )
}

test('paste an image: Obsidian name, inline at its size, not in the tree, on the other device', async ({ browser }) => {
  const a = await (await browser.newContext()).newPage()
  const b = await (await browser.newContext()).newPage()
  await login(a)
  await login(b)
  const note = `Photos ${Date.now()}`
  await newNote(a, note)
  await a.keyboard.type('Before\n')
  await pastePng(a, 320, 200)
  await a.keyboard.type('After')
  await expect(a.locator('.cm-content')).toContainText('After')
  const img = a.locator('.cm-content .jess-img img')
  await expect(img).toBeVisible({ timeout: 10_000 })
  expect(await img.getAttribute('width')).toBe('320')
  expect(await img.getAttribute('height')).toBe('200')
  await expect.poll(() => img.evaluate((i: HTMLImageElement) => i.naturalWidth), { timeout: 10_000 }).toBe(320)
  // The raw text uses Obsidian's pasted-image naming.
  await a.locator('.cm-line', { hasText: 'Before' }).click()
  await a.keyboard.press('ArrowDown')
  await expect(a.locator('.cm-content')).toContainText(/!\[\[Pasted image \d{14}\.png\]\]/)
  // Media stays out of the file tree.
  await expect(a.getByTestId('tree').getByText(/Pasted image/)).toHaveCount(0)
  await expect(a.getByTestId('sync-status')).toHaveText(/Synced/, { timeout: 15_000 })
  // Device B: sees the note and the image (bytes fetched from the server).
  await b.getByTestId('tree').getByText(note, { exact: true }).click()
  const bimg = b.locator('.cm-content .jess-img img')
  await expect(bimg).toBeVisible({ timeout: 15_000 })
  await expect.poll(() => bimg.evaluate((i: HTMLImageElement) => i.naturalWidth), { timeout: 15_000 }).toBe(320)
  // Viewer: click opens it on the original.
  await bimg.click()
  await expect(b.getByTestId('image-viewer')).toBeVisible()
  await b.keyboard.press('Escape')
  await expect(b.getByTestId('image-viewer')).toHaveCount(0)

  // After a reload the service worker controls the page and serves /_blob/ URLs; on device A the
  // bytes are local, so the image still shows with the network gone.
  await a.reload()
  await expect.poll(() => a.evaluate(() => !!navigator.serviceWorker.controller), { timeout: 10_000 }).toBe(true)
  await a.reload()
  const aimg = a.locator('.cm-content .jess-img img')
  await expect(aimg).toBeVisible({ timeout: 10_000 })
  expect(await aimg.getAttribute('src')).toMatch(/^\/_blob\/[0-9a-f]{64}\/display/)
  await a.context().setOffline(true)
  await a.reload()
  await expect.poll(() => a.locator('.cm-content .jess-img img').evaluate((i: HTMLImageElement) => i.naturalWidth), { timeout: 10_000 }).toBe(320)
  await a.context().setOffline(false)
})
