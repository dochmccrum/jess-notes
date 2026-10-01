import { defineConfig, devices } from '@playwright/test'
import { mkdtempSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

// E2E against the real server binary serving the built UI (`pnpm build` first). WebKit gets its
// own server and vault: the tests assume a vault only one browser has written to (the tree is
// virtualised, and the switcher/autocomplete rank the other browser's same-named notes first).
const port = Number(process.env.E2E_PORT ?? 18787)
const webkit = !!process.env.E2E_WEBKIT
const bin = process.env.JESS_BIN ?? '../target/debug/jess'
export const PASSWORD = 'correct horse battery'
const server = (port: number, data: string) => ({
  command: `${bin} serve`,
  url: `http://127.0.0.1:${port}/healthz`,
  reuseExistingServer: false,
  env: { JESS_DATA_DIR: data, PORT: String(port), JESS_UI_DIR: 'dist', JESS_ADMIN_PASSWORD: PASSWORD, RUST_LOG: 'warn', JESS_LOGIN_RATE_PER_MINUTE: '1000' },
})

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
    // On GitHub's runners WebKit's first page can take ~17 s to create, which the first test pays.
    ...(webkit ? [{ name: 'webkit', timeout: 60_000, use: { ...devices['Desktop Safari'], baseURL: `http://127.0.0.1:${port + 1}` }, grepInvert: /@touch|@chromium-only/ }] : []),
  ],
  webServer: [
    server(port, process.env.E2E_DATA ?? mkdtempSync(join(tmpdir(), 'jess-e2e-'))),
    ...(webkit ? [server(port + 1, mkdtempSync(join(tmpdir(), 'jess-e2e-webkit-')))] : []),
  ],
})
