package com.chatx.monitor

import android.content.Context
import java.io.Closeable
import java.io.IOException
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executors
import java.util.concurrent.ScheduledFuture
import java.util.concurrent.TimeUnit
import java.util.concurrent.TimeoutException
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicReference
import kotlin.math.min
import kotlin.random.Random
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import org.json.JSONObject

data class MonitorTransportState(
    val phase: String,
    val transportKind: String? = null,
    val url: String? = null,
    val desktopOnline: Boolean? = null,
    val reconnectAttempt: Int = 0,
    val error: String? = null,
)

class MonitorConnectionManager(
    context: Context,
    private val config: PairingConfig,
    private val listener: Listener,
) : Closeable {
    interface Listener {
        fun onTransportState(state: MonitorTransportState)
        fun onSnapshot(snapshot: MonitorSnapshot)
        fun onRevoked() {}
    }

    private data class Candidate(
        val key: String,
        val kind: String,
        val url: String,
        val token: String,
        val relay: Boolean,
    )

    private val store = SecureStore(context.applicationContext)
    private val scheduler = Executors.newScheduledThreadPool(4)
    private val directClient: OkHttpClient =
        WssClients.direct(config.fingerprintSha256)
    private val relayClient: OkHttpClient = WssClients.relay()
    private val sockets = ConcurrentHashMap<String, WebSocket>()
    private val winnerKey = AtomicReference<String?>(null)
    private val stopped = AtomicBoolean(true)
    private val generation = AtomicInteger(0)
    private val reconnectScheduled = AtomicBoolean(false)
    private val revokeRequested = AtomicBoolean(false)
    private var reconnectAttempt = 0
    private var raceTimeout: ScheduledFuture<*>? = null

    @Volatile
    private var producerSessionId: String? = null
    @Volatile
    private var lastSequence: Long = 0L
    @Volatile
    private var lastGeneratedAt: Long = 0L

    fun start() {
        if (!stopped.compareAndSet(true, false)) return
        reconnectAttempt = 0
        beginRace(0L)
    }

    override fun close() = stop()

    fun requestSelfRevoke() {
        revokeRequested.set(true)
        if (stopped.get()) {
            start()
            return
        }
        winnerKey.get()?.let { key ->
            sockets[key]?.send(
                JSONObject().apply { put("type", "revoke_self") }.toString(),
            )
        }
    }

    fun stop() {
        if (!stopped.compareAndSet(false, true)) return
        generation.incrementAndGet()
        raceTimeout?.cancel(true)
        sockets.values.forEach { socket ->
            runCatching { socket.close(1000, "stopped") }
        }
        sockets.clear()
        winnerKey.set(null)
        reconnectScheduled.set(false)
        scheduler.shutdownNow()
        directClient.dispatcher.executorService.shutdown()
        relayClient.dispatcher.executorService.shutdown()
        listener.onTransportState(
            MonitorTransportState(phase = "closed"),
        )
    }

    private fun beginRace(delayMs: Long) {
        if (stopped.get()) return
        scheduler.schedule({
            if (stopped.get()) return@schedule
            val raceGeneration = generation.incrementAndGet()
            raceTimeout?.cancel(true)
            winnerKey.set(null)
            producerSessionId = null
            lastSequence = 0L
            lastGeneratedAt = 0L
            sockets.values.forEach { runCatching { it.cancel() } }
            sockets.clear()

            val candidates = candidates()
            if (candidates.isEmpty()) {
                scheduleReconnect(
                    "没有可用的 Direct WSS 或 ChatX Relay 路径。",
                    raceGeneration,
                )
                return@schedule
            }
            listener.onTransportState(
                MonitorTransportState(
                    phase = if (reconnectAttempt == 0) "connecting" else "reconnecting",
                    reconnectAttempt = reconnectAttempt,
                ),
            )

            val last = store.getLastEndpoint()
            val direct = candidates.filterNot { it.relay }
            val relay = candidates.firstOrNull { it.relay }
            val preferred = candidates.firstOrNull { it.url == last }

            val starts = linkedMapOf<Candidate, Long>()
            if (preferred != null) starts[preferred] = 0L
            direct.forEachIndexed { index, candidate ->
                if (candidate !in starts) {
                    starts[candidate] =
                        if (preferred == null && index == 0) 0L else 250L
                }
            }
            if (relay != null && relay !in starts) {
                starts[relay] = 750L
            }

            starts.forEach { (candidate, stagger) ->
                scheduler.schedule(
                    { openCandidate(candidate, raceGeneration) },
                    stagger,
                    TimeUnit.MILLISECONDS,
                )
            }
            raceTimeout = scheduler.schedule(
                {
                    if (
                        !stopped.get() &&
                        generation.get() == raceGeneration &&
                        winnerKey.get() == null
                    ) {
                        sockets.values.forEach { runCatching { it.cancel() } }
                        sockets.clear()
                        scheduleReconnect(
                            "所有 WSS 路径均未在超时前完成握手。",
                            raceGeneration,
                        )
                    }
                },
                7,
                TimeUnit.SECONDS,
            )
        }, delayMs, TimeUnit.MILLISECONDS)
    }

    private fun candidates(): List<Candidate> {
        val result = mutableListOf<Candidate>()
        config.directEndpoints.forEach { endpoint ->
            result += Candidate(
                key = "direct:${endpoint.url}",
                kind = endpoint.kind,
                url = appendIdentityQuery(endpoint.url),
                token = config.directToken,
                relay = false,
            )
        }
        config.relay?.let { relay ->
            val scheme = when {
                relay.baseUrl.startsWith("https://") ->
                    "wss://" + relay.baseUrl.removePrefix("https://")
                relay.baseUrl.startsWith("http://") ->
                    "ws://" + relay.baseUrl.removePrefix("http://")
                else -> return@let
            }
            val url = "${scheme.trimEnd('/')}/v1/ws/device" +
                "?desktopId=${config.desktopId}&deviceId=${config.deviceId}"
            result += Candidate(
                key = "relay:$url",
                kind = "relay",
                url = url,
                token = relay.deviceToken,
                relay = true,
            )
        }
        return result
    }

    private fun appendIdentityQuery(url: String): String {
        val separator = if ('?' in url) '&' else '?'
        return "$url${separator}desktopId=${config.desktopId}" +
            "&deviceId=${config.deviceId}"
    }

    private fun openCandidate(candidate: Candidate, raceGeneration: Int) {
        if (
            stopped.get() ||
            generation.get() != raceGeneration ||
            winnerKey.get() != null
        ) return

        val request = Request.Builder()
            .url(candidate.url)
            .header("Authorization", "Bearer ${candidate.token}")
            .build()
        val client = if (candidate.relay) relayClient else directClient
        val socket = client.newWebSocket(
            request,
            CandidateListener(candidate, raceGeneration),
        )
        sockets[candidate.key] = socket
    }

    private inner class CandidateListener(
        private val candidate: Candidate,
        private val raceGeneration: Int,
    ) : WebSocketListener() {
        override fun onOpen(webSocket: WebSocket, response: Response) {
            if (!isCurrent()) {
                webSocket.close(1000, "superseded")
                return
            }
            val hello = JSONObject().apply {
                put("type", "hello")
                put("protocolVersion", 1)
                put("desktopId", config.desktopId)
                put("deviceId", config.deviceId)
            }
            if (!webSocket.send(hello.toString())) {
                webSocket.cancel()
            }
        }

        override fun onMessage(webSocket: WebSocket, text: String) {
            if (!isCurrent()) {
                webSocket.close(1000, "superseded")
                return
            }
            try {
                val root = JSONObject(text)
                when (root.optString("type")) {
                    "hello_ack" -> handleHelloAck(webSocket, root)
                    "snapshot" -> handleSnapshot(root)
                    "revoked" -> {
                        revokeRequested.set(false)
                        listener.onRevoked()
                        webSocket.close(1000, "revoked")
                    }
                    "desktop_presence" -> {
                        if (winnerKey.get() == candidate.key) {
                            listener.onTransportState(
                                MonitorTransportState(
                                    phase = if (root.optBoolean("online", false)) {
                                        "connected"
                                    } else {
                                        "desktop_offline"
                                    },
                                    transportKind = candidate.kind,
                                    url = candidate.url,
                                    desktopOnline = root.optBoolean("online", false),
                                ),
                            )
                        }
                    }
                    "error" -> {
                        if (winnerKey.get() == candidate.key) {
                            listener.onTransportState(
                                MonitorTransportState(
                                    phase = "degraded",
                                    transportKind = candidate.kind,
                                    url = candidate.url,
                                    error = root.optString("message", "WSS protocol error"),
                                ),
                            )
                        }
                    }
                }
            } catch (error: Throwable) {
                if (winnerKey.get() == candidate.key) {
                    listener.onTransportState(
                        MonitorTransportState(
                            phase = "degraded",
                            transportKind = candidate.kind,
                            url = candidate.url,
                            error = error.message,
                        ),
                    )
                }
            }
        }

        private fun handleHelloAck(
            webSocket: WebSocket,
            root: JSONObject,
        ) {
            if (!winnerKey.compareAndSet(null, candidate.key)) {
                if (winnerKey.get() != candidate.key) {
                    webSocket.close(1000, "race lost")
                }
                return
            }
            raceTimeout?.cancel(true)
            reconnectAttempt = 0
            reconnectScheduled.set(false)
            producerSessionId = null
            lastSequence = 0L
            lastGeneratedAt = 0L
            store.setLastEndpoint(candidate.url)
            sockets.entries.forEach { (key, socket) ->
                if (key != candidate.key) {
                    runCatching { socket.close(1000, "race lost") }
                    sockets.remove(key)
                }
            }
            val desktopOnline = if (candidate.relay) {
                root.optBoolean("desktopOnline", false)
            } else {
                true
            }
            listener.onTransportState(
                MonitorTransportState(
                    phase = if (desktopOnline) "connected" else "desktop_offline",
                    transportKind = candidate.kind,
                    url = candidate.url,
                    desktopOnline = desktopOnline,
                ),
            )
            if (revokeRequested.get()) {
                webSocket.send(
                    JSONObject().apply { put("type", "revoke_self") }.toString(),
                )
            }
        }

        private fun handleSnapshot(root: JSONObject) {
            if (winnerKey.get() != candidate.key) return
            val frame = EncryptedSnapshotFrame.parse(root.getJSONObject("snapshot"))
            if (producerSessionId == frame.sessionId) {
                if (frame.sequence <= lastSequence) return
                if (frame.generatedAt < lastGeneratedAt) return
            } else {
                // A Desktop WSS reconnect starts a fresh authenticated producer
                // session. Cached Relay frames may begin at any sequence, so
                // freshness is decided by generatedAt rather than assuming seq=1.
                if (lastGeneratedAt > 0L && frame.generatedAt < lastGeneratedAt) return
                lastSequence = 0L
            }
            val now = System.currentTimeMillis()
            if (frame.generatedAt > now + 60_000L) return
            if (now - frame.generatedAt > 90_000L) return

            val clear = MonitorCrypto.decryptSnapshot(config, frame)
            lastSequence = frame.sequence
            lastGeneratedAt = frame.generatedAt
            producerSessionId = frame.sessionId
            val snapshot = MonitorSnapshotParser.parse(
                root = clear,
                endpointUrl = candidate.url,
                transportKind = candidate.kind,
            )
            listener.onSnapshot(snapshot)
        }

        override fun onClosed(
            webSocket: WebSocket,
            code: Int,
            reason: String,
        ) {
            handleDisconnect("WSS closed $code $reason")
        }

        override fun onFailure(
            webSocket: WebSocket,
            t: Throwable,
            response: Response?,
        ) {
            handleDisconnect(
                t.message ?: "WSS connection failed",
            )
        }

        private fun handleDisconnect(message: String) {
            sockets.remove(candidate.key)
            if (winnerKey.compareAndSet(candidate.key, null)) {
                scheduleReconnect(message, raceGeneration)
            }
        }

        private fun isCurrent(): Boolean =
            !stopped.get() && generation.get() == raceGeneration
    }

    private fun scheduleReconnect(
        message: String,
        sourceGeneration: Int,
    ) {
        if (
            stopped.get() ||
            generation.get() != sourceGeneration ||
            !reconnectScheduled.compareAndSet(false, true)
        ) return

        reconnectAttempt += 1
        val seconds = RECONNECT_DELAYS[
            min(reconnectAttempt - 1, RECONNECT_DELAYS.lastIndex)
        ]
        val jitter = 0.8 + Random.nextDouble() * 0.4
        val delayMs = (seconds * 1000 * jitter).toLong()
        listener.onTransportState(
            MonitorTransportState(
                phase = "reconnecting",
                reconnectAttempt = reconnectAttempt,
                error = message,
            ),
        )
        scheduler.schedule({
            reconnectScheduled.set(false)
            beginRace(0L)
        }, delayMs, TimeUnit.MILLISECONDS)
    }

    companion object {
        private val RECONNECT_DELAYS = listOf(1, 2, 5, 10, 30)

        fun revokePairing(
            context: Context,
            config: PairingConfig,
            timeoutSeconds: Long = 8L,
        ) {
            val latch = CountDownLatch(1)
            val error = AtomicReference<String?>()
            lateinit var manager: MonitorConnectionManager
            manager = MonitorConnectionManager(
                context,
                config,
                object : Listener {
                    override fun onTransportState(state: MonitorTransportState) {
                        if (state.phase == "degraded" || state.phase == "reconnecting") {
                            state.error?.let(error::set)
                        }
                    }

                    override fun onSnapshot(snapshot: MonitorSnapshot) {}

                    override fun onRevoked() {
                        latch.countDown()
                    }
                },
            )
            manager.requestSelfRevoke()
            val completed = latch.await(timeoutSeconds, TimeUnit.SECONDS)
            manager.stop()
            if (!completed) {
                throw TimeoutException(
                    error.get() ?: "WSS 设备撤销超时。",
                )
            }
        }

        fun fetchOnce(
            context: Context,
            config: PairingConfig,
            timeoutSeconds: Long = 8L,
        ): MonitorSnapshot {
            val latch = CountDownLatch(1)
            val result = AtomicReference<MonitorSnapshot?>()
            val error = AtomicReference<String?>()
            lateinit var manager: MonitorConnectionManager
            manager = MonitorConnectionManager(
                context,
                config,
                object : Listener {
                    override fun onTransportState(state: MonitorTransportState) {
                        if (state.phase == "degraded") {
                            error.set(state.error)
                        }
                    }

                    override fun onSnapshot(snapshot: MonitorSnapshot) {
                        result.compareAndSet(null, snapshot)
                        latch.countDown()
                    }
                },
            )
            manager.start()
            val completed = latch.await(timeoutSeconds, TimeUnit.SECONDS)
            manager.stop()
            if (!completed) {
                throw TimeoutException(error.get() ?: "WSS Snapshot 等待超时。")
            }
            return result.get()
                ?: throw IOException(error.get() ?: "WSS Snapshot 不可用。")
        }
    }
}
