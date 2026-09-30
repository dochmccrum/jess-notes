package app.jessnotes.notes

import android.os.Bundle
import android.view.View
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
  }

  override fun onWebViewCreate(webView: WebView) {
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
