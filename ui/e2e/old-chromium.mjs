// The documented minimum engine (Chromium 100, DESIGN §22): the built UI in that Chromium, over
// CDP: sign in, write a note, wait for sync, reload, the note is back. Needs ui/dist and
// target/debug/jess. CI fetches the build (see ci.yml); locally:
//   node ui/e2e/old-chromium.mjs path/to/chrome-linux/chrome
import { spawn } from 'node:child_process'
import { createRequire } from 'node:module'
import { mkdtempSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
const root = resolve(new URL('../..', import.meta.url).pathname)
const { chromium } = createRequire(join(root, 'ui/package.json'))('@playwright/test')
const exe = process.argv[2]
const tmp = mkdtempSync(join(tmpdir(), 'oldchrome-'))
const port = 18000 + Math.floor(Math.random() * 1000)
const cdp = port + 1000
const kids = []
const sleep = (ms) => new Promise((r) => setTimeout(r, ms))
try {
  kids.push(spawn(join(root, 'target/debug/jess'), ['serve'], { env: { ...process.env, JESS_DATA_DIR: join(tmp, 's'), PORT: String(port), JESS_ADMIN_PASSWORD: 'correct horse battery', JESS_UI_DIR: join(root, 'ui/dist'), RUST_LOG: 'warn' }, stdio: 'inherit' }))
  kids.push(spawn(exe, ['--headless', '--no-sandbox', `--remote-debugging-port=${cdp}`, `--user-data-dir=${join(tmp, 'p')}`, 'about:blank'], { stdio: 'ignore' }))
  await sleep(2500)
  const b = await chromium.connectOverCDP(`http://127.0.0.1:${cdp}`)
  const page = b.contexts()[0].pages()[0]
  const errs = []
  page.on('pageerror', (e) => errs.push(e.message))
  page.on('console', (m) => m.type() === 'error' && errs.push(m.text()))
  await page.goto(`http://127.0.0.1:${port}/`)
  console.log(await page.evaluate(() => navigator.userAgent.match(/Chrome\/[\d.]+/)[0]))
  if (await page.$('[data-testid=unsupported]')) throw new Error('compat.js says this engine is unsupported')
  await page.waitForSelector('[data-testid=password]', { timeout: 10000 })
  await page.fill('[data-testid=password]', 'correct horse battery')
  await page.click('button[type=submit]')
  await page.waitForSelector('[data-testid=new-note]', { timeout: 15000 })
  await page.click('[data-testid=new-note]')
  await page.waitForSelector('.cm-content')
  await page.click('.cm-content')
  await page.keyboard.type('old chrome works [[Other]]')
  await page.waitForFunction(() => /Synced/.test(document.querySelector('[data-testid=sync-status]')?.textContent ?? ''), null, { timeout: 15000 })
  await sleep(1500)
  await page.reload()
  await page.waitForFunction(() => document.querySelector('.cm-content')?.textContent.includes('old chrome works'), null, { timeout: 15000 })
  if (errs.length) throw new Error(`console errors: ${errs.join(' | ')}`)
  console.log('OK')
  await b.close()
} catch (e) {
  console.log('FAILED', e.message.split('\n')[0])
  process.exitCode = 1
} finally {
  for (const k of kids) k.kill()
}
process.exit()
