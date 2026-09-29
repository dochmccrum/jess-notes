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
  await page.getByTestId('new-note').click()
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
