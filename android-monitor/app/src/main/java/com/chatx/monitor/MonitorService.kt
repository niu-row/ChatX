package com.chatx.monitor

import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import org.json.JSONObject

class MonitorService : Service() {
    private val alertEngine = AlertEngine()
    private val timer = Executors.newSingleThreadScheduledExecutor()
    private lateinit var store: SecureStore
    private var connection: MonitorConnectionManager? = null

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
            { offlineTick() },
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
            connection?.stop()
            connection = null
            transportReachable = false
            startConnection()
            return START_STICKY
        }
        if (intent?.action == ACTION_REEVALUATE) {
            if (connection == null) startConnection() else connection?.reevaluatePolicy()
            return START_STICKY
        }
        if (connection == null) {
            startConnection()
        }
        return START_STICKY
    }

    private fun startConnection() {
        val config = store.loadPairing()
        if (config == null) {
            NotificationCenter.updateForeground(
                this,
                "尚未配对 ChatX",
            )
            return
        }
        connection = MonitorConnectionManager(
            applicationContext,
            config,
            object : MonitorConnectionManager.Listener {
                override fun onTransportState(state: MonitorTransportState) {
                    when (state.phase) {
                        "connected" -> transportReachable = state.desktopOnline != false
                        "desktop_offline", "closed" -> transportReachable = false
                        "degraded" -> if (
                            state.reconnectAttempt >= store.getOfflineFailureThreshold()
                        ) {
                            transportReachable = false
                        }
                    }
                    handleTransportState(state)
                }

                override fun onSnapshot(snapshot: MonitorSnapshot) {
                    transportReachable = true
                    handleSnapshot(snapshot)
                }
            },
        ).also { it.start() }
    }

    override fun onDestroy() {
        store.setMonitorServiceRunning(false)
        connection?.stop()
        connection = null
        timer.shutdownNow()
        super.onDestroy()
    }

    override fun onBind(intent: Intent?): IBinder? = null

    private fun handleSnapshot(snapshot: MonitorSnapshot) {
        if (isOlderThanStoredSnapshot(snapshot.serverTime)) return
        connection?.updateRoutes(snapshot.endpoints)
        val json = StatusCodec.snapshot(snapshot)
        store.setLastSnapshotJson(json)
        NotificationCenter.updateForeground(
            this,
            StatusCodec.foregroundText(snapshot),
        )
        val thresholds = store.getAlertThresholds()
        alertEngine.onSnapshot(
            snapshot,
            thresholds,
            store.continuousModeStartedAt(),
        ).forEach(::postAlert)
        broadcast(json)
    }

    private fun isOlderThanStoredSnapshot(serverTime: Long): Boolean {
        if (serverTime <= 0L) return false
        val previous = store.getLastSnapshotJson()
            ?.let { raw ->
                runCatching {
                    JSONObject(raw).optLong("serverTime", 0L)
                }.getOrDefault(0L)
            }
            ?: 0L
        return previous > 0L && serverTime < previous
    }

    private fun handleTransportState(state: MonitorTransportState) {
        if (state.phase == "degraded" && transportReachable) {
            return
        }
        val label = when (state.transportKind) {
            "lan" -> "LAN"
            "ipv6" -> "IPv6"
            "tailscale" -> "Tailscale"
            "relay" -> "ChatX Relay"
            else -> "HTTPS"
        }
        val text = when (state.phase) {
            "connected" -> "监控正常 · $label"
            "desktop_offline" -> "电脑离线 · $label"
            "reconnecting", "degraded" -> "连接波动，正在重试 · $label"
            "closed" -> "监控已停止"
            else -> "正在寻找 ChatX…"
        }
        NotificationCenter.updateForeground(this, text)

        if (!transportReachable &&
            state.phase in setOf("desktop_offline", "reconnecting", "degraded")
        ) {
            val json = StatusCodec.transportError(state)
            store.setLastSnapshotJson(json)
            broadcast(json)
            checkOfflineAlerts()
        }
    }

    private fun offlineTick() {
        if (!transportReachable) {
            checkOfflineAlerts()
        }
    }

    private fun checkOfflineAlerts() {
        val now = System.currentTimeMillis()
        alertEngine.onUnreachable(
            now,
            store.getAlertThresholds(),
        ).forEach(::postAlert)
    }

    private fun postAlert(event: AlertEvent) {
        NotificationCenter.postAlert(this, event)
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
