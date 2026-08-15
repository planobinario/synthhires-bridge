/*
 * Secure storage del deviceToken + scopes concedidos.
 *
 * EncryptedSharedPreferences cifra con AES-256-GCM; la master key
 * vive en el Android Keystore (TEE / StrongBox cuando esté
 * disponible). El raw token NUNCA se persiste en plaintext — un
 * dump de /data/data deja sólo ciphertext indescifrable sin el
 * Keystore-bound key (que está atado al device).
 *
 * Esto es el equivalente Kotlin del OS keyring en Rust (Windows
 * Credential Manager / macOS Keychain / Linux Secret Service).
 */

package com.synthhires.bridge.core.security

import android.content.Context
import android.content.SharedPreferences
import android.util.Base64
import androidx.security.crypto.EncryptedSharedPreferences
import androidx.security.crypto.MasterKey
import com.synthhires.bridge.core.protocol.BridgeScopes

class TokenStore(context: Context) {

    private val prefs: SharedPreferences

    init {
        val masterKey = MasterKey.Builder(context)
            .setKeyScheme(MasterKey.KeyScheme.AES256_GCM)
            .build()
        prefs = EncryptedSharedPreferences.create(
            context,
            "synthhires-bridge-secure",
            masterKey,
            EncryptedSharedPreferences.PrefKeyEncryptionScheme.AES256_SIV,
            EncryptedSharedPreferences.PrefValueEncryptionScheme.AES256_GCM,
        )
    }

    fun saveToken(deviceId: String, token: String) {
        prefs.edit().putString("$PREFIX_TOKEN:$deviceId", token).apply()
    }

    fun loadToken(deviceId: String): String? =
        prefs.getString("$PREFIX_TOKEN:$deviceId", null)

    fun deleteToken(deviceId: String) {
        prefs.edit().remove("$PREFIX_TOKEN:$deviceId").apply()
    }

    fun saveScopes(deviceId: String, scopes: BridgeScopes) {
        val json = "${scopes.capabilities.joinToString(",")}|${
            scopes.alwaysAllowPaths.joinToString(",")
        }"
        prefs.edit().putString("$PREFIX_SCOPES:$deviceId", json).apply()
    }

    fun loadScopes(deviceId: String): BridgeScopes? {
        val raw = prefs.getString("$PREFIX_SCOPES:$deviceId", null) ?: return null
        val (caps, paths) = raw.split("|", limit = 2).let {
            it.getOrNull(0).orEmpty() to it.getOrNull(1).orEmpty()
        }
        return BridgeScopes(
            capabilities = caps.split(",").filter { it.isNotBlank() },
            alwaysAllowPaths = paths.split(",").filter { it.isNotBlank() },
        )
    }

    fun saveActiveDeviceId(deviceId: String) {
        prefs.edit().putString(KEY_ACTIVE, deviceId).apply()
    }

    fun loadActiveDeviceId(): String? = prefs.getString(KEY_ACTIVE, null)

    fun clear() { prefs.edit().clear().apply() }

    companion object {
        private const val PREFIX_TOKEN = "token"
        private const val PREFIX_SCOPES = "scopes"
        private const val KEY_ACTIVE = "active_device_id"
        @Suppress("unused")
        private fun b64(s: String) = Base64.encodeToString(s.toByteArray(), Base64.NO_WRAP)
        @Suppress("unused")
        private fun b64d(s: String) = String(Base64.decode(s, Base64.NO_WRAP))
    }
}