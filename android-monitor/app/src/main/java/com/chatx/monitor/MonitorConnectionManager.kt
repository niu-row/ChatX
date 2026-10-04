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
    private val dynamicRoutes: Boolean = true,
) : Closeable {
    interface Listener {
        fun onTransportState(state: MonitorTransportState)
        fun onSnapshot(snapshot: MonitorSnapshot)
        fun onRevoked() {}
    }

    private data class Candidate(
        val key: String,
        val kind: String,
        val family: String,
        val interfaceName: String,
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
    private val activeCandidate = AtomicReference<Candidate?>(null)
    private val stopped = AtomicBoolean(true)
    private val generation = AtomicInteger(0)
    private val reconnectScheduled = AtomicBoolean(false)
    private val promotionInFlight = AtomicBoolean(false)
    private val revokeRequested = AtomicBoolean(false)
    private var reconnectAttempt = 0
    private var raceTimeout: ScheduledFuture<*>? = null
    private var reconnectFuture: ScheduledFuture<*>? = null

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

    fun updateRoutes(endpoints: List<MonitorEndpoint>) {
        store.updateDirectEndpoints(endpoints)
        reevaluatePolicy()
    }

    fun reevaluatePolicy() {
        if (stopped.get()) return
        scheduler.execute { maybePromote() }
    }

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
        reconnectFuture?.cancel(true)
        sockets.values.forEach { socket ->
            runCatching { socket.close(1000, "stopped") }
        }
        sockets.clear()
        winnerKey.set(null)
        activeCandidate.set(null)
        reconnectScheduled.set(false)
        promotionInFlight.set(false)
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
            reconnectFuture?.cancel(true)
            winnerKey.set(null)
            activeCandidate.set(null)
            promotionInFlight.set(false)
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

            orderedCandidates(candidates).forEachIndexed { index, candidate ->
                scheduler.schedule(
                    { openCandidate(candidate, raceGeneration) },
                    (index * 250L).coerceAtMost(1_000L),
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
        val latest = if (dynamicRoutes) store.loadPairing() ?: config else config
        val result = mutableListOf<Candidate>()
        latest.directEndpoints.forEach { endpoint ->
            result += Candidate(
                key = "direct:${endpoint.url}",
                kind = endpoint.kind,
                family = endpoint.family,
                interfaceName = endpoint.interfaceName,
                url = appendIdentityQuery(endpoint.url),
                token = config.directToken,
                relay = false,
            )
        }
        latest.relay?.let { relay ->
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
                family = "wss",
                interfaceName = "chatx-relay",
                url = url,
                token = relay.deviceToken,
                relay = true,
            )
        }
        return result
    }

    private fun orderedCandidates(
        values: List<Candidate> = candidates(),
    ): List<Candidate> {
        val policy = store.getRoutePolicy()
        val last = store.getLastEndpoint()
        return values.sortedWith(
            compareBy<Candidate> {
                if (policy == RoutePolicy.AUTO && it.url == last) {
                    -1
                } else {
                    routeRank(it, policy)
                }
            }.thenBy { it.url },
        )
    }

    private fun routeRank(candidate: Candidate, policy: RoutePolicy): Int {
        val directRank = when (candidate.kind) {
            "lan" -> 0
            "tailscale" -> 1
            "ipv6" -> 2
            else -> 3
        }
        return when (policy) {
            RoutePolicy.AUTO -> directRank + if (candidate.relay) 10 else 0
            RoutePolicy.LAN_FIRST -> if (candidate.relay) 10 else directRank
            RoutePolicy.RELAY_FIRST -> if (candidate.relay) 0 else directRank + 10
            RoutePolicy.MANUAL -> {
                val selector = store.getManualRouteSelector()
                when {
                    selector != null && selectorMatches(selector, candidate) -> 0
                    candidate.relay -> 10
                    else -> directRank + 20
                }
            }
        }
    }

    private fun selectorMatches(
        selector: ManualRouteSelector,
        candidate: Candidate,
    ): Boolean =
        candidate.kind == selector.kind &&
            (selector.family.isBlank() || candidate.family == selector.family) &&
            (selector.interfaceName.isBlank() ||
                candidate.interfaceName == selector.interfaceName)


    private fun maybePromote() {
        if (stopped.get() || promotionInFlight.get()) return
        val current = activeCandidate.get() ?: return
        val policy = store.getRoutePolicy()
        if (policy == RoutePolicy.AUTO) return

        val available = candidates()
        val desired = orderedCandidates(available).firstOrNull() ?: return
        val currentStillAdvertised = available.any { it.key == current.key }
        val desiredRank = routeRank(desired, policy)
        val currentRank = routeRank(current, policy)
        val shouldSwitch =
            desired.key != current.key &&
                (desiredRank < currentRank ||
                    (!currentStillAdvertised && desiredRank <= currentRank))
        if (!shouldSwitch || !promotionInFlight.compareAndSet(false, true)) return

        val currentGeneration = generation.get()
        openCandidate(desired, currentGeneration, promotion = true)
        scheduler.schedule(
            {
                if (
                    generation.get() == currentGeneration &&
                    winnerKey.get() != desired.key
                ) {
                    sockets.remove(desired.key)?.cancel()
                    promotionInFlight.set(false)
                }
            },
            5,
            TimeUnit.SECONDS,
        )
    }

    private fun appendIdentityQuery(url: String): String {
        val separator = if ('?' in url) '&' else '?'
        return "$url${separator}desktopId=${config.desktopId}" +
            "&deviceId=${config.deviceId}"
    }

    private fun openCandidate(
        candidate: Candidate,
        raceGeneration: Int,
        promotion: Boolean = false,
    ) {
        if (
            stopped.get() ||
            generation.get() != raceGeneration ||
            (!promotion && winnerKey.get() != null)
        ) return

        val request = Request.Builder()
            .url(candidate.url)
            .header("Authorization", "Bearer ${candidate.token}")
            .build()
        val client = if (candidate.relay) relayClient else directClient
        val socket = client.newWebSocket(
            request,
            CandidateListener(candidate, raceGeneration, promotion),
        )
        sockets[candidate.key] = socket
    }

    private inner class CandidateListener(
        private val candidate: Candidate,
        private val raceGeneration: Int,
        private val promotion: Boolean,
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
            val desktopOnline = if (candidate.relay) {
                root.optBoolean("desktopOnline", false)
            } else {
                true
            }
            if (promotion && !desktopOnline) {
                sockets.remove(candidate.key)
                promotionInFlight.set(false)
                webSocket.close(1000, "desktop offline")
                return
            }

            val accepted = if (promotion) {
                winnerKey.getAndSet(candidate.key) != candidate.key
            } else {
                winnerKey.compareAndSet(null, candidate.key)
            }
            if (!accepted) {
                promotionInFlight.set(false)
                if (winnerKey.get() != candidate.key) {
                    webSocket.close(1000, "race lost")
                }
                return
            }

            raceTimeout?.cancel(true)
            reconnectFuture?.cancel(true)
            reconnectAttempt = 0
            reconnectScheduled.set(false)
            promotionInFlight.set(false)
            activeCandidate.set(candidate)
            producerSessionId = null
            lastSequence = 0L
            lastGeneratedAt = 0L
            store.setLastEndpoint(candidate.url)
            sockets.entries.forEach { (key, socket) ->
                if (key != candidate.key) {
                    runCatching {
                        socket.close(
                            1000,
                            if (promotion) "route promoted" else "race lost",
                        )
                    }
                    sockets.remove(key)
                }
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
            if (!promotion) {
                scheduler.schedule({ maybePromote() }, 500, TimeUnit.MILLISECONDS)
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
                activeCandidate.compareAndSet(candidate, null)
                promotionInFlight.set(false)
                scheduleReconnect(message, raceGeneration)
            } else if (promotion) {
                promotionInFlight.set(false)
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
        reconnectFuture = scheduler.schedule({
            reconnectScheduled.set(false)
            beginRace(0L)
        }, delayMs, TimeUnit.MILLISECONDS)
    }

    companion object {
        private val RECONNECT_DELAYS = listOf(1, 2, 5, 10, 30)

        fun probeOnce(
            context: Context,
            config: PairingConfig,
            timeoutMillis: Long = 2_500L,
        ) {
            val latch = CountDownLatch(1)
            val error = AtomicReference<String?>()
            lateinit var manager: MonitorConnectionManager
            manager = MonitorConnectionManager(
                context,
                config,
                object : Listener {
                    override fun onTransportState(state: MonitorTransportState) {
                        when (state.phase) {
                            "connected" -> latch.countDown()
                            "desktop_offline" -> {
                                error.set("Relay 可达，但 Desktop 当前离线。")
                                latch.countDown()
                            }
                            "degraded" -> state.error?.let(error::set)
                            "reconnecting" -> if (state.reconnectAttempt > 0) {
                                state.error?.let(error::set)
                                latch.countDown()
                            }
                        }
                    }

                    override fun onSnapshot(snapshot: MonitorSnapshot) {}
                },
                dynamicRoutes = false,
            )
            manager.start()
            val completed = latch.await(timeoutMillis, TimeUnit.MILLISECONDS)
            manager.stop()
            if (!completed) {
                throw TimeoutException(error.get() ?: "WSS 握手超时。")
            }
            error.get()?.let { throw IOException(it) }
        }

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
