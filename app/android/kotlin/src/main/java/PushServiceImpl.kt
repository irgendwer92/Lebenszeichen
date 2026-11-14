package io.github.irgendwer92.lebenszeichen.droid

import org.unifiedpush.android.connector.FailedReason
import org.unifiedpush.android.connector.PushService
import org.unifiedpush.android.connector.data.PushEndpoint
import org.unifiedpush.android.connector.data.PushMessage

/**
 * Receives the events of UnifiedPush. The receiver of the connector library
 * is started by the distributor (for example the ntfy app), also when this
 * app is not running, and forwards the events to this service. The work is
 * handed to WorkManager: the network is not allowed here.
 */
class PushServiceImpl : PushService() {
    override fun onNewEndpoint(endpoint: PushEndpoint, instance: String) {
        val changed = Store.endpoint(this) != endpoint.url
        Store.setEndpoint(this, endpoint.url)
        EventLog.add(this, Requests.NO_REQUEST, "endpoint", if (changed) "neu" else "unverändert")
        // Also when unchanged: the core sends nothing then, unless a mailbox still lacks it.
        PushUrlWorker.enqueue(this, endpoint.url)
    }

    /** The content of a wake signal carries nothing; its arrival is the signal. */
    override fun onMessage(message: PushMessage, instance: String) {
        FetchWorker.enqueue(this, "wake")
    }

    override fun onRegistrationFailed(reason: FailedReason, instance: String) {
        EventLog.add(this, Requests.NO_REQUEST, "registration-failed", reason.name)
    }

    override fun onUnregistered(instance: String) {
        Store.setEndpoint(this, "")
        EventLog.add(this, Requests.NO_REQUEST, "unregistered", "vom Verteiler abgemeldet")
        PushUrlWorker.enqueue(this, null)
    }
}
