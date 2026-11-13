package io.github.irgendwer92.lebenszeichen.droid

import android.content.Context
import android.os.Build
import androidx.work.BackoffPolicy
import androidx.work.Constraints
import androidx.work.Data
import androidx.work.ExistingWorkPolicy
import androidx.work.NetworkType
import androidx.work.OneTimeWorkRequest
import androidx.work.Operation
import androidx.work.OutOfQuotaPolicy
import androidx.work.WorkManager
import androidx.work.Worker
import androidx.work.WorkerParameters
import java.io.IOException
import java.net.HttpURLConnection
import java.net.URL
import java.util.concurrent.TimeUnit

/**
 * Sends the answer of a request as an HTTP POST to the configured test URL.
 *
 * WorkManager stores the job, so it survives the death of the process and a
 * reboot, waits for a network connection and repeats after network errors.
 */
class AnswerWorker(context: Context, params: WorkerParameters) : Worker(context, params) {
    override fun doWork(): Result {
        val app = applicationContext
        val requestId = inputData.getString(KEY_REQUEST_ID) ?: return Result.failure()
        val body = inputData.getString(KEY_BODY) ?: return Result.failure()
        val attempt = runAttemptCount + 1
        val url = Store.answerUrl(app)
        if (url.isBlank()) {
            EventLog.add(app, requestId, "answer-failed", "keine Antwort-URL eingestellt")
            return Result.failure()
        }
        return try {
            val code = post(url, body)
            when {
                code in 200..299 -> {
                    EventLog.add(app, requestId, "answer-sent", "HTTP $code, Versuch $attempt")
                    Result.success()
                }
                (code >= 500 || code == 408 || code == 429) && attempt < MAX_ATTEMPTS -> {
                    EventLog.add(app, requestId, "answer-retry", "HTTP $code, Versuch $attempt")
                    Result.retry()
                }
                else -> {
                    EventLog.add(app, requestId, "answer-failed", "HTTP $code, Versuch $attempt")
                    Result.failure()
                }
            }
        } catch (e: IOException) {
            if (attempt < MAX_ATTEMPTS) {
                EventLog.add(app, requestId, "answer-retry", "$e, Versuch $attempt")
                Result.retry()
            } else {
                EventLog.add(app, requestId, "answer-failed", "$e, Versuch $attempt")
                Result.failure()
            }
        } catch (e: Exception) {
            EventLog.add(app, requestId, "answer-failed", "$e, Versuch $attempt")
            Result.failure()
        }
    }

    private fun post(url: String, body: String): Int {
        val bytes = body.toByteArray(Charsets.UTF_8)
        val connection = URL(url).openConnection() as HttpURLConnection
        try {
            connection.requestMethod = "POST"
            connection.connectTimeout = 15_000
            connection.readTimeout = 15_000
            connection.doOutput = true
            connection.setRequestProperty("Content-Type", "text/plain; charset=utf-8")
            connection.setFixedLengthStreamingMode(bytes.size)
            connection.outputStream.use { it.write(bytes) }
            return connection.responseCode
        } finally {
            connection.disconnect()
        }
    }

    companion object {
        private const val KEY_REQUEST_ID = "request_id"
        private const val KEY_BODY = "body"
        private const val MAX_ATTEMPTS = 6

        /**
         * One job per request: a second call for the same request is ignored
         * while the first is still stored.
         */
        fun enqueue(context: Context, requestId: String, body: String): Operation {
            val builder =
                OneTimeWorkRequest.Builder(AnswerWorker::class.java)
                    .setInputData(
                        Data.Builder()
                            .putString(KEY_REQUEST_ID, requestId)
                            .putString(KEY_BODY, body)
                            .build(),
                    )
                    .setConstraints(
                        Constraints.Builder().setRequiredNetworkType(NetworkType.CONNECTED).build(),
                    )
                    .setBackoffCriteria(BackoffPolicy.EXPONENTIAL, 30, TimeUnit.SECONDS)
            // Expedited jobs start at once and get network access in Doze. Before
            // Android 12 they would need a foreground service, so they are left out there.
            if (Build.VERSION.SDK_INT >= 31) {
                builder.setExpedited(OutOfQuotaPolicy.RUN_AS_NON_EXPEDITED_WORK_REQUEST)
            }
            EventLog.add(context, requestId, "answer-queued", "")
            return WorkManager.getInstance(context)
                .enqueueUniqueWork("answer-$requestId", ExistingWorkPolicy.KEEP, builder.build())
        }
    }
}
