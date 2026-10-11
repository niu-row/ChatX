package com.chatx.monitor

import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.util.Log
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import org.json.JSONObject

class MonitorService : Service() {
    private val alertEngine = AlertEngine()
    private val alertDispatchLock = Any()
    private val timer = Executors.newSingleThreadScheduledExecutor()
    private val mainHandler = Handler(Looper.getMainLooper())
    private lateinit var store: SecureStore
    @Volatile private var connection: MonitorConnectionManager? = null
    @Volatile private var activePairing: PairingConfig? = null
    @Volatile private var connectionGeneration = 0L

    @Volatile
    private var transportReachable = false

    override fun onCreate() {
        super.onCreate()
        store = SecureStore(applicationContext)
        NotificationCenter.createChannels(this)
        store.setMonitorServiceRunning(true)
        val notification = NotificationCenter.foreground(
            this,
            "正在建立 ChatX HTTPS 监控…",
        )
        if (Build.VERSION.SDK_INT >= 34) {
            startForeground(
                NotificationCenter.FOREGROUND_ID,
                notification,
                ServiceInfo.FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE,
            )
        } else {
            startForeground(NotificationCenter.FOREGROUND_ID, notification)
        }
        timer.scheduleAtFixedRate(
            {
                runCatching { offlineTick() }
                    .onFailure { Log.e(TAG, "Monitor watchdog tick failed", it) }
            },
            30,
            30,
            TimeUnit.SECONDS,
        )
    }

    override fun onStartCommand(
        intent: Intent?,
        flags: Int,
        startId: Int,
    ): Int {
        if (intent?.action == ACTION_STOP) {
            stopSelf()
            return START_NOT_STICKY
        }
        if (intent?.action == ACTION_RELOAD) {
            restartConnection()
            return START_STICKY
        }
        if (intent?.action == ACTION_REEVALUATE) {
            when {
                connection == null -> startConnection()
                connection?.isPollerStalled() == true -> restartConnection()
                else -> connection?.reevaluatePolicy()
            }
            return START_STICKY
        }
        if (connection == null) {
            startConnection()
        }
        return START_STICKY
    }

    private fun restartConnection() {
        connectionGeneration += 1 // Invalidate callbacks from the old worker first.
        connection?.stop()
        connection = null
        activePairing = null
        transportReachable = false
        startConnection()
    }

    private fun startConnection() {
        val config = store.loadPairing()
        if (config == null) {
            updateForegroundStatus("尚未配对 ChatX")
            return
        }
        val generation = ++connectionGeneration
        activePairing = config
        connection = MonitorConnectionManager(
            applicationContext,
            config,
            object : MonitorConnectionManager.Listener {
                override fun onTransportState(state: MonitorTransportState) {
                    if (!isActivePairing(config, generation)) return
                    when (state.phase) {
                        "connected" -> transportReachable = state.desktopOnline != false
                        "desktop_offline", "closed" -> transportReachable = false
                        "degraded" -> if (
                            state.reconnectAttempt >= store.getOfflineFailureThreshold()
                        ) {
                            transportReachable = false
                        }
                    }
                    handleTransportState(config, state, generation)
                }

                override fun onSnapshot(snapshot: MonitorSnapshot) {
                    if (!isActivePairing(config, generation)) return
                    transportReachable = true
                    handleSnapshot(config, snapshot, generation)
                }
            },
        ).also { it.start() }
    }

    override fun onDestroy() {
        connectionGeneration += 1
        store.setMonitorServiceRunning(false)
        connection?.stop()
        connection = null
        activePairing = null
        timer.shutdownNow()
        mainHandler.removeCallbacksAndMessages(null)
        super.onDestroy()
    }

    override fun onBind(intent: Intent?): IBinder? = null

    private fun handleSnapshot(
        config: PairingConfig,
        snapshot: MonitorSnapshot,
        generation: Long,
    ) {
        if (!isActivePairing(config, generation)) return
        val json = StatusCodec.snapshot(snapshot)
        if (!store.setLastSnapshotJson(
                config.desktopId,
                config.deviceId,
                json,
            )
        ) {
            return
        }
        connection?.updateRoutes(snapshot.endpoints)
        updateForegroundStatus(StatusCodec.foregroundText(snapshot))
        synchronized(alertDispatchLock) {
            val thresholds = store.getAlertThresholds()
            alertEngine.onSnapshot(
                snapshot,
                thresholds,
                store.continuousModeStartedAt(),
            ).forEach(::postAlert)
        }
        broadcast(json)
    }

    private fun isActivePairing(config: PairingConfig, generation: Long): Boolean {
        if (generation != connectionGeneration) return false
        val active = activePairing ?: return false
        return active.desktopId == config.desktopId &&
            active.deviceId == config.deviceId &&
            store.hasPairing(config.desktopId, config.deviceId)
    }

    private fun handleTransportState(
        config: PairingConfig,
        state: MonitorTransportState,
        generation: Long,
    ) {
        if (!isActivePairing(config, generation)) return
        if (state.phase == "degraded" && transportReachable) {
            return
        }
        val label = when (state.transportKind) {
            "lan" -> "LAN"
            "ipv6" -> "IPv6"
            "relay" -> "服务器 Relay"
            else -> "HTTPS"
        }
        val text = when (state.phase) {
            "connected" -> "监控正常 · $label"
            "desktop_offline" -> "电脑离线 · $label"
            "reconnecting", "degraded" -> "连接波动，正在重试 · $label"
            "closed" -> "监控已停止"
            else -> "正在寻找 ChatX…"
        }
        updateForegroundStatus(text)

        if (!transportReachable &&
            state.phase in setOf("desktop_offline", "reconnecting", "degraded")
        ) {
            val json = StatusCodec.transportError(state)
            if (!store.setLastSnapshotJson(
                    config.desktopId,
                    config.deviceId,
                    json,
                )
            ) {
                return
            }
            broadcast(json)
            checkOfflineAlerts()
        }
    }

    private fun offlineTick() {
        val current = connection
        if (current?.isPollerStalled() == true) {
            mainHandler.post {
                if (current === connection && current.isPollerStalled()) {
                    val generation = connectionGeneration
                    activePairing?.let { config ->
                        transportReachable = false
                        runCatching {
                            handleTransportState(
                                config,
                                MonitorTransportState(
                                    phase = "reconnecting",
                                    error = "Monitor 轮询中断，正在自动重建连接。",
                                ),
                                generation,
                            )
                        }.onFailure { Log.e(TAG, "Failed to report stalled poller", it) }
                    }
                    Log.w(TAG, "Monitor poller stalled; rebuilding polling connection")
                    runCatching { restartConnection() }
                        .onFailure { Log.e(TAG, "Failed to restart Monitor polling", it) }
                }
            }
        } else if (current == null) {
            // Retry when a transient pairing/Keystore failure prevented startup.
            mainHandler.post {
                runCatching {
                    if (connection == null && store.loadPairing() != null) startConnection()
                }.onFailure { Log.e(TAG, "Failed to restore Monitor polling", it) }
            }
        }
        if (!transportReachable) checkOfflineAlerts()
    }

    private fun checkOfflineAlerts() = synchronized(alertDispatchLock) {
        val now = System.currentTimeMillis()
        alertEngine.onUnreachable(
            now,
            store.getAlertThresholds(),
        ).forEach(::postAlert)
    }

    private fun updateForegroundStatus(text: String) {
        runCatching { NotificationCenter.updateForeground(this, text) }
            .onFailure { Log.e(TAG, "Failed to update Monitor notification", it) }
    }

    private fun postAlert(event: AlertEvent) {
        runCatching { NotificationCenter.postAlert(this, event) }
            .onFailure { Log.e(TAG, "Failed to post Monitor alert", it) }
        store.appendEvent(
            MonitorEvent(
                at = System.currentTimeMillis(),
                type = event.type.name,
                title = event.title,
                message = event.message,
            ),
        )
    }

    private fun broadcast(json: String) {
        sendBroadcast(
            Intent(ACTION_STATUS)
                .setPackage(packageName)
                .putExtra(EXTRA_STATUS_JSON, json),
        )
    }

    companion object {
        private const val TAG = "ChatXMonitorService"
        const val ACTION_STATUS = "com.chatx.monitor.STATUS"
        const val EXTRA_STATUS_JSON = "status_json"
        private const val ACTION_STOP = "com.chatx.monitor.STOP"
        private const val ACTION_REEVALUATE = "com.chatx.monitor.REEVALUATE"
        private const val ACTION_RELOAD = "com.chatx.monitor.RELOAD"

        fun start(context: Context) {
            val intent = Intent(context, MonitorService::class.java)
            if (Build.VERSION.SDK_INT >= 26) {
                context.startForegroundService(intent)
            } else {
                context.startService(intent)
            }
        }

        fun reevaluate(context: Context) {
            context.startService(
                Intent(context, MonitorService::class.java).setAction(ACTION_REEVALUATE),
            )
        }

        fun reload(context: Context) {
            context.startService(
                Intent(context, MonitorService::class.java).setAction(ACTION_RELOAD),
            )
        }

        fun stop(context: Context) {
            context.stopService(Intent(context, MonitorService::class.java))
        }
    }
}


/** Prevents cached high timestamps from blocking valid snapshots indefinitely. */
internal object MonitorSnapshotFreshness {
    fun shouldRejectNonNewer(
        previousServerTime: Long,
        previousReceivedAt: Long,
        serverTime: Long,
        now: Long,
        staleAfterMs: Long,
    ): Boolean {
        if (serverTime <= 0L || previousServerTime <= 0L) return false
        if (serverTime > previousServerTime) return false
        if (serverTime < previousServerTime) {
            return shouldRejectOlder(
                previousServerTime, previousReceivedAt, serverTime, now, staleAfterMs,
            )
        }
        return previousReceivedAt > 0L && now >= previousReceivedAt &&
            now - previousReceivedAt <= staleAfterMs
    }

    fun shouldRejectOlder(
        previousServerTime: Long,
        previousReceivedAt: Long,
        serverTime: Long,
        now: Long,
        staleAfterMs: Long,
    ): Boolean {
        if (serverTime <= 0L || previousServerTime <= 0L || serverTime >= previousServerTime) {
            return false
        }
        if (previousReceivedAt <= 0L || now < previousReceivedAt) return false
        return now - previousReceivedAt <= staleAfterMs
    }
}
