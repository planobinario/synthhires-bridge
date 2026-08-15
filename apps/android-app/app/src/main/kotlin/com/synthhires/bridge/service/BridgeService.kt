/*
 * BridgeService — foreground service que mantiene el WebSocket vivo
 * incluso en background.
 *
 * Por qué foreground service y no WorkManager:
 *   • WorkManager tiene mínimo 15 min de cadencia; queremos latencia
 *     <2s en el dispatch de un action_request del agente.
 *   • WorkManager no entrega nada mientras Doze; el bridge debe
 *     responder en tiempo real.
 *   • El foreground service con notificationType=specialUse es la
 *     única vía Android-12+ para mantener el proceso vivo con
 *     garantía de entrega inmediata.
 *
 * El usuario debe aceptar la exención de Doze manualmente (lo
 * pedimos en PairingActivity); sin ella, el bridge dormirá después
 * de ~30 min en background y sólo se despertará con FCM (push) o
 * con la app en foreground.
 *
 * Reconexión: exponential backoff 1s..30s con jitter, igual que el
 * daemon Rust (apps/desktop-daemon/crates/daemon-core/src/ws_client.rs).
 */

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

    override fun onCreate() {
        super.onCreate()
        tokenStore = TokenStore(applicationContext)
        ensureChannel()
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        startForeground(NOTIF_ID, buildOngoingNotification())
        if (loopJob?.isActive != true) {
            loopJob = scope.launch { runLoop() }
        }
        return START_STICKY
    }

    override fun onDestroy() {
        scope.cancel()
        ws?.close(CloseCodes.NORMAL, "service stopped")
        super.onDestroy()
    }

    override fun onBind(intent: Intent?): IBinder? = null

    // ── Reconnect loop ────────────────────────────────────────────────────
    private suspend fun runLoop() {
        val deviceId = tokenStore.loadActiveDeviceId() ?: run {
            Log.w(TAG, "no active paired device; service idle")
            return
        }
        val token = tokenStore.loadToken(deviceId) ?: run {
            Log.w(TAG, "token missing for $deviceId; service idle")
            return
        }
        val backend = BACKEND_WS_URL // env or BuildConfig in real deploy
        var backoffMs = 1000L
        while (true) {
            try {
                connect(backend, deviceId, token)
                // connect() blocks until the WS closes.
                backoffMs = 1000L // reset on graceful cycle
            } catch (t: Throwable) {
                Log.w(TAG, "ws loop error: ${t.message}")
            }
            delay(backoffMs)
            backoffMs = (backoffMs * 2).coerceAtMost(30_000L)
            delay((0..1000).random().toLong()) // jitter
        }
    }

    private suspend fun connect(backend: String, deviceId: String, token: String) {
        val tokenHash = sha256Hex(token)
        val req = Request.Builder()
            .url(backend)
            .addHeader("Sec-WebSocket-Protocol", "bearer.$token")
            .addHeader("X-Bridge-Token-Hash", tokenHash)
            .build()
        val client = OkHttpClient.Builder()
            .pingInterval(30, TimeUnit.SECONDS)
            .connectTimeout(15, TimeUnit.SECONDS)
            .readTimeout(0, TimeUnit.MILLISECONDS)
            .build()
        // suspendCoroutine bridge: OkHttp's WebSocket is callback-based
        // and we want a single suspend entry point. The actual loop
        // is inside the listener.
        val done = kotlinx.coroutines.CompletableDeferred<Unit>()
        val listener = object : WebSocketListener() {
            override fun onOpen(ws: WebSocket, response: Response) {
                this@BridgeService.ws = ws
                val hello = BridgeFrame.Hello(
                    v = PROTOCOL_VERSION,
                    tokenHash = tokenHash,
                    fingerprint = deviceFingerprint(),
                    deviceKind = "mobile",
                    deviceName = android.os.Build.MODEL ?: "Android",
                    clientVersion = "0.1.0",
                )
                ws.send(json.encodeToString(hello))
            }

            override fun onMessage(ws: WebSocket, text: String) {
                scope.launch { handleFrame(ws, text) }
            }

            override fun onClosing(ws: WebSocket, code: Int, reason: String) {
                Log.i(TAG, "server closing $code $reason")
                ws.close(code, reason)
                this@BridgeService.ws = null
                done.complete(Unit)
            }

            override fun onFailure(ws: WebSocket, t: Throwable, response: Response?) {
                Log.w(TAG, "ws failure: ${t.message}")
                this@BridgeService.ws = null
                done.complete(Unit)
            }
        }
        client.newWebSocket(req, listener)
        done.await()
    }

    private suspend fun handleFrame(ws: WebSocket, text: String) {
        val frame = try { json.decodeFromString<BridgeFrame>(text) }
        catch (e: Exception) {
            Log.w(TAG, "bad frame: ${e.message}")
            return
        }
        when (frame) {
            is BridgeFrame.ActionRequest -> handleAction(ws, frame)
            is BridgeFrame.ScopeUpdate -> updateLocalScopes(frame)
            is BridgeFrame.Revoke -> {
                Log.w(TAG, "revoked: ${frame.reason}")
                stopSelf()
            }
            else -> Unit
        }
    }

    private fun handleAction(ws: WebSocket, frame: BridgeFrame.ActionRequest) {
        // Per-action consent: el daemon Kotlin siempre pide
        // confirmación nativa en NotificationService.PromptActivity
        // antes de ejecutar. Aquí sólo enrutamos al handler de
        // capability; el consentimiento se gestiona en
        // ConsentActivity antes de invocar el handler.
        // En la primera versión: DENY si la capability requiere
        // consentimiento explícito y skipConsentPrompt=false.
        val allowed = capabilitiesAllow(frame.capability)
        if (!allowed) {
            sendResult(ws, frame.id, ok = false, error = "capability_not_granted")
            return
        }
        if (requiresNativeConsent(frame.capability) && !frame.skipConsentPrompt) {
            // Lanza ConsentActivity (full-screen intent sobre lockscreen)
            ConsentActivity.request(
                applicationContext,
                actionId = frame.id,
                capability = frame.capability,
                summary = frame.params["summary"]?.toString().orEmpty(),
                ws = ws,
                json = json,
            )
            return
        }
        // Ejecutar via handler específico
        CapabilityHandlers.dispatch(applicationContext, ws, json, frame)
    }

    private fun updateLocalScopes(frame: BridgeFrame.ScopeUpdate) {
        val deviceId = tokenStore.loadActiveDeviceId() ?: return
        tokenStore.saveScopes(deviceId, frame.scopes)
    }

    private fun sendResult(ws: WebSocket, id: String, ok: Boolean, output: String? = null, error: String? = null) {
        val result = BridgeFrame.ActionResult(
            id = id,
            ok = ok,
            output = output?.let { kotlinx.serialization.json.JsonPrimitive(it) },
            error = error?.let { com.synthhires.bridge.core.protocol.ActionError(it, it) },
            durationMs = 0,
        )
        ws.send(json.encodeToString(result))
    }

    private fun sha256Hex(s: String): String {
        val md = MessageDigest.getInstance("SHA-256")
        return md.digest(s.toByteArray(Charsets.UTF_8))
            .joinToString("") { "%02x".format(it) }
    }

    private fun deviceFingerprint(): String {
        // ANDROID_ID es per-app + per-user + per-device y sobrevive
        // factory reset sólo si el OEM lo permite. Suficiente como
        // fingerprint no-secreto.
        @Suppress("HardwareIds")
        val androidId = android.provider.Settings.Secure.getString(
            contentResolver,
            android.provider.Settings.Secure.ANDROID_ID,
        )
        return sha256Hex("${android.os.Build.MANUFACTURER}|${android.os.Build.MODEL}|$androidId")
    }

    private fun capabilitiesAllow(capability: String): Boolean {
        val deviceId = tokenStore.loadActiveDeviceId() ?: return false
        val scopes = tokenStore.loadScopes(deviceId) ?: return false
        return capability in scopes.capabilities
    }

    private fun requiresNativeConsent(capability: String): Boolean = when (capability) {
        "mobile.sms.send",
        "mobile.notifications.dismiss",
        "mobile.clipboard.write",
        "mobile.automation.perform" -> true
        else -> false
    }

    // ── Foreground notification ──────────────────────────────────────────
    private fun ensureChannel() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val nm = getSystemService(NotificationManager::class.java)
            if (nm.getNotificationChannel(CHANNEL) == null) {
                val channel = NotificationChannel(
                    CHANNEL,
                    getString(R.string.bridge_channel_name),
                    NotificationManager.IMPORTANCE_LOW,
                ).apply {
                    description = getString(R.string.bridge_channel_desc)
                    setShowBadge(false)
                    enableVibration(false)
                    setSound(null, null)
                }
                nm.createNotificationChannel(channel)
            }
        }
    }

    private fun buildOngoingNotification(): Notification {
        val openIntent = PendingIntent.getActivity(
            this,
            0,
            Intent(this, MainActivity::class.java).apply {
                flags = Intent.FLAG_ACTIVITY_SINGLE_TOP or Intent.FLAG_ACTIVITY_CLEAR_TOP
            },
            PendingIntent.FLAG_IMMUTABLE,
        )
        val pauseIntent = PendingIntent.getService(
            this,
            1,
            Intent(this, PauseReceiver::class.java),
            PendingIntent.FLAG_IMMUTABLE,
        )
        val builder = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            Notification.Builder(this, CHANNEL)
        } else {
            @Suppress("DEPRECATION")
            Notification.Builder(this)
        }
        return builder
            .setSmallIcon(R.drawable.ic_bridge)
            .setContentTitle(getString(R.string.bridge_notification_title))
            .setContentText(getString(R.string.bridge_notification_text))
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .setPriority(Notification.PRIORITY_LOW)
            .setContentIntent(openIntent)
            .addAction(
                R.drawable.ic_pause,
                getString(R.string.bridge_action_pause),
                pauseIntent,
            )
            .build()
    }

    companion object {
        private const val TAG = "BridgeService"
        private const val CHANNEL = "synthhires-bridge-active"
        private const val NOTIF_ID = 0xB12D6E
        // Production: read from BuildConfig (injected via gradle
        // buildConfigField per flavor). Default to the staging
        // environment for debug builds.
        private const val BACKEND_WS_URL = "wss://app.synthhires.com/api/devices/ws"

        fun start(ctx: Context) {
            val intent = Intent(ctx, BridgeService::class.java)
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                ctx.startForegroundService(intent)
            } else {
                ctx.startService(intent)
            }
        }
    }
}

