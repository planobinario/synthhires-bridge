package com.synthhires.bridge

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Intent
import android.os.Build
import android.os.IBinder
import android.util.Log
import kotlinx.coroutines.*

class BridgeService : Service() {

    companion object {
        private const val TAG = "SynthHiresBridge"
        private const val CHANNEL_ID = "bridge_service"
        private const val NOTIFICATION_ID = 1
    }

    private val scope = CoroutineScope(Dispatchers.IO + SupervisorJob())
    private var isRunning = false

    override fun onCreate() {
        super.onCreate()
        createNotificationChannel()
        Log.i(TAG, "BridgeService created")
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (!isRunning) {
            val token = intent?.getStringExtra("device_token") ?: run {
                Log.e(TAG, "No device_token in intent extras")
                stopSelf()
                return START_NOT_STICKY
            }
            val deviceId = intent.getStringExtra("device_id") ?: run {
                Log.e(TAG, "No device_id in intent extras")
                stopSelf()
                return START_NOT_STICKY
            }
            val backendUrl = intent.getStringExtra("backend_url")
                ?: "wss://app.synthhires.com/api/devices/ws"

            val notification = buildNotification("SynthHires Bridge", "Connected — agent ready")
            startForeground(NOTIFICATION_ID, notification)
            isRunning = true

            scope.launch {
                try {
                    Log.i(TAG, "Starting bridge: device=$deviceId backend=$backendUrl")
                    daemon_core.runBridge(token, deviceId, backendUrl)
                } catch (e: Exception) {
                    Log.e(TAG, "Bridge loop crashed", e)
                }
                isRunning = false
                stopSelf()
            }
        }
        return START_STICKY
    }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onDestroy() {
        scope.cancel()
        isRunning = false
        Log.i(TAG, "BridgeService destroyed")
        super.onDestroy()
    }

    private fun createNotificationChannel() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val channel = NotificationChannel(
                CHANNEL_ID,
                "Bridge Service",
                NotificationManager.IMPORTANCE_LOW
            ).apply {
                description = "SynthHires Bridge — keeps the agent connected"
            }
            val manager = getSystemService(NotificationManager::class.java)
            manager.createNotificationChannel(channel)
        }
    }

    private fun buildNotification(title: String, text: String): Notification {
        val intent = Intent(this, MainActivity::class.java)
        val pendingIntent = PendingIntent.getActivity(
            this, 0, intent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
        )

        return if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            Notification.Builder(this, CHANNEL_ID)
                .setContentTitle(title)
                .setContentText(text)
                .setSmallIcon(android.R.drawable.ic_dialog_info)
                .setContentIntent(pendingIntent)
                .build()
        } else {
            @Suppress("DEPRECATION")
            Notification.Builder(this)
                .setContentTitle(title)
                .setContentText(text)
                .setSmallIcon(android.R.drawable.ic_dialog_info)
                .setContentIntent(pendingIntent)
                .build()
        }
    }
}
