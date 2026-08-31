package com.synthhires.bridge.service

import android.Manifest
import android.content.Context
import android.content.ClipData
import android.content.ClipboardManager
import android.content.pm.PackageManager
import android.location.LocationManager
import android.net.Uri
import android.provider.ContactsContract
import android.provider.Settings
import android.telephony.SmsManager
import android.util.Base64
import com.synthhires.bridge.core.protocol.BridgeFrame
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonArray
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonPrimitive
import okhttp3.WebSocket
import org.json.JSONObject
import java.util.concurrent.Executors

object CapabilityHandlers {
    private val executor = Executors.newCachedThreadPool()

    fun dispatch(context: Context, ws: WebSocket, json: Json, frame: BridgeFrame.ActionRequest) {
        executor.execute {
            val started = System.currentTimeMillis()
            try {
                val output = when (frame.capability) {
                    "mobile.notifications.read" -> notifications(frame.params)
                    "mobile.notifications.dismiss" -> notificationsDismiss(frame.params)
                    "mobile.sms.read" -> smsRead(context, frame.params)
                    "mobile.sms.send" -> smsSend(context, frame.params)
                    "mobile.contacts.read" -> contactsRead(context, frame.params)
                    "mobile.location.read" -> locationRead(context)
                    "mobile.clipboard.read" -> clipboardRead(context)
                    "mobile.clipboard.write" -> clipboardWrite(context, frame.params)
                    "mobile.automation.perform" -> automation(frame.params)
                    else -> error("unsupported_capability", "Capability no implementada en Android: ${frame.capability}")
                }
                send(ws, json, frame.id, true, output, null, System.currentTimeMillis() - started)
            } catch (t: Throwable) {
                send(ws, json, frame.id, false, null, t.message ?: "mobile_action_failed", System.currentTimeMillis() - started)
            }
        }
    }

    private fun notifications(params: Map<String, JsonElement>): JsonElement {
        val filter = params["appFilter"]?.jsonPrimitive?.contentOrNull
        val limit = params["limit"]?.jsonPrimitive?.intOrNull ?: 20
        return buildJsonObject { put("items", JsonArray(NotificationListener.list(filter, limit).map(::mapJson))) }
    }

    private fun notificationsDismiss(params: Map<String, JsonElement>): JsonElement {
        val key = params["key"]?.jsonPrimitive?.content ?: error("bad_params", "key es obligatorio")
        return buildJsonObject { put("dismissed", NotificationListener.dismissKey(key)) }
    }

    private fun smsRead(context: Context, params: Map<String, JsonElement>): JsonElement {
        requirePermission(context, Manifest.permission.READ_SMS)
        val limit = (params["limit"]?.jsonPrimitive?.intOrNull ?: 50).coerceIn(1, 100)
        val rows = mutableListOf<JsonElement>()
        context.contentResolver.query(Uri.parse("content://sms/inbox"), arrayOf("address", "body", "date"), null, null, "date DESC")?.use { c ->
            val address = c.getColumnIndex("address"); val body = c.getColumnIndex("body"); val date = c.getColumnIndex("date")
            while (c.moveToNext() && rows.size < limit) rows += buildJsonObject {
                put("from", c.getString(address).orEmpty()); put("body", c.getString(body).orEmpty()); put("receivedAt", c.getLong(date))
            }
        }
        return buildJsonObject { put("messages", JsonArray(rows)) }
    }

    private fun smsSend(context: Context, params: Map<String, JsonElement>): JsonElement {
        requirePermission(context, Manifest.permission.SEND_SMS)
        val to = params["to"]?.jsonPrimitive?.content?.trim().orEmpty()
        val body = params["body"]?.jsonPrimitive?.content.orEmpty()
        require(to.length >= 3 && body.isNotBlank()) { "to y body son obligatorios" }
        SmsManager.getDefault().sendTextMessage(to, null, body, null, null)
        return buildJsonObject { put("sent", true); put("to", to) }
    }

    private fun contactsRead(context: Context, params: Map<String, JsonElement>): JsonElement {
        requirePermission(context, Manifest.permission.READ_CONTACTS)
        val limit = (params["limit"]?.jsonPrimitive?.intOrNull ?: 100).coerceIn(1, 200)
        val rows = mutableListOf<JsonElement>()
        context.contentResolver.query(ContactsContract.CommonDataKinds.Phone.CONTENT_URI, arrayOf("display_name", "data1"), null, null, "display_name ASC")?.use { c ->
            val name = c.getColumnIndex("display_name"); val number = c.getColumnIndex("data1")
            while (c.moveToNext() && rows.size < limit) rows += buildJsonObject {
                put("name", c.getString(name).orEmpty()); put("phone", c.getString(number).orEmpty())
            }
        }
        return buildJsonObject { put("contacts", JsonArray(rows)) }
    }

    private fun locationRead(context: Context): JsonElement {
        val hasFine = context.checkSelfPermission(Manifest.permission.ACCESS_FINE_LOCATION) == PackageManager.PERMISSION_GRANTED
        val hasCoarse = context.checkSelfPermission(Manifest.permission.ACCESS_COARSE_LOCATION) == PackageManager.PERMISSION_GRANTED
        require(hasFine || hasCoarse) { "location_permission_required" }
        val manager = context.getSystemService(Context.LOCATION_SERVICE) as LocationManager
        val location = listOf(LocationManager.GPS_PROVIDER, LocationManager.NETWORK_PROVIDER).asSequence()
            .mapNotNull { provider -> runCatching { manager.getLastKnownLocation(provider) }.getOrNull() }
            .maxByOrNull { it.time } ?: error("location_unavailable", "No hay una ubicación reciente disponible")
        return buildJsonObject { put("latitude", location.latitude); put("longitude", location.longitude); put("accuracyMeters", location.accuracy.toDouble()); put("at", location.time) }
    }

    private fun clipboardRead(context: Context): JsonElement {
        val manager = context.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
        val value = manager.primaryClip?.getItemAt(0)?.coerceToText(context)?.toString().orEmpty()
        return buildJsonObject { put("text", value) }
    }

    private fun clipboardWrite(context: Context, params: Map<String, JsonElement>): JsonElement {
        val text = params["text"]?.jsonPrimitive?.content.orEmpty()
        val manager = context.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
        manager.setPrimaryClip(ClipData.newPlainText("SynthHires", text))
        return buildJsonObject { put("written", true); put("length", text.length) }
    }

    private fun automation(params: Map<String, JsonElement>): JsonElement {
        val action = params["action"]?.jsonPrimitive?.content ?: error("bad_params", "action es obligatorio")
        val ok = AutomationAccessibilityService.current()?.perform(
            action,
            params["text"]?.jsonPrimitive?.contentOrNull,
            params["resourceId"]?.jsonPrimitive?.contentOrNull,
        ) == true
        return buildJsonObject { put("performed", ok); put("action", action) }
    }

    private fun requirePermission(context: Context, permission: String) {
        require(context.checkSelfPermission(permission) == PackageManager.PERMISSION_GRANTED) { "permission_required:$permission" }
    }

    private fun error(code: String, message: String): Nothing = throw IllegalStateException("$code:$message")

    private fun mapJson(value: Map<String, Any>): JsonElement = buildJsonObject {
        value.forEach { (key, item) -> when (item) {
            is String -> put(key, item); is Long -> put(key, item); is Int -> put(key, item); is Boolean -> put(key, item); else -> put(key, item.toString())
        } }
    }

    private fun send(ws: WebSocket, json: Json, id: String, ok: Boolean, output: JsonElement?, error: String?, duration: Long) {
        val frame = BridgeFrame.ActionResult(id = id, ok = ok, output = output, error = error?.let { com.synthhires.bridge.core.protocol.ActionError("action_failed", it) }, durationMs = duration)
        ws.send(json.encodeToString(BridgeFrame.serializer(), frame))
    }
}

private val JsonPrimitive.contentOrNull: String? get() = if (isString) content else contentOrNullSafe()
private fun JsonPrimitive.contentOrNullSafe(): String? = runCatching { content }.getOrNull()
private val JsonPrimitive.intOrNull: Int? get() = content.toIntOrNull()
