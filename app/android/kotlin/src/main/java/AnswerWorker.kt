package io.github.irgendwer92.lebenszeichen.droid

import android.content.Context
import androidx.work.Data
import androidx.work.ExistingWorkPolicy
import androidx.work.WorkManager
import androidx.work.Worker
import androidx.work.WorkerParameters

/**
 * Sends the answer to a request through the core, encrypted to the relay of
 * the contact.
 *
 * WorkManager stores the job, so it survives the death of the process and a
 * reboot, waits for a network connection and repeats after network errors.
 * If the answer fails for good, the notification comes back.
 */
class AnswerWorker(context: Context, params: WorkerParameters) : Worker(context, params) {
    override fun doWork(): Result {
        val app = applicationContext
        val name = inputData.getString(KEY_NAME) ?: return Result.failure()
        val since = inputData.getLong(KEY_SINCE, -1).takeIf { it >= 0 } ?: return Result.failure()
        val key = "$name@$since"
        val attempt = runAttemptCount + 1
        val outcome = CoreBridge.respond(app, name, since)
        return when {
            outcome.optBoolean("ok") -> {
                EventLog.add(app, key, "answer-sent", "attempt $attempt")
                Requests.refresh(app, "answer")
                Result.success()
            }
            outcome.optBoolean("retry") && attempt < MAX_ATTEMPTS -> {
                EventLog.add(app, key, "answer-retry", "${outcome.errorText()}, attempt $attempt")
                Result.retry()
            }
            else -> {
                EventLog.add(app, key, "answer-failed", "${outcome.errorText()}, attempt $attempt")
                Store.releaseAnswer(app, key)
                Requests.refresh(app, "answer-failed")
                Result.failure()
            }
        }
    }

    companion object {
        private const val KEY_NAME = "name"
        private const val KEY_SINCE = "since"
        private const val MAX_ATTEMPTS = 6

        /** One job per request: a second reaction is ignored while the first is stored. */
        fun enqueue(context: Context, name: String, since: Long) {
            val data = Data.Builder().putString(KEY_NAME, name).putLong(KEY_SINCE, since).build()
            val request = Work.networked(AnswerWorker::class.java, data)
            EventLog.add(context, "$name@$since", "answer-queued", "")
            WorkManager.getInstance(context)
                .enqueueUniqueWork("answer-$name@$since", ExistingWorkPolicy.KEEP, request)
        }
    }
}
