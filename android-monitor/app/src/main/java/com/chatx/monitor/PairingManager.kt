package com.chatx.monitor

import android.os.Build
import java.io.IOException
import java.util.Collections
import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicReference
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import org.json.JSONObject

class PairingManager {
    private data class Candidate(
        val label: String,
        val url: String,
        val relay: Boolean,
        val routeToken: String? = null,
    )

    fun pair(text: String): PairingConfig {
        val invite = PairingInvite.parse(text)
        if (System.currentTimeMillis() > invite.expiresAt) {
            throw IllegalArgumentException(
                "配对二维码已过期，请在 ChatX 中重新生成。",
            )
        }

        val candidates = buildList {
            invite.directCandidates.forEach { endpoint ->
                add(
                    Candidate(
                        label = endpoint.kind,
                        url = endpoint.url,
                        relay = false,
                    ),
                )
            }
            invite.relayPairing?.let { relay ->
                add(
                    Candidate(
                        label = "relay",
                        url = relay.url,
                        relay = true,
                        routeToken = relay.routeToken,
                    ),
                )
            }
        }
        require(candidates.isNotEmpty()) { "配对资料中没有可用 WSS 路径。" }
        return racePairing(invite, androidDeviceName(), candidates)
    }
    private fun racePairing(
        invite: PairingInvite,
        deviceName: String,
        candidates: List<Candidate>,
    ): PairingConfig {
        val result = AtomicReference<PairingConfig?>()
        val winner = AtomicBoolean(false)
        val failures = AtomicInteger(0)
        val errors = Collections.synchronizedList(mutableListOf<String>())
        val latch = CountDownLatch(1)
        val sockets = Collections.synchronizedList(mutableListOf<WebSocket>())
        val clients = Collections.synchronizedList(mutableListOf<OkHttpClient>())
        val scheduler = Executors.newScheduledThreadPool(
            candidates.size.coerceIn(1, 4),
        )

        fun fail(candidate: Candidate, error: Throwable) {
            errors += "${candidate.label} ${candidate.url}: " +
                (error.message ?: error.javaClass.simpleName)
            if (failures.incrementAndGet() >= candidates.size) {
                latch.countDown()
            }
        }

        candidates.forEachIndexed { index, candidate ->
            val delay = when {
                candidate.relay -> 750L
                index == 0 -> 0L
                else -> 250L
            }
            scheduler.schedule(
                {
                    if (winner.get()) return@schedule
                    openCandidate(
                        invite = invite,
                        deviceName = deviceName,
                        candidate = candidate,
                        result = result,
                        winner = winner,
                        latch = latch,
                        onFailure = { fail(candidate, it) },
                        sockets = sockets,
                        clients = clients,
                    )
                },
                delay,
                TimeUnit.MILLISECONDS,
            )
        }

        val completed = latch.await(12, TimeUnit.SECONDS)
        scheduler.shutdownNow()
        synchronized(sockets) {
            sockets.forEach { socket -> socket.cancel() }
        }
        synchronized(clients) {
            clients.forEach { client ->
                client.dispatcher.executorService.shutdown()
                client.connectionPool.evictAll()
            }
        }

        if (!completed) {
            throw IOException("WSS 配对超时。")
        }
        result.get()?.let { return it }
        throw IOException(
            "无法通过 Direct 或 ChatX Relay 完成配对。\n" +
                errors.joinToString("\n"),
        )
    }

    private fun openCandidate(
        invite: PairingInvite,
        deviceName: String,
        candidate: Candidate,
        result: AtomicReference<PairingConfig?>,
        winner: AtomicBoolean,
        latch: CountDownLatch,
        onFailure: (Throwable) -> Unit,
        sockets: MutableList<WebSocket>,
        clients: MutableList<OkHttpClient>,
    ) {
        val candidateDone = AtomicBoolean(false)
        val client = if (candidate.relay) {
            WssClients.relay()
        } else {
            WssClients.direct(invite.fingerprintSha256)
        }
        clients += client
        val builder = Request.Builder().url(candidate.url)
        candidate.routeToken?.let { token ->
            builder.header("Authorization", "Bearer $token")
        }

        val socket = client.newWebSocket(
            builder.build(),
            object : WebSocketListener() {
                private fun failOnce(error: Throwable) {
                    if (candidateDone.compareAndSet(false, true)) {
                        onFailure(error)
                    }
                }

                override fun onOpen(
                    webSocket: WebSocket,
                    response: Response,
                ) {
                    try {
                        val clear = JSONObject().apply {
                            put("pairingCode", invite.pairingCode)
                            put("deviceName", deviceName)
                        }
                        val encrypted = MonitorCrypto.encryptPairing(
                            pairingCode = invite.pairingCode,
                            pairingId = invite.pairingId,
                            direction = "request",
                            payload = clear,
                        )
                        val message = JSONObject().apply {
                            put("type", "pair")
                            put("pairingId", invite.pairingId)
                            put("payload", encrypted.toJson())
                        }
                        if (!webSocket.send(message.toString())) {
                            failOnce(IOException("发送 WSS 配对请求失败。"))
                        }
                    } catch (error: Throwable) {
                        failOnce(error)
                        webSocket.cancel()
                    }
                }

                override fun onMessage(
                    webSocket: WebSocket,
                    text: String,
                ) {
                    if (candidateDone.get() || winner.get()) return
                    try {
                        val root = JSONObject(text)
                        when (root.optString("type")) {
                            "pair_result" -> {
                                require(
                                    root.getString("pairingId") ==
                                        invite.pairingId,
                                ) { "Pairing ID 不匹配。" }
                                val encrypted = EncryptedPairingFrame.parse(
                                    root.getJSONObject("payload"),
                                )
                                val clear = MonitorCrypto.decryptPairing(
                                    pairingCode = invite.pairingCode,
                                    pairingId = invite.pairingId,
                                    direction = "response",
                                    frame = encrypted,
                                )
                                val config = configFromPairResult(
                                    invite,
                                    clear,
                                )
                                if (candidateDone.compareAndSet(false, true) &&
                                    winner.compareAndSet(false, true)
                                ) {
                                    result.set(config)
                                    latch.countDown()
                                }
                                webSocket.close(1000, "paired")
                            }
                            "error" -> {
                                failOnce(
                                    IOException(
                                        root.optString(
                                            "message",
                                            "Desktop 拒绝配对。",
                                        ),
                                    ),
                                )
                                webSocket.close(1000, "pairing failed")
                            }
                        }
                    } catch (error: Throwable) {
                        failOnce(error)
                        webSocket.cancel()
                    }
                }

                override fun onFailure(
                    webSocket: WebSocket,
                    t: Throwable,
                    response: Response?,
                ) {
                    failOnce(t)
                }
            },
        )
        sockets += socket
    }

    private fun configFromPairResult(
        invite: PairingInvite,
        payload: JSONObject,
    ): PairingConfig {
        require(payload.optInt("schemaVersion", -1) == 3) {
            "不支持的配对响应版本。"
        }
        val deviceId = payload.getString("deviceId").trim()
        val directToken = payload.getString("directToken")
            .trim()
            .lowercase()
        val deviceKey = payload.getString("deviceKey").trim()
        val directRuntimeEndpoints = invite.directCandidates.map { endpoint ->
            endpoint.copy(
                url = endpoint.url.replace(
                    "/v1/ws/pair",
                    "/v1/ws/monitor",
                ),
            )
        }
        val relay = payload.optJSONObject("relay")?.let {
            RelayEnrollment(
                baseUrl = it.getString("baseUrl").trimEnd('/'),
                desktopId = it.getString("desktopId").trim(),
                deviceToken = it.getString("deviceToken")
                    .trim()
                    .lowercase(),
            )
        }
        require(directRuntimeEndpoints.isNotEmpty() || relay != null) {
            "Desktop 未返回可用的 WSS 路径。"
        }
        return PairingConfig(
            desktopId = invite.desktopId,
            port = invite.port,
            fingerprintSha256 = invite.fingerprintSha256,
            deviceId = deviceId,
            directToken = directToken,
            deviceKey = deviceKey,
            directEndpoints = directRuntimeEndpoints,
            relay = relay,
        )
    }

    private fun androidDeviceName(): String = buildString {
        val manufacturer = Build.MANUFACTURER
            .trim()
            .replaceFirstChar { it.uppercase() }
        if (manufacturer.isNotBlank()) append(manufacturer)
        if (Build.MODEL.isNotBlank()) {
            if (isNotEmpty()) append(' ')
            append(Build.MODEL.trim())
        }
        if (isEmpty()) append("Android Device")
    }
}
