// Entry point. Kept tiny: theme first (no flash), then the boot record, then mount.
import './app.css'
import { mount } from 'svelte'
import Root from './Root.svelte'
import { readBoot } from './lib/boot'
import { loadDevice } from './stores/device'
import { applyTheme } from './lib/theme'
import { eraseIfRequested } from './lib/erase'

performance.mark('boot-start')
applyTheme(loadDevice().theme)

if ('serviceWorker' in navigator && import.meta.env.PROD) {
  navigator.serviceWorker.register('/sw.js', { scope: '/' }).catch(() => {})
}

// Local data must survive storage pressure (DESIGN §11.7).
void navigator.storage?.persist?.().catch(() => false)

void eraseIfRequested()
  .catch((e) => console.error('erase failed', e))
  .then(readBoot)
  .then((boot) => {
    mount(Root, { target: document.getElementById('app')!, props: { boot } })
  })
