package com.chatx.monitor

import android.content.Context
import android.os.Handler
import android.os.Looper
import java.io.Closeable
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicBoolean

class PairingActionController(
    context: Context,
    private val store: SecureStore,
    private val listener: Listener,
) : Closeable {
    interface Listener {
        fun onPairingSucceeded(config: PairingConfig)
        fun onPairingFailed(message: String)
        fun onRevokeSucceeded()
        fun onRevokeFailed(message: String)
    }

    private val appContext = context.applicationContext
    private val executor = Executors.newSingleThreadExecutor()
    private val mainHandler = Handler(Looper.getMainLooper())
    private val closed = AtomicBoolean(false)
    private val pairing = AtomicBoolean(false)
    private val revoking = AtomicBoolean(false)

    fun pair(text: String) {
        if (closed.get() || !pairing.compareAndSet(false, true)) return
        executor.execute {
            val result = runCatching { PairingManager().pair(text) }
            if (!closed.get()) {
                result.getOrNull()?.let(store::savePairing)
            }
            post {
                pairing.set(false)
                result.fold(
                    onSuccess = listener::onPairingSucceeded,
                    onFailure = { error ->
                        listener.onPairingFailed(
                            error.message ?: "配对失败。",
                        )
                    },
                )
            }
        }
    }

    fun revoke(config: PairingConfig) {
        if (closed.get() || !revoking.compareAndSet(false, true)) return
        executor.execute {
            val result = runCatching {
                MonitorConnectionManager.revokePairing(
                    appContext,
                    config,
                )
                store.clearPairing()
                store.setContinuousMode(false)
            }
            post {
                revoking.set(false)
                result.fold(
                    onSuccess = { listener.onRevokeSucceeded() },
                    onFailure = { error ->
                        listener.onRevokeFailed(
                            error.message ?: "无法连接 ChatX 撤销设备。",
                        )
                    },
                )
            }
        }
    }

    override fun close() {
        if (!closed.compareAndSet(false, true)) return
        mainHandler.removeCallbacksAndMessages(null)
        executor.shutdownNow()
    }

    private fun post(block: () -> Unit) {
        if (closed.get()) return
        mainHandler.post {
            if (!closed.get()) block()
        }
    }
}
