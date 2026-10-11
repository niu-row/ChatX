package com.chatx.monitor

import android.content.Context
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import java.io.Closeable
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicReference

class MonitorActionController(
    context: Context,
    private val store: SecureStore,
    private val listener: Listener,
) : Closeable {
    interface Listener {
        fun onStateChanged()
        fun onSnapshot(json: String)
        fun onEndpointHealth(results: List<EndpointHealth>?)
        fun onToast(message: String)
    }

    private val appContext = context.applicationContext
    private val executor = Executors.newFixedThreadPool(3)
    private val mainHandler = Handler(Looper.getMainLooper())
    private val closed = AtomicBoolean(false)
    private val refreshing = AtomicBoolean(false)
    private val reconnecting = AtomicBoolean(false)
    private val testingEndpoints = AtomicBoolean(false)

    fun isRefreshing(): Boolean = refreshing.get()
    fun isReconnecting(): Boolean = reconnecting.get()

    fun isTestingEndpoints(): Boolean = testingEndpoints.get()

    fun refresh(config: PairingConfig) {
        if (closed.get() || !refreshing.compareAndSet(false, true)) return
        notifyStateChanged()

        // Never render a cached HTTPS response only to replace it seconds later
        // with the fresh control response. Publish at most once per manual refresh.
        val selector = ManualRefreshSelector()
        val publishLock = Any()
        val lastError = AtomicReference<String?>(null)

        fun complete(fresh: Boolean, json: String?, error: String?) {
            val choice = synchronized(publishLock) {
                if (!error.isNullOrBlank()) lastError.set(error)
                selector.complete(fresh, json)
            } ?: return
            choice.json?.let { selected ->
                store.setLastSnapshotJson(config.desktopId, config.deviceId, selected)
            }
            post {
                refreshing.set(false)
                if (choice.json != null) {
                    listener.onSnapshot(store.getLastSnapshotJson() ?: choice.json)
                } else {
                    listener.onToast(lastError.get() ?: "Monitor 刷新失败。")
                }
                listener.onStateChanged()
            }
        }

        executor.execute {
            val attempt = runCatching {
                StatusCodec.snapshot(
                    MonitorRepository(appContext).fetchSnapshot(),
                )
            }
            complete(
                fresh = false,
                json = attempt.getOrNull(),
                error = attempt.exceptionOrNull()?.message,
            )
        }

        executor.execute {
            val attempt = runCatching {
                val result = MonitorControlClient(
                    appContext,
                    config,
                ).execute("refresh_snapshot", timeoutSeconds = 3L)
                if (!result.ok) {
                    error(result.message ?: "Desktop 拒绝实时刷新请求。")
                }
                controlSnapshotJson(config, result)
                    ?: error("实时刷新响应缺少 Snapshot。")
            }
            complete(
                fresh = true,
                json = attempt.getOrNull(),
                error = attempt.exceptionOrNull()?.message,
            )
        }
    }

    fun reconnect(config: PairingConfig) {
        if (closed.get() || !reconnecting.compareAndSet(false, true)) return
        notifyStateChanged()
        executor.execute {
            val attempt = runCatching {
                val result = MonitorControlClient(
                    appContext,
                    config,
                ).execute("reconnect_tunnel", timeoutSeconds = 5L)
                val initialJson = controlSnapshotJson(config, result)
                if (initialJson != null) {
                    store.setLastSnapshotJson(
                        config.desktopId,
                        config.deviceId,
                        initialJson,
                    )
                }
                if (!result.ok) {
                    initialJson to (
                        result.message ?: "Desktop 拒绝 Tunnel 重连请求。"
                    )
                } else {
                    followReconnect(config, initialJson)
                }
            }
            post {
                reconnecting.set(false)
                val outcome = attempt.getOrNull()
                val json = outcome?.first
                if (json != null) listener.onSnapshot(store.getLastSnapshotJson() ?: json)
                listener.onStateChanged()
                listener.onToast(
                    outcome?.second
                        ?: attempt.exceptionOrNull()?.message
                        ?: "Tunnel 重连请求失败。",
                )
            }
        }
    }

    fun testEndpoints(showToast: Boolean = true) {
        if (
            closed.get() ||
            !testingEndpoints.compareAndSet(false, true)
        ) {
            return
        }
        post {
            listener.onEndpointHealth(null)
            listener.onStateChanged()
            if (showToast) {
                listener.onToast("正在测试所有路径…")
            }
        }
        executor.execute {
            val result = runCatching { MonitorRepository(appContext).testEndpoints() }
            post {
                // A failed future/Keystore read must never lock the button in "testing".
                testingEndpoints.set(false)
                listener.onEndpointHealth(result.getOrNull() ?: emptyList())
                listener.onStateChanged()
                result.exceptionOrNull()?.let { failure ->
                    listener.onToast(
                        "路径检测失败：" + (failure.message ?: failure.javaClass.simpleName),
                    )
                }
            }
        }
    }

    override fun close() {
        if (!closed.compareAndSet(false, true)) return
        mainHandler.removeCallbacksAndMessages(null)
        executor.shutdownNow()
    }

    private fun controlSnapshotJson(
        config: PairingConfig,
        result: MonitorControlResult,
    ): String? {
        val root = result.snapshot ?: return null
        val snapshot = MonitorSnapshotParser.parse(
            root = root,
            endpointUrl = result.endpointUrl,
            transportKind = result.transportKind,
        )
        store.updateDirectEndpoints(
            config.desktopId,
            config.deviceId,
            snapshot.endpoints,
        )
        return StatusCodec.snapshot(snapshot)
    }

    private fun followReconnect(
        config: PairingConfig,
        initialJson: String?,
    ): Pair<String?, String> {
        var latestJson = initialJson
        val deadline = SystemClock.elapsedRealtime() + 24_000L
        while (!closed.get() && SystemClock.elapsedRealtime() < deadline) {
            Thread.sleep(1_500L)
            val snapshot = runCatching {
                MonitorRepository(appContext).fetchSnapshot()
            }.getOrNull() ?: continue
            latestJson = StatusCodec.snapshot(snapshot)
            store.setLastSnapshotJson(
                config.desktopId,
                config.deviceId,
                latestJson,
            )
            if (snapshot.tunnel.active && snapshot.tunnel.health == "healthy") {
                return latestJson to "Tunnel 已恢复 Ready。"
            }
            if (!snapshot.tunnel.desiredConnected) {
                return latestJson to "Desktop 已停止 Tunnel。"
            }
            if (snapshot.tunnel.reconnectPhase in setOf("blocked", "failed")) {
                return latestJson to (
                    snapshot.tunnel.lastReconnectError
                        ?: "Tunnel 重连未能恢复 Ready。"
                )
            }
        }
        return latestJson to "Tunnel 尚未恢复 Ready；ChatX 会继续按自动恢复策略处理。"
    }

    private fun notifyStateChanged() {
        post { listener.onStateChanged() }
    }

    private fun post(block: () -> Unit) {
        if (closed.get()) return
        mainHandler.post {
            if (!closed.get()) block()
        }
    }
}

internal data class ManualRefreshChoice(val json: String?)

// Coordinates concurrent cached HTTPS + live control results without UI rollback.
internal class ManualRefreshSelector {
    private var controlDone = false
    private var passiveDone = false
    private var completed = false
    private var controlJson: String? = null
    private var passiveJson: String? = null

    fun complete(fresh: Boolean, json: String?): ManualRefreshChoice? {
        if (completed) return null
        if (fresh) {
            controlDone = true
            controlJson = json
        } else {
            passiveDone = true
            passiveJson = json
        }
        if (controlJson == null && !(controlDone && passiveDone)) return null
        completed = true
        return ManualRefreshChoice(controlJson ?: passiveJson)
    }
}
