package com.chatx.monitor

import android.content.Context
import android.os.Handler
import android.os.Looper
import java.io.Closeable
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicBoolean

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
        executor.execute {
            var notice: String? = null
            val controlAttempt = runCatching {
                MonitorControlClient(
                    appContext,
                    config,
                ).execute("refresh_snapshot", timeoutSeconds = 4L)
            }
            val json = controlAttempt.fold(
                onSuccess = { result ->
                    if (result.ok) {
                        controlSnapshotJson(result)
                            ?: StatusCodec.error(
                                "刷新响应缺少 Snapshot。",
                            )
                    } else {
                        notice = result.message
                            ?: "Desktop 拒绝刷新请求。"
                        null
                    }
                },
                onFailure = { controlError ->
                    runCatching {
                        StatusCodec.snapshot(
                            MonitorRepository(appContext)
                                .fetchSnapshot(),
                        )
                    }.getOrElse { fallbackError ->
                        StatusCodec.error(
                            fallbackError.message
                                ?: controlError.message
                                ?: "连接失败",
                        )
                    }
                },
            )
            if (json != null) {
                store.setLastSnapshotJson(json)
            }
            post {
                refreshing.set(false)
                if (json != null) listener.onSnapshot(json)
                listener.onStateChanged()
                notice?.let(listener::onToast)
            }
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
                val json = controlSnapshotJson(result)
                if (json != null) {
                    store.setLastSnapshotJson(json)
                }
                result to json
            }
            post {
                reconnecting.set(false)
                val outcome = attempt.getOrNull()
                val result = outcome?.first
                val json = outcome?.second
                if (json != null) listener.onSnapshot(json)
                listener.onStateChanged()
                listener.onToast(
                    when {
                        result == null ->
                            attempt.exceptionOrNull()?.message
                                ?: "Tunnel 重连请求失败。"
                        result.ok ->
                            result.message ?: "已触发 Tunnel 重连。"
                        else ->
                            result.message
                                ?: "Desktop 拒绝 Tunnel 重连请求。"
                    },
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
            val results = MonitorRepository(appContext).testEndpoints()
            post {
                testingEndpoints.set(false)
                listener.onEndpointHealth(results)
                listener.onStateChanged()
            }
        }
    }

    override fun close() {
        if (!closed.compareAndSet(false, true)) return
        mainHandler.removeCallbacksAndMessages(null)
        executor.shutdownNow()
    }

    private fun controlSnapshotJson(
        result: MonitorControlResult,
    ): String? {
        val root = result.snapshot ?: return null
        val snapshot = MonitorSnapshotParser.parse(
            root = root,
            endpointUrl = result.endpointUrl,
            transportKind = result.transportKind,
        )
        store.updateDirectEndpoints(snapshot.endpoints)
        return StatusCodec.snapshot(snapshot)
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
