package com.chatx.monitor

import android.content.Context
import java.io.IOException
import java.security.SecureRandom
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicReference
import okhttp3.Request
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import org.json.JSONObject

data class MonitorControlResult(
    val ok: Boolean,
    val message: String?,
    val snapshot: JSONObject?,
    val endpointUrl: String,
    val transportKind: String,
)

class MonitorControlClient(
    context: Context,
    private val config: PairingConfig,
) {
    private val store = SecureStore(context.applicationContext)
    fun execute(
        action: String,
        timeoutSeconds: Long = 7L,
    ): MonitorControlResult {
        require(action in setOf("refresh_snapshot", "reconnect_tunnel")) {
            "不支持的 Monitor 控制动作。"
        }
        val requestId = "c_" + randomHex(16)
        val issuedAt = System.currentTimeMillis()
        val expiresAt = issuedAt + 30_000L
        val frame = MonitorCrypto.encryptControl(
            config = config,
            requestId = requestId,
            direction = "request",
            issuedAt = issuedAt,
            expiresAt = expiresAt,
            payload = JSONObject().put("action", action),
        )
        val deadlineNanos =
            System.nanoTime() + TimeUnit.SECONDS.toNanos(timeoutSeconds)
        val failures = mutableListOf<String>()
        for (candidate in orderedCandidates()) {
            val remainingNanos = deadlineNanos - System.nanoTime()
            if (remainingNanos <= 0L) break
            val timeoutMillis = TimeUnit.NANOSECONDS
                .toMillis(remainingNanos)
                .coerceAtLeast(1L)
            val result = runCatching {
                executeCandidate(
                    candidate = candidate,
                    action = action,
                    requestId = requestId,
                    issuedAt = issuedAt,
                    expiresAt = expiresAt,
                    frame = frame,
                    timeoutMillis = timeoutMillis,
                )
            }
            result.getOrNull()?.let { return it }
            failures += result.exceptionOrNull()?.message
                ?: "${candidate.kind} 控制请求失败"
        }
        throw IOException(
            failures.firstOrNull()
                ?: "Monitor 控制请求在总超时内没有可用路径。",
        )
    }

    private fun executeCandidate(
        candidate: MonitorRouteCandidate,
        action: String,
        requestId: String,
        issuedAt: Long,
        expiresAt: Long,
        frame: EncryptedControlFrame,
        timeoutMillis: Long,
    ): MonitorControlResult {
        val latch = CountDownLatch(1)
        val sent = AtomicBoolean(false)
        val value = AtomicReference<JSONObject?>()
        val error = AtomicReference<String?>()
        val client = if (candidate.relay) {
            WssClients.relay()
        } else {
            WssClients.direct(config.fingerprintSha256)
        }
        val request = Request.Builder()
            .url(candidate.controlUrl)
            .header("Authorization", "Bearer ${candidate.token}")
            .build()
        val socket = client.newWebSocket(
            request,
            object : WebSocketListener() {
                override fun onOpen(
                    webSocket: WebSocket,
                    response: Response,
                ) {
                    val hello = JSONObject().apply {
                        put("type", "hello")
                        put("protocolVersion", 1)
                        put("desktopId", config.desktopId)
                        put("deviceId", config.deviceId)
                    }
                    if (!webSocket.send(hello.toString())) {
                        error.compareAndSet(null, "发送 Monitor Hello 失败。")
                        latch.countDown()
                    }
                }

                override fun onMessage(
                    webSocket: WebSocket,
                    text: String,
                ) {
                    runCatching {
                        val root = JSONObject(text)
                        when (root.optString("type")) {
                            "hello_ack" -> {
                                if (
                                    candidate.relay &&
                                    root.has("desktopOnline") &&
                                    !root.optBoolean("desktopOnline", false)
                                ) {
                                    throw IOException(
                                        "Desktop 离线，控制暂不可用。",
                                    )
                                }
                                val capability = when (action) {
                                    "refresh_snapshot" ->
                                        "control.refresh_snapshot"
                                    "reconnect_tunnel" ->
                                        "control.reconnect_tunnel"
                                    else -> error("unsupported action")
                                }
                                val capabilities =
                                    root.optJSONArray("capabilities")
                                val supported =
                                    capabilities != null &&
                                        (0 until capabilities.length()).any {
                                            capabilities.optString(it) ==
                                                capability
                                        }
                                if (!supported) {
                                    throw IOException(
                                        "当前 Desktop / Relay 版本不支持该控制动作。",
                                    )
                                }
                                if (sent.compareAndSet(false, true)) {
                                    val control = JSONObject().apply {
                                        put("type", "control")
                                        put("request", frame.toJson())
                                    }
                                    if (!webSocket.send(control.toString())) {
                                        throw IOException(
                                            "发送 Monitor 控制请求失败。",
                                        )
                                    }
                                }
                            }
                            "control_result" -> {
                                val responseFrame =
                                    EncryptedControlFrame.parse(
                                        root.getJSONObject("response"),
                                    )
                                require(responseFrame.requestId == requestId) {
                                    "Monitor 控制响应 requestId 不匹配。"
                                }
                                require(
                                    responseFrame.issuedAt == issuedAt &&
                                        responseFrame.expiresAt == expiresAt,
                                ) {
                                    "Monitor 控制响应时间窗不匹配。"
                                }
                                value.set(
                                    MonitorCrypto.decryptControl(
                                        config,
                                        "response",
                                        responseFrame,
                                    ),
                                )
                                latch.countDown()
                            }
                            "error" -> {
                                error.set(
                                    root.optString(
                                        "message",
                                        "Monitor 控制请求失败。",
                                    ),
                                )
                                latch.countDown()
                            }
                        }
                    }.onFailure {
                        error.compareAndSet(
                            null,
                            it.message ?: it.javaClass.simpleName,
                        )
                        latch.countDown()
                    }
                }

                override fun onFailure(
                    webSocket: WebSocket,
                    t: Throwable,
                    response: Response?,
                ) {
                    error.compareAndSet(
                        null,
                        t.message ?: t.javaClass.simpleName,
                    )
                    latch.countDown()
                }

                override fun onClosed(
                    webSocket: WebSocket,
                    code: Int,
                    reason: String,
                ) {
                    if (value.get() == null && error.get() == null) {
                        error.set("Monitor 控制连接已关闭。")
                        latch.countDown()
                    }
                }
            },
        )
        try {
            if (!latch.await(timeoutMillis, TimeUnit.MILLISECONDS)) {
                throw IOException("Monitor 控制请求超时。")
            }
            error.get()?.let { throw IOException(it) }
            val payload = value.get()
                ?: throw IOException("Monitor 控制响应为空。")
            return MonitorControlResult(
                ok = payload.optBoolean("ok", false),
                message = payload.optString("message")
                    .takeIf { it.isNotBlank() },
                snapshot = payload.optJSONObject("snapshot"),
                endpointUrl = candidate.snapshotUrl,
                transportKind = candidate.kind,
            )
        } finally {
            socket.cancel()
            client.dispatcher.executorService.shutdown()
            client.connectionPool.evictAll()
        }
    }

    private fun orderedCandidates(): List<MonitorRouteCandidate> =
        MonitorRoutePlanner.ordered(
            values = MonitorRoutePlanner.candidates(config),
            policy = store.getRoutePolicy(),
            lastEndpoint = store.getLastEndpoint(),
            selector = store.getManualRouteSelector(),
        )

    private fun randomHex(bytes: Int): String {
        val data = ByteArray(bytes)
        SecureRandom().nextBytes(data)
        return data.joinToString("") { byte ->
            "%02x".format(byte.toInt() and 0xff)
        }
    }
}
