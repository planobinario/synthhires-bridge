package com.synthhires.bridge.core.security

import android.content.Context
import android.content.SharedPreferences
import androidx.security.crypto.EncryptedSharedPreferences
import androidx.security.crypto.MasterKey
import com.synthhires.bridge.core.protocol.BridgeScopes

/** Encrypted device credentials backed by Android Keystore. */
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

    fun saveToken(deviceId: String, token: String) = prefs.edit().putString("$PREFIX_TOKEN:$deviceId", token).apply()
    fun loadToken(deviceId: String): String? = prefs.getString("$PREFIX_TOKEN:$deviceId", null)
    fun deleteToken(deviceId: String) = prefs.edit().remove("$PREFIX_TOKEN:$deviceId").apply()

    fun saveScopes(deviceId: String, scopes: BridgeScopes) {
        val caps = scopes.capabilities.joinToString("\u001f")
        val paths = scopes.alwaysAllowPaths.joinToString("\u001f")
        prefs.edit().putString("$PREFIX_SCOPES:$deviceId", "$caps\u001e$paths").apply()
    }

    fun loadScopes(deviceId: String): BridgeScopes? {
        val raw = prefs.getString("$PREFIX_SCOPES:$deviceId", null) ?: return null
        val parts = raw.split("\u001e", limit = 2)
        return BridgeScopes(
            capabilities = parts.getOrNull(0).orEmpty().split("\u001f").filter(String::isNotBlank),
            alwaysAllowPaths = parts.getOrNull(1).orEmpty().split("\u001f").filter(String::isNotBlank),
        )
    }

    fun saveBackendUrl(url: String) = prefs.edit().putString(KEY_BACKEND, url.trimEnd('/')).apply()
    fun loadBackendUrl(): String = prefs.getString(KEY_BACKEND, DEFAULT_BACKEND) ?: DEFAULT_BACKEND
    fun saveActiveDeviceId(deviceId: String) = prefs.edit().putString(KEY_ACTIVE, deviceId).apply()
    fun loadActiveDeviceId(): String? = prefs.getString(KEY_ACTIVE, null)
    fun clear() = prefs.edit().clear().apply()

    companion object {
        private const val PREFIX = "synthhires:bridge:"
        private const val PREFIX_TOKEN = "${PREFIX}token"
        private const val PREFIX_SCOPES = "${PREFIX}scopes"
        private const val KEY_ACTIVE = "${PREFIX}active_device_id"
        private const val KEY_BACKEND = "${PREFIX}backend_url"
        private const val DEFAULT_BACKEND = "https://synthhires.com"
    }
}
