package io.github.irgendwer92.lebenszeichen.droid

import android.os.Build
import android.os.Bundle
import android.view.Gravity
import android.view.WindowManager
import android.widget.LinearLayout
import android.widget.TextView
import androidx.activity.ComponentActivity
import androidx.activity.OnBackPressedCallback

/**
 * Full-screen display of a request over the lock screen.
 *
 * A tap anywhere, closing with "back" and leaving through the home button or
 * the recents view count as the answer. The screen switching off and the
 * system removing the activity do not count: the notification stays and can
 * still be answered.
 */
class RequestActivity : ComponentActivity() {
    private var name: String = ""
    private var since: Long = -1
    private val key: String get() = "$name@$since"

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        if (Build.VERSION.SDK_INT >= 27) {
            setShowWhenLocked(true)
            setTurnScreenOn(true)
        } else {
            @Suppress("DEPRECATION")
            window.addFlags(
                WindowManager.LayoutParams.FLAG_SHOW_WHEN_LOCKED or
                    WindowManager.LayoutParams.FLAG_TURN_SCREEN_ON,
            )
        }

        name = intent.getStringExtra(RequestPresenter.EXTRA_NAME) ?: ""
        since = intent.getLongExtra(RequestPresenter.EXTRA_SINCE, -1)
        val titleText = intent.getStringExtra(RequestPresenter.EXTRA_TITLE) ?: "Lebenszeichen?"
        if (name.isEmpty() || since < 0 || Store.isAnswered(this, key)) {
            finish()
            return
        }

        val padding = (32 * resources.displayMetrics.density).toInt()
        val title =
            TextView(this).apply {
                text = titleText
                textSize = 32f
                gravity = Gravity.CENTER
            }
        val hint =
            TextView(this).apply {
                text = "Antippen oder Schließen sendet dein Lebenszeichen."
                textSize = 18f
                gravity = Gravity.CENTER
                setPadding(0, padding / 2, 0, 0)
            }
        val root =
            LinearLayout(this).apply {
                orientation = LinearLayout.VERTICAL
                gravity = Gravity.CENTER
                fitsSystemWindows = true
                setPadding(padding, padding, padding, padding)
                addView(title)
                addView(hint)
                setOnClickListener { react(Requests.REACTION_FULLSCREEN_TAP) }
            }
        setContentView(root)

        onBackPressedDispatcher.addCallback(
            this,
            object : OnBackPressedCallback(true) {
                override fun handleOnBackPressed() {
                    react(Requests.REACTION_FULLSCREEN_CLOSE)
                }
            },
        )
        EventLog.add(this, key, "fullscreen-shown", "")
    }

    /** Closes the display if the request was answered through the notification meanwhile. */
    override fun onResume() {
        super.onResume()
        if (name.isNotEmpty() && Store.isAnswered(this, key)) {
            finish()
        }
    }

    /**
     * Called only when the user leaves on purpose (home, recents), not when
     * the screen times out or another activity interrupts.
     */
    override fun onUserLeaveHint() {
        super.onUserLeaveHint()
        if (name.isNotEmpty() && !isFinishing) {
            react(Requests.REACTION_FULLSCREEN_LEAVE)
        }
    }

    private fun react(reaction: String) {
        // Only stores the mark and queues the answer; the network is WorkManager's.
        Requests.react(this, name, since, reaction)
        finish()
    }
}
