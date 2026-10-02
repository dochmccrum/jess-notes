package app.jessnotes.notes

import android.app.Activity
import android.content.Intent
import android.os.Bundle
import android.os.Process

// Restarting the app (switching spaces, erasing this device: DESIGN §24.1), the "process
// phoenix" way. MainActivity starts this activity in its own process (`:restart`, see the
// manifest) and exits; this makes sure the old process is gone, starts MainActivity in a fresh
// one, and exits too. It runs while the app is in the foreground, so Android lets it start an
// activity.
class RestartActivity : Activity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    super.onCreate(savedInstanceState)
    val old = intent.getIntExtra(EXTRA_PID, -1)
    if (old > 0) Process.killProcess(old)
    startActivity(Intent(this, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TASK))
    finish()
    Runtime.getRuntime().exit(0)
  }

  companion object {
    const val EXTRA_PID = "app.jessnotes.notes.PID"

    fun restart(from: Activity) {
      from.startActivity(
        Intent(from, RestartActivity::class.java)
          .putExtra(EXTRA_PID, Process.myPid())
          .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
      )
      from.finishAffinity()
      Runtime.getRuntime().exit(0)
    }
  }
}
