package com.synthhires.bridge.ui

import android.Manifest
import android.app.Activity
import android.content.Intent
import android.net.Uri
import android.os.Bundle
import android.provider.Settings
import android.view.Gravity
import android.widget.Button
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.TextView
import com.synthhires.bridge.core.protocol.BridgeScopes
import com.synthhires.bridge.core.security.TokenStore
import com.synthhires.bridge.service.BridgeService
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import org.json.JSONObject
import java.security.MessageDigest
import java.util.concurrent.Executors

class MainActivity : Activity() {
    private lateinit var backendInput: EditText
    private lateinit var codeInput: EditText
    private lateinit var status: TextView
    private lateinit var store: TokenStore
    private val executor = Executors.newSingleThreadExecutor()

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        store = TokenStore(this)
        buildUi()
        handleIntent(intent)
    }

    override fun onDestroy() {
        executor.shutdownNow()
        super.onDestroy()
    }

    override fun onNewIntent(intent: Intent?) {
        super.onNewIntent(intent)
        if (intent != null) handleIntent(intent)
    }

    private fun buildUi() {
        val root = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(48, 64, 48, 40)
            gravity = Gravity.CENTER_HORIZONTAL
        }
        val title = TextView(this).apply { text = "SynthHires Bridge"; textSize = 26f; setTextColor(0xff18181b.toInt()) }
        val subtitle = TextView(this).apply { text = "Conecta tu móvil de forma segura y local-first."; textSize = 14f; setPadding(0, 12, 0, 28) }
        backendInput = EditText(this).apply { hint = "https://synthhires.com"; setText(store.loadBackendUrl()); singleLine = true }
        codeInput = EditText(this).apply { hint = "Código de emparejamiento"; singleLine = true; setInputType(2 or 0x80000) }
        val pair = Button(this).apply { text = "Emparejar dispositivo"; setOnClickListener { pair() } }
        val service = Button(this).apply { text = "Iniciar conexión"; setOnClickListener { startBridge() } }
        val notificationSettings = Button(this).apply {
            text = "Configurar notificaciones y automatización"
            setOnClickListener { startActivity(Intent(Settings.ACTION_NOTIFICATION_LISTENER_SETTINGS)) }
        }
        status = TextView(this).apply { text = "Listo para emparejar"; textSize = 13f; setPadding(0, 24, 0, 0) }
        listOf(title, subtitle, backendInput, codeInput, pair, service, notificationSettings, status).forEach { view ->
            root.addView(view, LinearLayout.LayoutParams(-1, -2).apply { bottomMargin = 12 })
        }
        setContentView(root)
    }

    private fun handleIntent(intent: Intent) {
        val uri: Uri = intent.data ?: return
        if (uri.scheme != "synthhires" || uri.host != "pair") return
        uri.getQueryParameter("backend")?.takeIf { it.isNotBlank() }?.let { backendInput.setText(it) }
        uri.getQueryParameter("code")?.takeIf { it.isNotBlank() }?.let { codeInput.setText(it) }
        if (!codeInput.text.isNullOrBlank()) pair()
    }

    private fun pair() {
        val backend = backendInput.text.toString().trim().trimEnd('/')
        val code = codeInput.text.toString().trim().uppercase()
        if (backend.isBlank() || code.isBlank()) { status.text = "Introduce el backend y el código."; return }
        status.text = "Emparejando…"
        executor.execute {
            try {
                val body = JSONObject().apply {
                    put("code", code)
                    put("deviceKind", "mobile")
                    put("deviceName", android.os.Build.MODEL ?: "Android")
                    put("fingerprint", fingerprint())
                    put("desiredScopes", org.json.JSONArray(listOf(
                        "mobile.notifications.read", "mobile.notifications.dismiss", "mobile.sms.read",
                        "mobile.sms.send", "mobile.contacts.read", "mobile.location.read",
                        "mobile.automation.perform", "mobile.clipboard.read", "mobile.clipboard.write",
                    )))
                }
                val request = Request.Builder().url("$backend/api/devices/pair/complete")
                    .post(body.toString().toRequestBody("application/json".toMediaType())).build()
                val response = OkHttpClient().newCall(request).execute()
                val payload = JSONObject(response.body?.string().orEmpty())
                if (!response.isSuccessful || payload.optBoolean("success") != true) error(payload.optString("error", "pairing_failed"))
                val data = payload.getJSONObject("data")
                val deviceId = data.getString("deviceId")
                val token = data.getString("token")
                val scopes = data.getJSONObject("scopes")
                val caps = scopes.getJSONArray("capabilities").let { array -> (0 until array.length()).map(array::getString) }
                val paths = scopes.optJSONArray("alwaysAllowPaths")?.let { array -> (0 until array.length()).map(array::getString) } ?: emptyList()
                store.saveToken(deviceId, token)
                store.saveScopes(deviceId, BridgeScopes(caps, paths))
                store.saveBackendUrl(backend)
                store.saveActiveDeviceId(deviceId)
                runOnUiThread { status.text = "Emparejado correctamente · ${android.os.Build.MODEL}"; requestRuntimePermissions(); startBridge() }
            } catch (error: Throwable) {
                runOnUiThread { status.text = "No se pudo emparejar: ${error.message ?: "error desconocido"}" }
            }
        }
    }

    private fun requestRuntimePermissions() {
        val permissions = mutableListOf(
            Manifest.permission.READ_SMS,
            Manifest.permission.SEND_SMS,
            Manifest.permission.READ_CONTACTS,
            Manifest.permission.ACCESS_COARSE_LOCATION,
            Manifest.permission.ACCESS_FINE_LOCATION,
        )
        if (android.os.Build.VERSION.SDK_INT >= 33) permissions += Manifest.permission.POST_NOTIFICATIONS
        requestPermissions(permissions.toTypedArray(), 40)
    }

    private fun startBridge() {
        if (store.loadActiveDeviceId() == null) { status.text = "Empareja primero este móvil."; return }
        BridgeService.start(this)
        status.text = "Bridge activo en segundo plano"
    }

    private fun fingerprint(): String {
        val id = Settings.Secure.getString(contentResolver, Settings.Secure.ANDROID_ID).orEmpty()
        val value = "${android.os.Build.MANUFACTURER}|${android.os.Build.MODEL}|$id"
        return MessageDigest.getInstance("SHA-256").digest(value.toByteArray()).joinToString("") { "%02x".format(it) }
    }
}
