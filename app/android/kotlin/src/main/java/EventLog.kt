package io.github.irgendwer92.lebenszeichen.droid

import android.content.Context
import android.util.Log
import java.io.File
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale

/**
 * Local event log in a file, so that it survives the death of the process.
 * One line per event: time, request, event, detail, separated by tabs.
 */
object EventLog {
    private const val TAG = "Lebenszeichen"
    private const val FILE_NAME = "events.log"
    private const val MAX_BYTES = 256 * 1024
    private val lock = Any()

    private fun file(context: Context): File = File(context.applicationContext.filesDir, FILE_NAME)

    fun add(context: Context, requestId: String, event: String, detail: String = "") {
        val time = SimpleDateFormat("yyyy-MM-dd HH:mm:ss.SSS", Locale.US).format(Date())
        val cleanDetail = detail.replace('\t', ' ').replace('\n', ' ').replace('\r', ' ')
        val line = "$time\t$requestId\t$event\t$cleanDetail"
        Log.i(TAG, line)
        synchronized(lock) {
            try {
                val target = file(context)
                if (target.length() > MAX_BYTES) {
                    val lines = target.readLines()
                    target.writeText(lines.drop(lines.size / 2).joinToString("\n", postfix = "\n"))
                }
                target.appendText(line + "\n")
            } catch (e: Exception) {
                Log.e(TAG, "event log not writable", e)
            }
        }
    }

    /** Returns the newest lines, oldest first. */
    fun read(context: Context, maxLines: Int): List<String> =
        synchronized(lock) {
            try {
                val target = file(context)
                if (target.exists()) target.readLines().takeLast(maxLines) else emptyList()
            } catch (e: Exception) {
                listOf("Protokoll nicht lesbar: ${e.message}")
            }
        }

    fun clear(context: Context) {
        synchronized(lock) {
            try {
                file(context).delete()
            } catch (e: Exception) {
                Log.e(TAG, "event log not deletable", e)
            }
        }
    }
}
