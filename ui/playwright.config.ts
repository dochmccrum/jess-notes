import { defineConfig, devices } from '@playwright/test'
import { mkdtempSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

// E2E against the real server binary serving the built UI (`pnpm build` first).
const port = Number(process.env.E2E_PORT ?? 18787)
const data = process.env.E2E_DATA ?? mkdtempSync(join(tmpdir(), 'jess-e2e-'))
const bin = process.env.JESS_BIN ?? '../target/debug/jess'
export const PASSWORD = 'correct horse battery'

export default defineConfig({
  testDir: 'e2e',
  timeout: 30_000,
  fullyParallel: false,
  workers: 1,
  retries: 0,
  reporter: [['list']],
  use: {
    baseURL: `http://127.0.0.1:${port}`,
    trace: 'retain-on-failure',
  },
  projects: [
    { name: 'chromium', use: { ...devices['Desktop Chrome'] }, grepInvert: /@touch/ },
    { name: 'touch', use: { ...devices['Pixel 7'] }, grep: /@touch/ },
    ...(process.env.E2E_WEBKIT ? [{ name: 'webkit', use: { ...devices['Desktop Safari'] }, grepInvert: /@touch|@chromium-only/ }] : []),
  ],
  webServer: {
    command: `${bin} serve`,
    url: `http://127.0.0.1:${port}/healthz`,
    reuseExistingServer: false,
    env: { JESS_DATA_DIR: data, PORT: String(port), JESS_UI_DIR: 'dist', JESS_ADMIN_PASSWORD: PASSWORD, RUST_LOG: 'warn', JESS_LOGIN_RATE_PER_MINUTE: '1000' },
  },
})
