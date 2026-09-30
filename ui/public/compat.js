// Runs before the app bundle, as a classic ES5 script: on an engine too old for the bundle
// (tested minimum Chromium 100, DESIGN §22) say so instead of showing a blank page. The checks
// are APIs the bundle relies on; an engine without them can't parse it either (`?.`, Chromium 80).
;(function () {
  if (typeof structuredClone === 'function' && typeof Array.prototype.at === 'function' && typeof Object.hasOwn === 'function') return
  var android = /Android/.test(navigator.userAgent)
  var msg = android
    ? 'Jess Notes needs a newer Android System WebView (Chromium 100 or later). Update “Android System WebView” (or Chrome) from the Play Store, then reopen the app.'
    : 'This browser is too old for Jess Notes. Use a current version of Chrome, Edge, Firefox or Safari.'
  function show() {
    var app = document.getElementById('app')
    if (app) app.parentNode.removeChild(app)
    var p = document.createElement('p')
    p.setAttribute('data-testid', 'unsupported')
    p.style.cssText = 'max-width:28em;margin:30vh auto 0;padding:0 16px;font:16px/1.5 system-ui,sans-serif;text-align:center'
    p.textContent = msg
    document.body.appendChild(p)
  }
  if (document.body) show()
  else document.addEventListener('DOMContentLoaded', show)
})()
