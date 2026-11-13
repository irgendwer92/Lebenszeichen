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
 * A tap anywhere and closing with "back" count as the answer. Leaving
 * through the home button, the screen switching off and the system removing
 * the activity do not count: the notification stays and can still be
 * answered.
 */
class RequestActivity : ComponentActivity() {
    private var requestId: String = ""

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

        requestId = intent.getStringExtra(RequestPresenter.EXTRA_REQUEST_ID) ?: ""
        if (requestId.isEmpty() || Store.isAnswered(this, requestId)) {
            finish()
            return
        }

        val padding = (32 * resources.displayMetrics.density).toInt()
        val title =
            TextView(this).apply {
                text = "Lebenszeichen?"
                textSize = 36f
                gravity = Gravity.CENTER
            }
        val hint =
            TextView(this).apply {
                text = "Antippen oder schließen genügt."
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
        EventLog.add(this, requestId, "fullscreen-shown", "")
    }

    /** Closes the display if the request was answered through the notification meanwhile. */
    override fun onResume() {
        super.onResume()
        if (requestId.isNotEmpty() && Store.isAnswered(this, requestId)) {
            finish()
        }
    }

    private fun react(reaction: String) {
        Requests.react(this, requestId, reaction)
        finish()
    }
}
