package io.github.irgendwer92.lebenszeichen.droid

import org.unifiedpush.android.connector.FailedReason
import org.unifiedpush.android.connector.PushService
import org.unifiedpush.android.connector.data.PushEndpoint
import org.unifiedpush.android.connector.data.PushMessage

/**
 * Receives the events of UnifiedPush. The receiver of the connector library
 * is started by the distributor (for example the ntfy app), also when this
 * app is not running, and forwards the events to this service.
 */
class PushServiceImpl : PushService() {
    override fun onNewEndpoint(endpoint: PushEndpoint, instance: String) {
        Store.setEndpoint(this, endpoint.url)
        EventLog.add(this, NO_REQUEST, "endpoint", endpoint.url)
    }

    /** The content of a wake signal is ignored; its arrival is the signal. */
    override fun onMessage(message: PushMessage, instance: String) {
        val requestId = Requests.newId()
        EventLog.add(
            this,
            requestId,
            "wake",
            "UnifiedPush, ${message.content.size} Byte, entschlüsselt=${message.decrypted}",
        )
        Requests.open(this, requestId, "push")
    }

    override fun onRegistrationFailed(reason: FailedReason, instance: String) {
        EventLog.add(this, NO_REQUEST, "registration-failed", reason.name)
    }

    override fun onUnregistered(instance: String) {
        Store.setEndpoint(this, "")
        EventLog.add(this, NO_REQUEST, "unregistered", "vom Verteiler abgemeldet")
    }

    companion object {
        const val NO_REQUEST = "-"
    }
}
