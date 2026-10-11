package com.chatx.monitor

import android.content.Context
import android.os.SystemClock
import android.util.Log
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
    private val persistActiveRoute: Boolean = true,
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
    private val pollIntervalSeconds = store.getPollIntervalSeconds()
    @Volatile private var pollFuture: ScheduledFuture<*>? = null
    @Volatile private var lastPollCompletedAt = 0L
    @Volatile private var unexpectedPollFailures = 0
    @Volatile private var consecutiveFailures = 0
    private var activeRouteFailures = 0
    private var preferredRecoverySuccesses = 0
    private var lastPreferredProbeAt = 0L

    fun start() {
        if (!stopped.compareAndSet(true, false)) return
        consecutiveFailures = 0
        activeRouteFailures = 0
        preferredRecoverySuccesses = 0
        lastPreferredProbeAt = 0L
        unexpectedPollFailures = 0
        lastPollCompletedAt = SystemClock.elapsedRealtime()
        pollFuture = scheduler.scheduleWithFixedDelay(
            { pollCycle() },
            0,
            pollIntervalSeconds,
            TimeUnit.SECONDS,
        )
    }

    /** A completed or silent periodic task must not look like a live monitor forever. */
    fun isPollerStalled(now: Long = SystemClock.elapsedRealtime()): Boolean =
        !stopped.get() && (
            pollFuture?.isDone == true ||
                unexpectedPollFailures >= 3 ||
                MonitorPollWatchdog.isStalled(lastPollCompletedAt, now, pollIntervalSeconds)
        )

    override fun close() = stop()

    fun updateRoutes(endpoints: List<MonitorEndpoint>) {
        store.updateDirectEndpoints(
            config.desktopId,
            config.deviceId,
            endpoints,
        )
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
            unexpectedPollFailures = 0
        } catch (error: Throwable) {
            unexpectedPollFailures += 1
            Log.e(TAG, "Monitor polling cycle failed", error)
            // A listener/notification error must not terminate scheduleWithFixedDelay.
            runCatching {
                if (!stopped.get()) {
                    publishFailure(error.message ?: error.javaClass.simpleName, false)
                }
            }.onFailure { Log.e(TAG, "Failed to report Monitor poll error", it) }
        } finally {
            lastPollCompletedAt = SystemClock.elapsedRealtime()
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

        if (persistActiveRoute && store.getRoutePolicy() == RoutePolicy.AUTO) {
            pollAutoStableRoute(candidates)
        } else {
            pollOrderedCandidates(orderedCandidates(candidates))
        }
    }

    private fun pollAutoStableRoute(candidates: List<MonitorRouteCandidate>) {
        val remembered = store.getLastEndpoint()
        val stable = activeCandidate.get()
            ?.takeIf { current ->
                candidates.any { it.snapshotUrl == current.snapshotUrl }
            }
            ?: candidates.firstOrNull { it.snapshotUrl == remembered }
                ?.also(activeCandidate::set)

        if (stable == null) {
            pollOrderedCandidates(orderedCandidates(candidates))
            return
        }

        val preferred = MonitorRoutePlanner.ordered(
            values = candidates,
            policy = RoutePolicy.LAN_FIRST,
            lastEndpoint = null,
            selector = null,
        ).firstOrNull()
        var preferredSnapshot: FetchResult.Snapshot? = null
        val now = System.currentTimeMillis()
        if (
            preferred != null &&
            preferred.snapshotUrl != stable.snapshotUrl &&
            now - lastPreferredProbeAt >= AUTO_RECOVERY_PROBE_INTERVAL_MS
        ) {
            lastPreferredProbeAt = now
            when (val result = fetchCandidate(preferred)) {
                is FetchResult.Snapshot -> {
                    preferredRecoverySuccesses += 1
                    preferredSnapshot = result
                    if (preferredRecoverySuccesses >= AUTO_RECOVERY_SUCCESS_THRESHOLD) {
                        publishConnected(preferred, result.value)
                        preferredRecoverySuccesses = 0
                        return
                    }
                }
                else -> preferredRecoverySuccesses = 0
            }
        }

        when (val result = fetchCandidate(stable)) {
            is FetchResult.Snapshot -> {
                activeRouteFailures = 0
                publishConnected(stable, result.value)
            }
            else -> {
                activeRouteFailures += 1
                if (preferred != null && preferredSnapshot != null) {
                    publishConnected(preferred, preferredSnapshot.value)
                    preferredRecoverySuccesses = 0
                    return
                }
                if (activeRouteFailures < AUTO_FAILOVER_FAILURE_THRESHOLD) {
                    publishFailure(fetchFailureMessage(result), false)
                    return
                }

                val alternatives = MonitorRoutePlanner.ordered(
                    values = candidates.filterNot {
                        it.snapshotUrl == stable.snapshotUrl
                    },
                    policy = RoutePolicy.LAN_FIRST,
                    lastEndpoint = null,
                    selector = null,
                )
                if (pollOrderedCandidates(alternatives)) {
                    preferredRecoverySuccesses = 0
                    return
                }
            }
        }
    }

    private fun pollOrderedCandidates(
        ordered: List<MonitorRouteCandidate>,
    ): Boolean {
        var offlineMessage: String? = null
        var failureMessage: String? = null
        for (candidate in ordered) {
            when (val result = fetchCandidate(candidate)) {
                is FetchResult.Snapshot -> {
                    publishConnected(candidate, result.value)
                    return true
                }
                is FetchResult.Offline ->
                    offlineMessage = offlineMessage ?: result.message
                is FetchResult.Failure ->
                    failureMessage = failureMessage ?: result.message
            }
        }

        publishFailure(
            offlineMessage ?: failureMessage ?: "Monitor HTTPS 请求失败。",
            offlineMessage != null && failureMessage == null,
        )
        return false
    }

    private fun publishConnected(
        candidate: MonitorRouteCandidate,
        snapshot: MonitorSnapshot,
    ) {
        if (stopped.get()) return
        val previousUrl = activeCandidate.get()?.snapshotUrl
        consecutiveFailures = 0
        activeRouteFailures = 0
        activeCandidate.set(candidate)
        if (previousUrl != null && previousUrl != candidate.snapshotUrl) {
            preferredRecoverySuccesses = 0
        }
        if (persistActiveRoute) {
            store.setLastEndpoint(
                config.desktopId,
                config.deviceId,
                candidate.snapshotUrl,
            )
        }
        listener.onTransportState(
            MonitorTransportState(
                phase = "connected",
                transportKind = candidate.kind,
                url = candidate.snapshotUrl,
                desktopOnline = true,
            ),
        )
        listener.onSnapshot(snapshot)
    }

    private fun fetchFailureMessage(result: FetchResult): String = when (result) {
        is FetchResult.Offline -> result.message
        is FetchResult.Failure -> result.message
        is FetchResult.Snapshot -> "Monitor HTTPS 请求失败。"
    }

    private fun publishFailure(
        message: String,
        definitelyOffline: Boolean,
        candidate: MonitorRouteCandidate? = activeCandidate.get(),
    ) {
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
                transportKind = candidate?.kind,
                url = candidate?.snapshotUrl,
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
        private const val TAG = "ChatXMonitorPoll"
        private const val AUTO_FAILOVER_FAILURE_THRESHOLD = 2
        private const val AUTO_RECOVERY_SUCCESS_THRESHOLD = 2
        private const val AUTO_RECOVERY_PROBE_INTERVAL_MS = 20_000L

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
                persistActiveRoute = false,
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


/** Detects missing periodic work, including a scheduled task cancelled by an exception. */
internal object MonitorPollWatchdog {
    fun isStalled(lastCompletedAt: Long, now: Long, pollIntervalSeconds: Long): Boolean {
        if (lastCompletedAt <= 0L || now < lastCompletedAt) return false
        val deadlineMs = maxOf(90_000L, pollIntervalSeconds * 3_000L)
        return now - lastCompletedAt > deadlineMs
    }
}
