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
    private val activeCandidate = AtomicReference<MonitorRouteCandidate?>(null)
    private var pollFuture: ScheduledFuture<*>? = null
    @Volatile private var consecutiveFailures = 0

    fun start() {
        if (!stopped.compareAndSet(true, false)) return
        consecutiveFailures = 0
        pollFuture = scheduler.scheduleWithFixedDelay(
            { pollCycle() },
            0,
            store.getPollIntervalSeconds(),
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
                    store.setLastEndpoint(candidate.snapshotUrl)
                    listener.onTransportState(
                        MonitorTransportState(
                            phase = "connected",
                            transportKind = candidate.kind,
                            url = candidate.snapshotUrl,
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
                phase = if (
                    definitelyOffline ||
                    consecutiveFailures >= store.getOfflineFailureThreshold()
                ) {
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

    private fun fetchCandidate(candidate: MonitorRouteCandidate): FetchResult {
        val request = Request.Builder()
            .url(candidate.snapshotUrl)
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

    private fun decodeSnapshot(candidate: MonitorRouteCandidate, root: JSONObject): FetchResult {
        val frame = EncryptedSnapshotFrame.parse(root.getJSONObject("snapshot"))
        val now = System.currentTimeMillis()
        if (frame.generatedAt > now + 60_000L) {
            return FetchResult.Failure("Monitor Snapshot 时间戳超前。")
        }
        val age = now - frame.generatedAt
        val staleMs = store.getSnapshotStaleSeconds() * 1000L
        if (age > staleMs) {
            return FetchResult.Offline("Desktop Snapshot 已过期 ${age / 1000} 秒。")
        }
        val clear = MonitorCrypto.decryptSnapshot(config, frame)
        val snapshot = MonitorSnapshotParser.parse(
            root = clear,
            endpointUrl = candidate.snapshotUrl,
            transportKind = candidate.kind,
        )
        return FetchResult.Snapshot(snapshot)
    }

    private fun candidates(): List<MonitorRouteCandidate> {
        val latest = if (dynamicRoutes) {
            store.loadPairing() ?: config
        } else {
            config
        }
        return MonitorRoutePlanner.candidates(config, latest)
    }

    private fun orderedCandidates(
        values: List<MonitorRouteCandidate> = candidates(),
    ): List<MonitorRouteCandidate> =
        MonitorRoutePlanner.ordered(
            values = values,
            policy = store.getRoutePolicy(),
            lastEndpoint = store.getLastEndpoint(),
            selector = store.getManualRouteSelector(),
        )

    companion object {
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
            val completed = try {
                latch.await(timeoutSeconds, TimeUnit.SECONDS)
            } finally {
                manager.stop()
            }
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
            val completed = try {
                latch.await(timeoutSeconds, TimeUnit.SECONDS)
            } finally {
                manager.stop()
            }
            if (!completed) {
                throw TimeoutException(error.get() ?: "HTTPS Snapshot 等待超时。")
            }
            return result.get()
                ?: throw IOException(error.get() ?: "HTTPS Snapshot 不可用。")
        }
    }
}
