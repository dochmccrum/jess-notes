package app.jessnotes.notes

import android.os.Build
import android.os.Bundle
import android.view.View
import android.webkit.JavascriptInterface
import android.webkit.WebView
import androidx.activity.OnBackPressedCallback
import androidx.activity.enableEdgeToEdge
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat

class MainActivity : TauriActivity() {
  // Back is ours (DESIGN §11.5): see onWebViewCreate.
  override val handleBackNavigation: Boolean = false

  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge()
    super.onCreate(savedInstanceState)
    // System bars, cutouts and the keyboard become padding, so the WebView itself shrinks when the
    // keyboard opens. That keeps the caret visible on every WebView version (older ones ignore
    // `interactive-widget=resizes-content`, and edge-to-edge windows don't resize for the IME).
    val content = findViewById<View>(android.R.id.content)
    ViewCompat.setOnApplyWindowInsetsListener(content) { v, insets ->
      val i = insets.getInsets(
        WindowInsetsCompat.Type.systemBars() or WindowInsetsCompat.Type.displayCutout() or WindowInsetsCompat.Type.ime()
      )
      v.setPadding(i.left, i.top, i.right, i.bottom)
      WindowInsetsCompat.CONSUMED
    }
    preferHighestRefreshRate()
  }

  // 120 Hz everywhere (DESIGN §23.4): without this, many phones run apps at 60 Hz on a 120 Hz
  // panel. Asks for the fastest mode at the current resolution; the WebView (Chromium) then
  // renders at that rate, and the system can still lower it (battery saver, thermal limits).
  private fun preferHighestRefreshRate() {
    val display = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) display else @Suppress("DEPRECATION") windowManager.defaultDisplay
    display ?: return
    val current = display.mode
    val best = display.supportedModes
      .filter { it.physicalWidth == current.physicalWidth && it.physicalHeight == current.physicalHeight }
      .maxByOrNull { it.refreshRate } ?: return
    window.attributes = window.attributes.also { it.preferredDisplayModeId = best.modeId }
  }

  override fun onWebViewCreate(webView: WebView) {
    // `window.JessAndroid.restart()`: the UI restarts the app after a space switch or an erase
    // (Tauri's own restart re-executes a binary, which an APK doesn't have).
    webView.addJavascriptInterface(object {
      @JavascriptInterface
      fun restart() {
        runOnUiThread { RestartActivity.restart(this@MainActivity) }
      }
    }, "JessAndroid")
    // Back gesture: the UI closes whatever is open (prompt, viewer, dialog, drawer) and says
    // whether it did; otherwise the app goes to the background like a home press, so coming back
    // is a warm resume rather than a cold start. Posted so it's registered after the Tauri app
    // plugin's own callback (the dispatcher runs the most recently added one).
    webView.post {
      onBackPressedDispatcher.addCallback(this, object : OnBackPressedCallback(true) {
        override fun handleOnBackPressed() {
          webView.evaluateJavascript("window.__jess ? window.__jess.back() : false") { r ->
            if (r != "true") moveTaskToBack(true)
          }
        }
      })
    }
  }
}
