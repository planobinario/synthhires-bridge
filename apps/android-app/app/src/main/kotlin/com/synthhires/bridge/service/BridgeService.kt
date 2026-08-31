package com.synthhires.bridge.service

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.os.Build
import android.os.IBinder
import android.util.Log
import com.synthhires.bridge.R
import com.synthhires.bridge.core.protocol.BridgeFrame
import com.synthhires.bridge.core.protocol.CloseCodes
import com.synthhires.bridge.core.protocol.PROTOCOL_VERSION
import com.synthhires.bridge.core.security.TokenStore
import com.synthhires.bridge.ui.MainActivity
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.Json
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import java.security.MessageDigest
import java.util.concurrent.TimeUnit

class BridgeService : Service() {
    private val scope = CoroutineScope(Dispatchers.IO + SupervisorJob())
    private var loopJob: Job? = null
    private var ws: WebSocket? = null
    private lateinit var tokenStore: TokenStore
    private val json = Json { classDiscriminator = "kind"; ignoreUnknownKeys = true }

    override fun onCreate() { super.onCreate(); tokenStore = TokenStore(applicationContext); ensureChannel() }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        startForeground(NOTIF_ID, buildOngoingNotification())
        if (loopJob?.isActive != true) loopJob = scope.launch { runLoop() }
        return START_STICKY
    }

    override fun onDestroy() { scope.cancel(); ws?.close(CloseCodes.NORMAL, "service stopped"); super.onDestroy() }
    override fun onBind(intent: Intent?): IBinder? = null

    private suspend fun runLoop() {
        val deviceId = tokenStore.loadActiveDeviceId() ?: run { Log.w(TAG, "no active paired device"); return }
        val token = tokenStore.loadToken(deviceId) ?: run { Log.w(TAG, "token missing"); return }
        val backend = tokenStore.loadBackendUrl().trimEnd('/') + "/api/devices/ws"
        var backoffMs = 1000L
        while (true) {
            try { connect(backend, token); backoffMs = 1000L }
            catch (error: Throwable) { Log.w(TAG, "ws loop error: ${error.message}") }
            delay(backoffMs)
            backoffMs = (backoffMs * 2).coerceAtMost(30_000L)
            delay((0..1000).random().toLong())
        }
    }

    private suspend fun connect(backend: String, token: String) {
        val tokenHash = sha256Hex(token)
        val request = Request.Builder().url(backend)
            .addHeader("Sec-WebSocket-Protocol", "bearer.$token")
            .addHeader("X-Bridge-Token-Hash", tokenHash).build()
        val client = OkHttpClient.Builder()
            .pingInterval(30, TimeUnit.SECONDS)
            .connectTimeout(15, TimeUnit.SECONDS)
            .readTimeout(0, TimeUnit.MILLISECONDS).build()
        val done = kotlinx.coroutines.CompletableDeferred<Unit>()
        val listener = object : WebSocketListener() {
            override fun onOpen(socket: WebSocket, response: Response) {
                this@BridgeService.ws = socket
                val hello = BridgeFrame.Hello(
                    tokenHash = tokenHash,
                    fingerprint = deviceFingerprint(),
                    deviceName = android.os.Build.MODEL ?: "Android",
                    clientVersion = "0.1.0",
                    os = "android",
                    arch = android.os.Build.SUPPORTED_ABIS.firstOrNull() ?: "unknown",
                )
                socket.send(json.encodeToString(BridgeFrame.serializer(), hello))
            }

            override fun onMessage(socket: WebSocket, text: String) {
                scope.launch { handleFrame(socket, text) }
            }

            override fun onClosing(socket: WebSocket, code: Int, reason: String) {
                socket.close(code, reason); this@BridgeService.ws = null; done.complete(Unit)
            }

            override fun onFailure(socket: WebSocket, error: Throwable, response: Response?) {
                Log.w(TAG, "ws failure: ${error.message}"); this@BridgeService.ws = null; done.complete(Unit)
            }
        }
        client.newWebSocket(request, listener)
        done.await()
    }

    private suspend fun handleFrame(socket: WebSocket, text: String) {
        val frame = try { json.decodeFromString<BridgeFrame>(text) }
        catch (error: Exception) { Log.w(TAG, "bad frame: ${error.message}"); return }
        when (frame) {
            is BridgeFrame.ActionRequest -> handleAction(socket, frame)
            is BridgeFrame.ScopeUpdate -> updateLocalScopes(frame)
            is BridgeFrame.Revoke -> { Log.w(TAG, "revoked: ${frame.reason}"); stopSelf() }
            else -> Unit
        }
    }

    private fun handleAction(socket: WebSocket, frame: BridgeFrame.ActionRequest) {
        if (!capabilitiesAllow(frame.capability)) {
            sendResult(socket, frame.id, false, error = "capability_not_granted"); return
        }
        // These operations are destructive or privacy-sensitive and must
        // always show a local prompt, even if a malformed server request
        // attempts to set skipConsentPrompt=true.
        if (requiresNativeConsent(frame.capability)) {
            ConsentActivity.request(applicationContext, frame, socket, json); return
        }
        CapabilityHandlers.dispatch(applicationContext, socket, json, frame)
    }

    private fun updateLocalScopes(frame: BridgeFrame.ScopeUpdate) {
        tokenStore.loadActiveDeviceId()?.let { tokenStore.saveScopes(it, frame.scopes) }
    }

    private fun sendResult(socket: WebSocket, id: String, ok: Boolean, output: kotlinx.serialization.json.JsonElement? = null, error: String? = null) {
        val result = BridgeFrame.ActionResult(id = id, ok = ok, output = output, error = error?.let { com.synthhires.bridge.core.protocol.ActionError("action_failed", it) })
        socket.send(json.encodeToString(BridgeFrame.serializer(), result))
    }

    private fun sha256Hex(value: String): String = MessageDigest.getInstance("SHA-256")
        .digest(value.toByteArray(Charsets.UTF_8)).joinToString("") { "%02x".format(it) }

    private fun deviceFingerprint(): String {
        @Suppress("HardwareIds")
        val id = android.provider.Settings.Secure.getString(contentResolver, android.provider.Settings.Secure.ANDROID_ID)
        return sha256Hex("${android.os.Build.MANUFACTURER}|${android.os.Build.MODEL}|$id")
    }

    private fun capabilitiesAllow(capability: String): Boolean {
        val id = tokenStore.loadActiveDeviceId() ?: return false
        return tokenStore.loadScopes(id)?.capabilities?.contains(capability) == true
    }

    private fun requiresNativeConsent(capability: String) = capability in setOf(
        "mobile.sms.send",
        "mobile.notifications.dismiss",
        "mobile.clipboard.write",
        "mobile.automation.perform",
    )

    private fun ensureChannel() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val manager = getSystemService(NotificationManager::class.java)
            if (manager.getNotificationChannel(CHANNEL) == null) {
                manager.createNotificationChannel(NotificationChannel(
                    CHANNEL, getString(R.string.bridge_channel_name), NotificationManager.IMPORTANCE_LOW,
                ).apply {
                    description = getString(R.string.bridge_channel_desc)
                    setShowBadge(false); enableVibration(false); setSound(null, null)
                })
            }
        }
    }

    private fun buildOngoingNotification(): Notification {
        val openIntent = PendingIntent.getActivity(this, 0, Intent(this, MainActivity::class.java), PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT)
        val pauseIntent = PendingIntent.getBroadcast(this, 1, Intent(this, PauseReceiver::class.java), PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT)
        val builder = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) Notification.Builder(this, CHANNEL) else @Suppress("DEPRECATION") Notification.Builder(this)
        return builder.setSmallIcon(R.drawable.ic_bridge)
            .setContentTitle(getString(R.string.bridge_notification_title))
            .setContentText(getString(R.string.bridge_notification_text))
            .setOngoing(true).setOnlyAlertOnce(true).setPriority(Notification.PRIORITY_LOW)
            .setContentIntent(openIntent)
            .addAction(R.drawable.ic_pause, getString(R.string.bridge_action_pause), pauseIntent).build()
    }

    companion object {
        private const val TAG = "BridgeService"
        private const val CHANNEL = "synthhires-bridge-active"
        private const val NOTIF_ID = 0xB12D6E
        fun start(context: Context) {
            val intent = Intent(context, BridgeService::class.java)
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) context.startForegroundService(intent) else context.startService(intent)
        }
    }
}
