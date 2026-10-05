package com.chatx.monitor

import android.content.Context
import java.io.Closeable
import java.io.IOException
import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executors
import java.util.concurrent.ScheduledFuture
import java.util.concurrent.TimeUnit
import java.util.concurrent.TimeoutException
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicReference
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
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
        val kind: String,
        val family: String,
        val interfaceName: String,
        val url: String,
        val revokeUrl: String,
        val token: String,
        val relay: Boolean,
    )

    private sealed interface FetchResult {
        data class Snapshot(val value: MonitorSnapshot) : FetchResult
        data class Offline(val message: String) : FetchResult
        data class Failure(val message: String) : FetchResult
    }

    private val store = SecureStore(context.applicationContext)
    private val scheduler = Executors.newScheduledThreadPool(2)
    private val directClient: OkHttpClient = WssClients.direct(config.fingerprintSha256)
        .newBuilder()
        .readTimeout(6, TimeUnit.SECONDS)
        .callTimeout(8, TimeUnit.SECONDS)
        .build()
    private val relayClient: OkHttpClient = WssClients.relay()
        .newBuilder()
        .readTimeout(8, TimeUnit.SECONDS)
        .callTimeout(10, TimeUnit.SECONDS)
        .build()
    private val stopped = AtomicBoolean(true)
    private val pollInFlight = AtomicBoolean(false)
    private val activeCandidate = AtomicReference<Candidate?>(null)
    private var pollFuture: ScheduledFuture<*>? = null
    @Volatile private var consecutiveFailures = 0

    fun start() {
        if (!stopped.compareAndSet(true, false)) return
        consecutiveFailures = 0
        pollFuture = scheduler.scheduleWithFixedDelay(
            { pollCycle() },
            0,
            POLL_INTERVAL_SECONDS,
            TimeUnit.SECONDS,
        )
    }

    override fun close() = stop()

    fun updateRoutes(endpoints: List<MonitorEndpoint>) {
        store.updateDirectEndpoints(endpoints)
    }

    fun reevaluatePolicy() {
        if (!stopped.get()) scheduler.execute { pollCycle() }
    }

    fun requestSelfRevoke() {
        if (stopped.get()) start()
        scheduler.execute {
            val failures = mutableListOf<String>()
            for (candidate in orderedCandidates()) {
                val request = Request.Builder()
                    .url(candidate.revokeUrl)
                    .header("Authorization", "Bearer ${candidate.token}")
                    .post(ByteArray(0).toRequestBody())
                    .build()
                val client = if (candidate.relay) relayClient else directClient
                val result = runCatching {
                    client.newCall(request).execute().use { response ->
                        if (!response.isSuccessful) {
                            throw IOException("HTTP ${response.code}")
                        }
                    }
                }
                if (result.isSuccess) {
                    listener.onRevoked()
                    return@execute
                }
                failures += result.exceptionOrNull()?.message ?: "撤销失败"
            }
            listener.onTransportState(
                MonitorTransportState(
                    phase = "degraded",
                    error = failures.firstOrNull() ?: "没有可用的 HTTPS 撤销路径。",
                ),
            )
        }
    }

    fun stop() {
        if (!stopped.compareAndSet(false, true)) return
        pollFuture?.cancel(true)
        pollFuture = null
        directClient.dispatcher.cancelAll()
        relayClient.dispatcher.cancelAll()
        scheduler.shutdownNow()
        directClient.dispatcher.executorService.shutdown()
        relayClient.dispatcher.executorService.shutdown()
        activeCandidate.set(null)
        listener.onTransportState(MonitorTransportState(phase = "closed"))
    }

    private fun pollCycle() {
        if (stopped.get() || !pollInFlight.compareAndSet(false, true)) return
        try {
            doPollCycle()
        } catch (error: Throwable) {
            publishFailure(error.message ?: error.javaClass.simpleName, false)
        } finally {
            pollInFlight.set(false)
        }
    }

    private fun doPollCycle() {
        val candidates = candidates()
        if (candidates.isEmpty()) {
            publishFailure("没有可用的 HTTPS 或 ChatX Relay 路径。", false)
            return
        }
        if (activeCandidate.get() == null && consecutiveFailures == 0) {
            listener.onTransportState(MonitorTransportState(phase = "connecting"))
        }

        val ordered = orderedCandidates(candidates)
        var offlineMessage: String? = null
        var failureMessage: String? = null
        for (candidate in ordered) {
            when (val result = fetchCandidate(candidate)) {
                is FetchResult.Snapshot -> {
                    consecutiveFailures = 0
                    activeCandidate.set(candidate)
                    store.setLastEndpoint(candidate.url)
                    listener.onTransportState(
                        MonitorTransportState(
                            phase = "connected",
                            transportKind = candidate.kind,
                            url = candidate.url,
                            desktopOnline = true,
                        ),
                    )
                    listener.onSnapshot(result.value)
                    return
                }
                is FetchResult.Offline -> offlineMessage = offlineMessage ?: result.message
                is FetchResult.Failure -> failureMessage = failureMessage ?: result.message
            }
        }

        activeCandidate.set(null)
        publishFailure(
            offlineMessage ?: failureMessage ?: "Monitor HTTPS 请求失败。",
            offlineMessage != null && failureMessage == null,
        )
    }

    private fun publishFailure(message: String, definitelyOffline: Boolean) {
        consecutiveFailures += 1
        listener.onTransportState(
            MonitorTransportState(
                phase = if (definitelyOffline || consecutiveFailures >= 3) {
                    "desktop_offline"
                } else {
                    "degraded"
                },
                reconnectAttempt = consecutiveFailures,
                error = message,
                desktopOnline = if (definitelyOffline) false else null,
            ),
        )
    }

    private fun fetchCandidate(candidate: Candidate): FetchResult {
        val request = Request.Builder()
            .url(candidate.url)
            .header("Authorization", "Bearer ${candidate.token}")
            .header("Cache-Control", "no-store")
            .get()
            .build()
        val client = if (candidate.relay) relayClient else directClient
        return try {
            client.newCall(request).execute().use { response ->
                if (response.code == 401 || response.code == 403) {
                    return FetchResult.Failure("Monitor 凭据已失效（HTTP ${response.code}）。")
                }
                if (!response.isSuccessful) {
                    return FetchResult.Failure("Monitor HTTPS HTTP ${response.code}")
                }
                val text = response.body?.string().orEmpty()
                if (text.isBlank()) return FetchResult.Failure("Monitor HTTPS 返回空响应。")
                val root = JSONObject(text)
                when (root.optString("type")) {
                    "snapshot" -> decodeSnapshot(candidate, root)
                    "status" -> {
                        if (root.optBoolean("desktopOnline", false)) {
                            FetchResult.Failure("Desktop 在线，但 Relay 尚无可用快照。")
                        } else {
                            FetchResult.Offline("Desktop 当前离线。")
                        }
                    }
                    "revoked" -> FetchResult.Failure("当前设备已被撤销。")
                    else -> FetchResult.Failure(
                        root.optString("error", "Monitor HTTPS 响应无效。"),
                    )
                }
            }
        } catch (error: Throwable) {
            FetchResult.Failure(error.message ?: error.javaClass.simpleName)
        }
    }

    private fun decodeSnapshot(candidate: Candidate, root: JSONObject): FetchResult {
        val frame = EncryptedSnapshotFrame.parse(root.getJSONObject("snapshot"))
        val now = System.currentTimeMillis()
        if (frame.generatedAt > now + 60_000L) {
            return FetchResult.Failure("Monitor Snapshot 时间戳超前。")
        }
        val age = now - frame.generatedAt
        if (age > SNAPSHOT_STALE_MS) {
            return FetchResult.Offline("Desktop Snapshot 已过期 ${age / 1000} 秒。")
        }
        val clear = MonitorCrypto.decryptSnapshot(config, frame)
        val snapshot = MonitorSnapshotParser.parse(
            root = clear,
            endpointUrl = candidate.url,
            transportKind = candidate.kind,
        )
        return FetchResult.Snapshot(snapshot)
    }

    private fun candidates(): List<Candidate> {
        val latest = if (dynamicRoutes) store.loadPairing() ?: config else config
        val result = mutableListOf<Candidate>()
        latest.directEndpoints.forEach { endpoint ->
            val snapshotUrl = directHttpsUrl(endpoint.url, "/v1/monitor/snapshot")
            val revokeUrl = directHttpsUrl(endpoint.url, "/v1/monitor/revoke")
            result += Candidate(
                kind = endpoint.kind,
                family = endpoint.family,
                interfaceName = endpoint.interfaceName,
                url = appendIdentityQuery(snapshotUrl),
                revokeUrl = appendIdentityQuery(revokeUrl),
                token = config.directToken,
                relay = false,
            )
        }
        latest.relay?.let { relay ->
            val base = relay.baseUrl.trimEnd('/')
            val root = "$base/v1/desktops/${config.desktopId}/devices/${config.deviceId}"
            result += Candidate(
                kind = "relay",
                family = "https",
                interfaceName = "chatx-relay",
                url = "$root/snapshot",
                revokeUrl = "$root/revoke-self",
                token = relay.deviceToken,
                relay = true,
            )
        }
        return result
    }

    private fun orderedCandidates(values: List<Candidate> = candidates()): List<Candidate> {
        val policy = store.getRoutePolicy()
        val last = store.getLastEndpoint()
        return values.sortedWith(
            compareBy<Candidate> { candidate ->
                when {
                    policy == RoutePolicy.AUTO && candidate.url == last -> -1
                    else -> routeRank(candidate, policy)
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

    private fun selectorMatches(selector: ManualRouteSelector, candidate: Candidate): Boolean =
        candidate.kind == selector.kind &&
            (selector.family.isBlank() ||
                candidate.family == selector.family ||
                (selector.family == "wss" && candidate.family == "https")) &&
            (selector.interfaceName.isBlank() || candidate.interfaceName == selector.interfaceName)

    private fun appendIdentityQuery(url: String): String {
        val separator = if ('?' in url) '&' else '?'
        return "$url${separator}desktopId=${config.desktopId}&deviceId=${config.deviceId}"
    }

    companion object {
        private const val POLL_INTERVAL_SECONDS = 10L
        private const val SNAPSHOT_STALE_MS = 90_000L

        private fun directHttpsUrl(source: String, path: String): String {
            val base = when {
                source.startsWith("wss://") -> "https://" + source.removePrefix("wss://")
                source.startsWith("ws://") -> "http://" + source.removePrefix("ws://")
                else -> source
            }
            return base
                .substringBefore('?')
                .replace("/v1/ws/monitor", path)
                .replace("/v1/ws/pair", path)
        }

        fun probeOnce(
            context: Context,
            config: PairingConfig,
            timeoutMillis: Long = 2_500L,
        ) {
            fetchOnce(
                context = context,
                config = config,
                timeoutSeconds = ((timeoutMillis + 999L) / 1000L).coerceAtLeast(1L),
            )
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
                        if (state.phase == "degraded" || state.phase == "desktop_offline") {
                            state.error?.let(error::set)
                        }
                    }
                    override fun onSnapshot(snapshot: MonitorSnapshot) = Unit
                    override fun onRevoked() { latch.countDown() }
                },
            )
            manager.requestSelfRevoke()
            val completed = latch.await(timeoutSeconds, TimeUnit.SECONDS)
            manager.stop()
            if (!completed) {
                throw TimeoutException(error.get() ?: "HTTPS 设备撤销超时。")
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
                        if (state.phase == "degraded" || state.phase == "desktop_offline") {
                            state.error?.let(error::set)
                            if (state.phase == "desktop_offline") latch.countDown()
                        }
                    }
                    override fun onSnapshot(snapshot: MonitorSnapshot) {
                        result.compareAndSet(null, snapshot)
                        latch.countDown()
                    }
                },
                dynamicRoutes = false,
            )
            manager.start()
            val completed = latch.await(timeoutSeconds, TimeUnit.SECONDS)
            manager.stop()
            if (!completed) {
                throw TimeoutException(error.get() ?: "HTTPS Snapshot 等待超时。")
            }
            return result.get()
                ?: throw IOException(error.get() ?: "HTTPS Snapshot 不可用。")
        }
    }
}
