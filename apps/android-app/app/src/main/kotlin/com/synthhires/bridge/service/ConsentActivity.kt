package com.synthhires.bridge.service

import android.app.Activity
import android.app.AlertDialog
import android.content.Context
import android.content.Intent
import android.os.Bundle
import com.synthhires.bridge.core.protocol.BridgeFrame
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.Json
import okhttp3.WebSocket

class ConsentActivity : Activity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val request = pending ?: run { finish(); return }
        AlertDialog.Builder(this)
            .setTitle("SynthHires necesita tu permiso")
            .setMessage("${request.frame.capability}\n\nAcción solicitada por el agente")
            .setNegativeButton("Denegar") { _, _ -> answer(false, false) }
            .setPositiveButton("Permitir") { _, _ -> answer(true, false) }
            .setNeutralButton("Permitir siempre") { _, _ -> answer(true, true) }
            .setOnCancelListener { answer(false, false) }
            .show()
    }

    private fun answer(approved: Boolean, remember: Boolean) {
        val request = pending ?: run { finish(); return }
        val consent = BridgeFrame.ConsentResponse(id = request.frame.id, approved = approved, remember = remember)
        request.ws.send(request.json.encodeToString(BridgeFrame.serializer(), consent))
        if (approved) {
            CapabilityHandlers.dispatch(applicationContext, request.ws, request.json, request.frame)
        } else {
            val result = BridgeFrame.ActionResult(
                id = request.frame.id,
                ok = false,
                error = com.synthhires.bridge.core.protocol.ActionError("consent_denied", "El usuario denegó la acción"),
            )
            request.ws.send(request.json.encodeToString(BridgeFrame.serializer(), result))
        }
        pending = null
        finish()
    }

    data class Pending(
        val frame: BridgeFrame.ActionRequest,
        val ws: WebSocket,
        val json: Json,
    )

    companion object {
        @Volatile private var pending: Pending? = null

        fun request(
            context: Context,
            frame: BridgeFrame.ActionRequest,
            ws: WebSocket,
            json: Json,
        ) {
            pending = Pending(frame, ws, json)
            context.startActivity(
                Intent(context, ConsentActivity::class.java)
                    .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP),
            )
        }
    }
}
