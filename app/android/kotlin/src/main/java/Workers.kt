package io.github.irgendwer92.lebenszeichen.droid

import android.content.Context
import android.os.Build
import androidx.work.BackoffPolicy
import androidx.work.Constraints
import androidx.work.Data
import androidx.work.ExistingWorkPolicy
import androidx.work.ListenableWorker
import androidx.work.NetworkType
import androidx.work.OneTimeWorkRequest
import androidx.work.OutOfQuotaPolicy
import androidx.work.WorkManager
import androidx.work.Worker
import androidx.work.WorkerParameters
import java.util.concurrent.TimeUnit

/** Building blocks for the jobs of the background path. */
object Work {
    /**
     * A job that needs the network. Expedited jobs start at once and get
     * network access in Doze; before Android 12 they would need a
     * foreground service, so they are left out there.
     */
    fun networked(
        worker: Class<out ListenableWorker>,
        data: Data = Data.EMPTY,
        delaySeconds: Long = 0,
    ): OneTimeWorkRequest {
        val builder =
            OneTimeWorkRequest.Builder(worker)
                .setInputData(data)
                .setConstraints(Constraints.Builder().setRequiredNetworkType(NetworkType.CONNECTED).build())
                .setBackoffCriteria(BackoffPolicy.EXPONENTIAL, 30, TimeUnit.SECONDS)
        if (delaySeconds > 0) {
            builder.setInitialDelay(delaySeconds, TimeUnit.SECONDS)
        } else if (Build.VERSION.SDK_INT >= 31) {
            // A job with a delay cannot be expedited.
            builder.setExpedited(OutOfQuotaPolicy.RUN_AS_NON_EXPEDITED_WORK_REQUEST)
        }
        return builder.build()
    }
}

/**
 * Fetches after a wake signal and shows what is open. A wake signal during
 * a running fetch queues one more fetch after it, so no message is missed.
 */
class FetchWorker(context: Context, params: WorkerParameters) : Worker(context, params) {
    override fun doWork(): Result {
        val app = applicationContext
        val answer = CoreBridge.fetch(app)
        Answer.parseAll(answer).forEach { AnswerPresenter.show(app, it) }
        Requests.update(app, answer, "fetch")
        // Every fetch also looks at the battery (cheap; the core decides).
        try {
            Battery.check(app)
        } catch (e: Exception) {
            EventLog.add(app, Requests.NO_REQUEST, "error", "battery not checked")
        }
        // A reported answer still due right now could not be sent (no
        // network, relay down): try again like after an error.
        val wait = if (answer.has("report_wait")) answer.optLong("report_wait") else null
        val failed = answer.errorText() != null || wait == 0L
        if (failed && runAttemptCount + 1 < MAX_ATTEMPTS) return Result.retry()
        // Out of attempts: try again later rather than never.
        ReportWorker.schedule(app, if (wait == 0L) RETRY_LATER_SECONDS else wait)
        return Result.success()
    }

    companion object {
        private const val MAX_ATTEMPTS = 4
        private const val RETRY_LATER_SECONDS = 900L

        fun enqueue(context: Context, source: String) {
            EventLog.add(context, Requests.NO_REQUEST, "fetch-queued", source)
            WorkManager.getInstance(context)
                .enqueueUniqueWork("fetch", ExistingWorkPolicy.APPEND_OR_REPLACE, Work.networked(FetchWorker::class.java))
        }
    }
}

/**
 * Fetches once when a reported answer becomes due. A fetch sends the
 * reported answers that are due (the core does that in every sync), so this
 * is the same job as [FetchWorker] with a delay and a name of its own.
 * Under Doze it may run late.
 */
object ReportWorker {
    private const val NAME = "report-due"

    /** Margin so the core surely sees the moment as reached. */
    private const val MARGIN_SECONDS = 1L

    /** Plans the fetch for in [waitSeconds], or cancels it with `null`. */
    fun schedule(context: Context, waitSeconds: Long?) {
        val work = WorkManager.getInstance(context)
        if (waitSeconds == null) {
            work.cancelUniqueWork(NAME)
            return
        }
        EventLog.add(context, Requests.NO_REQUEST, "report-planned", "in ${waitSeconds}s")
        val request = Work.networked(FetchWorker::class.java, delaySeconds = waitSeconds + MARGIN_SECONDS)
        work.enqueueUniqueWork(NAME, ExistingWorkPolicy.REPLACE, request)
    }

    /** Fetches now (something is due) and plans the next one. */
    fun now(context: Context) {
        EventLog.add(context, Requests.NO_REQUEST, "report-due", "now")
        FetchWorker.enqueue(context, "state")
    }
}

/**
 * Withdraws a request when it expires. Without a new wake signal nothing
 * else would notice; the core decides from the clock what is still open.
 */
class ExpiryWorker(context: Context, params: WorkerParameters) : Worker(context, params) {
    override fun doWork(): Result {
        Requests.refresh(applicationContext, "expiry")
        return Result.success()
    }

    companion object {
        /** A little after the expiry, so the core surely sees it as past. */
        private const val MARGIN_SECONDS = 5L

        /** Plans the check for the earliest expiry, or cancels it with `null`. */
        fun schedule(context: Context, expiresAt: Long?) {
            val work = WorkManager.getInstance(context)
            if (expiresAt == null) {
                work.cancelUniqueWork("expiry")
                return
            }
            val delay = (expiresAt - System.currentTimeMillis() / 1000 + MARGIN_SECONDS).coerceAtLeast(0)
            val request =
                OneTimeWorkRequest.Builder(ExpiryWorker::class.java)
                    .setInitialDelay(delay, TimeUnit.SECONDS)
                    .build()
            work.enqueueUniqueWork("expiry", ExistingWorkPolicy.REPLACE, request)
        }
    }
}

/**
 * Hands a new push endpoint to the core, which sends it to every mailbox.
 * Never repeated here: once stored, every later fetch sends it until all
 * mailboxes have it.
 */
class PushUrlWorker(context: Context, params: WorkerParameters) : Worker(context, params) {
    override fun doWork(): Result {
        val app = applicationContext
        val url = inputData.getString(KEY_URL)?.takeIf { it.isNotEmpty() }
        val outcome = CoreBridge.setPushUrl(app, url)
        val detail = if (url == null) "removed" else "set"
        if (outcome.optBoolean("ok")) {
            EventLog.add(app, Requests.NO_REQUEST, "push-url", detail)
        } else {
            EventLog.add(app, Requests.NO_REQUEST, "push-url-failed", "$detail: ${outcome.errorText()}")
        }
        return Result.success()
    }

    companion object {
        private const val KEY_URL = "url"

        /** The latest endpoint wins over one still waiting. */
        fun enqueue(context: Context, url: String?) {
            val data = Data.Builder().putString(KEY_URL, url ?: "").build()
            WorkManager.getInstance(context)
                .enqueueUniqueWork("push-url", ExistingWorkPolicy.REPLACE, Work.networked(PushUrlWorker::class.java, data))
        }
    }
}
