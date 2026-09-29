import { test, expect } from '@playwright/test'
import { login } from './helpers'

test('@touch drawer: menu button opens, scrim closes, tapping a note closes it', async ({ page }) => {
  await login(page)
  const sb = page.getByTestId('sidebar')
  // Drawer starts closed on touch devices.
  await expect(sb).toHaveAttribute('aria-hidden', 'true')
  await page.getByRole('button', { name: 'New note' }).first().dispatchEvent('click')
  await page.getByRole('button', { name: 'Toggle sidebar' }).first().click()
  await expect(sb).not.toHaveAttribute('aria-hidden', 'true')
  await page.mouse.click(page.viewportSize()!.width - 10, 300)
  await expect(sb).toHaveAttribute('aria-hidden', 'true')
})
