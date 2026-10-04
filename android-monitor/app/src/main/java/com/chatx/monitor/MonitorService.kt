package com.chatx.monitor

import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit

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
            "正在建立 ChatX WSS…",
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
                    transportReachable =
                        state.phase == "connected" &&
                            state.desktopOnline != false
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
        store.updateDirectEndpoints(snapshot.endpoints)
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

    private fun handleTransportState(state: MonitorTransportState) {
        val label = when (state.transportKind) {
            "lan" -> "LAN"
            "ipv6" -> "IPv6"
            "tailscale" -> "Tailscale"
            "relay" -> "ChatX Relay"
            else -> "WSS"
        }
        val text = when (state.phase) {
            "connected" -> "已连接 · $label"
            "desktop_offline" -> "电脑离线 · $label"
            "reconnecting" -> {
                if (state.reconnectAttempt > 0) {
                    "正在重新连接 · 第 ${state.reconnectAttempt} 次"
                } else {
                    "正在重新连接"
                }
            }
            "degraded" -> "连接异常 · $label"
            "closed" -> "实时监控已停止"
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

        fun start(context: Context) {
            val intent = Intent(context, MonitorService::class.java)
            if (Build.VERSION.SDK_INT >= 26) {
                context.startForegroundService(intent)
            } else {
                context.startService(intent)
            }
        }

        fun stop(context: Context) {
            context.stopService(Intent(context, MonitorService::class.java))
        }
    }
}
