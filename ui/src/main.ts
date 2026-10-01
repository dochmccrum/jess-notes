// Entry point. Kept tiny: theme first (no flash), then the boot record, then mount.
import './app.css'
import { mount } from 'svelte'
import Root from './Root.svelte'
import { readBoot } from './lib/boot'
import { loadDevice } from './stores/device'
import { applyTheme } from './lib/theme'
import { eraseIfRequested } from './lib/erase'
import { isTauri } from './lib/platform'
import { busyStats, calibrate, frameInterval, frameStats } from './lib/frames'

performance.mark('boot-start')
applyTheme(loadDevice().theme)

// The apps ship their UI and read blobs through `jess-blob://`: no service worker there.
if (!isTauri && 'serviceWorker' in navigator && import.meta.env.PROD) {
  navigator.serviceWorker.register('/sw.js', { scope: '/' }).catch(() => {})
}

// The apps' backend module loads while the boot record is read (Root imports it on launch).
if (isTauri) void import('./backend/tauri')

// Local data must survive storage pressure (DESIGN §11.7).
if (!isTauri) void navigator.storage?.persist?.().catch(() => false)

void eraseIfRequested()
  .catch((e) => console.error('erase failed', e))
  .then(readBoot)
  .then((boot) => {
    mount(Root, { target: document.getElementById('app')!, props: { boot } })
    // The display's refresh interval sets every frame budget (DESIGN §23); learn it once shown.
    void calibrate()
  })

// Frame recorders for the smoothness tests and manual profiling (`__jessFrames.busyStats()`).
;(window as unknown as { __jessFrames?: object }).__jessFrames = { frameStats, busyStats, frameInterval }
