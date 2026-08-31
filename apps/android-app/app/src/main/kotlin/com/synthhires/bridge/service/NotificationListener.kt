package com.synthhires.bridge.service

import android.service.notification.NotificationListenerService
import android.service.notification.StatusBarNotification
import java.util.concurrent.ConcurrentHashMap

class NotificationListener : NotificationListenerService() {
    override fun onListenerConnected() { instance = this }
    override fun onListenerDisconnected() { if (instance === this) instance = null }

    override fun onNotificationPosted(sbn: StatusBarNotification) {
        val title = sbn.notification.extras.getCharSequence("android.title")?.toString().orEmpty()
        val text = sbn.notification.extras.getCharSequence("android.text")?.toString().orEmpty()
        recent[sbn.key] = NotificationItem(sbn.packageName, title, text, sbn.postTime)
        while (recent.size > MAX_ITEMS) recent.entries.firstOrNull()?.key?.let(recent::remove)
    }

    override fun onNotificationRemoved(sbn: StatusBarNotification) { recent.remove(sbn.key) }

    private fun dismiss(key: String): Boolean = try {
        cancelNotification(key); recent.remove(key); true
    } catch (_: SecurityException) { false }

    data class NotificationItem(val packageName: String, val title: String, val text: String, val postedAt: Long)

    companion object {
        private const val MAX_ITEMS = 200
        private val recent = ConcurrentHashMap<String, NotificationItem>()
        @Volatile private var instance: NotificationListener? = null

        fun list(appFilter: String?, limit: Int): List<Map<String, Any>> = recent.entries.asSequence()
            .filter { appFilter.isNullOrBlank() || it.value.packageName == appFilter }
            .sortedByDescending { it.value.postedAt }
            .take(limit.coerceIn(1, 100))
            .map { (key, item) -> mapOf("key" to key, "packageName" to item.packageName, "title" to item.title, "text" to item.text, "postedAt" to item.postedAt) }
            .toList()

        fun dismissKey(key: String): Boolean = instance?.dismiss(key) == true
    }
}
