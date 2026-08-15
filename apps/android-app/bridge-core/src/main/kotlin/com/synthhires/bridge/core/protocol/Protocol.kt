/*
 * Mirror Kotlin del WS protocol en src/lib/agent/bridge-protocol.ts.
 * Las data classes serializan camelCase para interoperar con el
 * backend (que habla snake_case en JSON via la convención de serde
 * del daemon-protocol Rust crate).
 *
 * Cualquier cambio de schema requiere tocar:
 *   1. src/lib/agent/bridge-protocol.ts (server)
 *   2. apps/desktop-daemon/crates/daemon-protocol/src/lib.rs (Rust)
 *   3. ESTE archivo (Kotlin)
 *
 * Una tarea pendiente (PR-FUTURE) es generar los tres desde un
 * solo schema (TypeBox / pkl / similar). Hoy el contrato se mantiene
 * por convención y revisión cruzada en PR.
 */

package com.synthhires.bridge.core.protocol

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonClassDiscriminator

const val PROTOCOL_VERSION = 1

@OptIn(kotlinx.serialization.ExperimentalSerializationApi::class)
@JsonClassDiscriminator("kind")
@Serializable
sealed class BridgeFrame {

    @Serializable
    @SerialName("hello")
    data class Hello(
        val v: Int = PROTOCOL_VERSION,
        val tokenHash: String,
        val fingerprint: String,
        val deviceKind: String, // "desktop" | "mobile"
        val deviceName: String,
        val clientVersion: String,
    ) : BridgeFrame()

    @Serializable
    @SerialName("hello_ack")
    data class HelloAck(
        val v: Int = PROTOCOL_VERSION,
        val deviceId: String,
        val scopes: BridgeScopes,
        val heartbeatIntervalMs: Long,
    ) : BridgeFrame()

    @Serializable
    @SerialName("heartbeat")
    data class Heartbeat(
        val v: Int = PROTOCOL_VERSION,
        val t: Long,
    ) : BridgeFrame()

    @Serializable
    @SerialName("heartbeat_ack")
    data class HeartbeatAck(
        val v: Int = PROTOCOL_VERSION,
        val t: Long,
    ) : BridgeFrame()

    @Serializable
    @SerialName("action_request")
    data class ActionRequest(
        val v: Int = PROTOCOL_VERSION,
        val id: String,
        val capability: String,
        val params: Map<String, kotlinx.serialization.json.JsonElement>,
        val conversationId: String? = null,
        val skipConsentPrompt: Boolean = false,
    ) : BridgeFrame()

    @Serializable
    @SerialName("action_result")
    data class ActionResult(
        val v: Int = PROTOCOL_VERSION,
        val id: String,
        val ok: Boolean,
        val output: kotlinx.serialization.json.JsonElement? = null,
        val error: ActionError? = null,
        val durationMs: Long,
    ) : BridgeFrame()

    @Serializable
    @SerialName("action_stream")
    data class ActionStream(
        val v: Int = PROTOCOL_VERSION,
        val id: String,
        val seq: Long,
        val channel: String, // "stdout" | "stderr" | "log"
        val data: String,
        val eof: Boolean = false,
    ) : BridgeFrame()

    @Serializable
    @SerialName("consent_prompt")
    data class ConsentPrompt(
        val v: Int = PROTOCOL_VERSION,
        val id: String,
        val capability: String,
        val summary: String,
        val paramsHash: String,
    ) : BridgeFrame()

    @Serializable
    @SerialName("consent_response")
    data class ConsentResponse(
        val v: Int = PROTOCOL_VERSION,
        val id: String,
        val approved: Boolean,
        val remember: Boolean,
    ) : BridgeFrame()

    @Serializable
    @SerialName("scope_update")
    data class ScopeUpdate(
        val v: Int = PROTOCOL_VERSION,
        val scopes: BridgeScopes,
        val reason: String? = null,
    ) : BridgeFrame()

    @Serializable
    @SerialName("resume")
    data class Resume(
        val v: Int = PROTOCOL_VERSION,
        val deviceId: String,
    ) : BridgeFrame()

    @Serializable
    @SerialName("revoke")
    data class Revoke(
        val v: Int = PROTOCOL_VERSION,
        val reason: String,
    ) : BridgeFrame()

    @Serializable
    @SerialName("error")
    data class Error(
        val v: Int = PROTOCOL_VERSION,
        val code: String,
        val message: String,
        val close: Int? = null,
    ) : BridgeFrame()
}

@Serializable
data class BridgeScopes(
    val capabilities: List<String> = emptyList(),
    val alwaysAllowPaths: List<String> = emptyList(),
)

@Serializable
data class ActionError(
    val code: String,
    val message: String,
)

/** Close codes from src/lib/agent/bridge-protocol.ts BRIDGE_CLOSE_CODES. */
object CloseCodes {
    const val NORMAL = 1000
    const val GOING_AWAY = 1001
    const val AUTH_FAILED = 4001
    const val CAPABILITY_NOT_GRANTED = 4003
    const val RATE_LIMITED = 4029
    const val REVOKED = 4401
    const val PROTOCOL_MISMATCH = 4400
}