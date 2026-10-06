package com.chatx.monitor

import android.Manifest
import android.annotation.SuppressLint
import android.app.Activity
import android.app.AlertDialog
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.PowerManager
import android.provider.Settings
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.widget.CheckBox
import android.widget.EditText
import android.widget.ImageView
import android.widget.LinearLayout
import android.widget.RadioButton
import android.widget.RadioGroup
import android.widget.ScrollView
import android.widget.Switch
import android.widget.TextView
import android.widget.Toast
import com.google.mlkit.vision.barcode.common.Barcode
import com.google.mlkit.vision.codescanner.GmsBarcodeScannerOptions
import com.google.mlkit.vision.codescanner.GmsBarcodeScanning
import org.json.JSONObject
import java.text.SimpleDateFormat
import java.time.Instant
import java.util.Date
import java.util.Locale

class MainActivity : Activity() {
    private lateinit var store: SecureStore
    private lateinit var ui: UiKit
    private lateinit var pageContent: LinearLayout
    private lateinit var bottomNav: LinearLayout
    private lateinit var actions: MonitorActionController
    private var pairingInput: EditText? = null
    private var currentPage = Page.OVERVIEW
    private var latestStatusJson: String? = null
    private var endpointHealth: List<EndpointHealth>? = null
    private var pendingStart = false

    private val statusReceiver = object : BroadcastReceiver() {
        override fun onReceive(context: Context?, intent: Intent?) {
            val json = intent?.getStringExtra(MonitorService.EXTRA_STATUS_JSON) ?: return
            latestStatusJson = json
            if (currentPage == Page.OVERVIEW || currentPage == Page.CONNECTION) {
                renderCurrentPage()
            }
        }
    }
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        store = SecureStore(this)
        ui = UiKit(this)
        actions = buildActionController()
        latestStatusJson = store.getLastSnapshotJson()
        NotificationCenter.createChannels(this)
        setContentView(buildShell())
        registerStatusReceiver()
        renderApp()
    }

    override fun onResume() {
        super.onResume()
        if (::pageContent.isInitialized) renderCurrentPage()
    }

    override fun onDestroy() {
        actions.close()
        unregisterReceiver(statusReceiver)
        super.onDestroy()
    }

    private fun buildActionController(): MonitorActionController =
        MonitorActionController(
            applicationContext,
            store,
            object : MonitorActionController.Listener {
                override fun onStateChanged() {
                    if (::pageContent.isInitialized && !isDestroyed) {
                        renderCurrentPage()
                    }
                }

                override fun onSnapshot(json: String) {
                    latestStatusJson = json
                }

                override fun onEndpointHealth(
                    results: List<EndpointHealth>?,
                ) {
                    endpointHealth = results
                }

                override fun onToast(message: String) {
                    if (!isDestroyed) toast(message)
                }
            },
        )

    private fun buildShell(): View {
        val root = ui.column().apply {
            setBackgroundColor(ui.color(R.color.cx_bg))
        }
        root.addView(buildHeader(), ui.margin(bottom = 8))
        val scroll = ScrollView(this).apply {
            isFillViewport = true
            overScrollMode = View.OVER_SCROLL_NEVER
        }
        pageContent = ui.column().apply {
            val horizontal = ui.dp(ui.pageHorizontalPadding)
            setPadding(horizontal, ui.dp(6), horizontal, ui.dp(18))
        }
        scroll.addView(
            pageContent,
            ViewGroup.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.WRAP_CONTENT,
            ),
        )
        root.addView(
            scroll,
            LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                0,
                1f,
            ),
        )
        bottomNav = buildBottomNav()
        root.addView(bottomNav, ui.margin())
        return root
    }

    private fun buildHeader(): View {
        val row = ui.row().apply {
            setPadding(ui.dp(16), ui.dp(8), ui.dp(16), ui.dp(6))
            setOnApplyWindowInsetsListener { view, insets ->
                view.setPadding(
                    ui.dp(16),
                    insets.systemWindowInsetTop + ui.dp(8),
                    ui.dp(16),
                    ui.dp(6),
                )
                insets
            }
        }
        row.addView(ImageView(this).apply {
            setImageResource(R.drawable.ic_chatx_logo)
        }, ui.margin(width = ui.dp(32), height = ui.dp(32), right = 9))
        row.addView(
            ui.title("ChatX Monitor", 16f),
            ui.margin(width = 0, weight = 1f),
        )
        return row
    }

    private fun buildBottomNav(): LinearLayout {
        val nav = ui.row().apply {
            setPadding(ui.dp(10), ui.dp(8), ui.dp(10), ui.dp(10))
            setBackgroundColor(ui.color(R.color.cx_surface))
            setOnApplyWindowInsetsListener { view, insets ->
                view.setPadding(
                    ui.dp(10),
                    ui.dp(8),
                    ui.dp(10),
                    ui.dp(10) + insets.systemWindowInsetBottom,
                )
                insets
            }
        }
        Page.entries.forEach { page ->
            val item = TextView(this).apply {
                text = page.label
                textSize = 12f
                includeFontPadding = false
                gravity = Gravity.CENTER
                minHeight = ui.dp(48)
                isClickable = true
                isFocusable = true
                setPadding(ui.dp(8), ui.dp(10), ui.dp(8), ui.dp(10))
                setOnClickListener {
                    currentPage = page
                    renderCurrentPage()
                    updateBottomNav()
                }
                tag = page.name
            }
            nav.addView(item, ui.margin(width = 0, weight = 1f, left = 2, right = 2))
        }
        return nav
    }
    private fun renderApp() {
        val paired = store.loadPairing() != null
        bottomNav.visibility = if (paired) View.VISIBLE else View.GONE
        if (!paired) {
            renderOnboarding()
        } else {
            renderCurrentPage()
            updateBottomNav()
        }
    }

    private fun updateBottomNav() {
        for (index in 0 until bottomNav.childCount) {
            val item = bottomNav.getChildAt(index) as TextView
            val selected = item.tag == currentPage.name
            item.setTextColor(
                ui.color(if (selected) R.color.cx_primary else R.color.cx_text_muted),
            )
            item.background = if (selected) {
                ui.rounded(ui.color(R.color.cx_surface_soft), 12f)
            } else {
                null
            }
        }
    }

    private fun renderCurrentPage() {
        if (store.loadPairing() == null) {
            renderApp()
            return
        }
        pageContent.removeAllViews()
        when (currentPage) {
            Page.OVERVIEW -> renderOverview()
            Page.ALERTS -> renderAlerts()
            Page.CONNECTION -> renderConnection()
            Page.SETTINGS -> renderSettings()
        }
    }

    private fun pageHeading(title: String, subtitle: String) {
        pageContent.addView(ui.title(title, 21f), ui.margin(bottom = 4))
        pageContent.addView(ui.muted(subtitle, 13f), ui.margin(bottom = 12))
    }

    private fun renderOverview() {
        pageHeading("概览", "ChatX、Tunnel 与 MCP 的实时运行状态")
        val root = latestStatusJson?.let { runCatching { JSONObject(it) }.getOrNull() }
        val reachable = root?.optBoolean("reachable", false) == true
        val hero = ui.heroCard()
        val top = ui.row()
        val heroCopy = ui.column()
        heroCopy.addView(ui.eyebrow("HOST STATUS"))
        heroCopy.addView(
            ui.title(
                when {
                    root == null -> "等待状态"
                    reachable -> "ChatX 在线"
                    else -> "ChatX 不可达"
                },
                22f,
            ),
            ui.margin(top = 6),
        )
        val receivedAt = root?.optLong("receivedAt", 0L) ?: 0L
        heroCopy.addView(
            ui.muted(
                if (receivedAt > 0) "更新于 ${formatTime(receivedAt)}"
                else "尚未收到监控数据",
            ),
            ui.margin(top = 4),
        )
        top.addView(heroCopy, ui.margin(width = 0, weight = 1f))
        top.addView(
            ui.pill(
                if (reachable) "ONLINE" else if (root == null) "WAITING" else "OFFLINE",
                if (reachable) UiKit.Tone.SUCCESS
                else if (root == null) UiKit.Tone.NEUTRAL
                else UiKit.Tone.DANGER,
            ),
        )
        hero.addView(top)

        if (reachable) {
            val tunnel = root.getJSONObject("tunnel")
            val mcp = root.getJSONObject("mcp")
            val metrics = ui.row()
            val probeAt = tunnel.optLong("lastProbeAt", tunnel.optLong("updatedAt", 0L))
            val probeFresh = probeAt > 0L && (root.optLong("serverTime", 0L) - probeAt).coerceAtLeast(0L) <= 12_000L
            val tunnelHealth = tunnel.optString("health", if (tunnel.optBoolean("active")) "healthy" else "unknown")
            val tunnelActive = tunnel.optBoolean("active") && tunnelHealth == "healthy" && probeFresh
            val tunnelLabel = when {
                !probeFresh -> "Stale"
                tunnelHealth == "suspect" -> "Check"
                tunnelHealth == "down" -> "Down"
                tunnelActive -> "Ready"
                else -> "Stopped"
            }
            val tunnelTone = when (tunnelLabel) {
                "Ready" -> UiKit.Tone.SUCCESS
                "Check" -> UiKit.Tone.WARNING
                "Stopped" -> UiKit.Tone.NEUTRAL
                else -> UiKit.Tone.DANGER
            }
            val mcpState = mcp.optString("state", "idle")
            metrics.addView(
                ui.statusTile(
                    "Tunnel",
                    tunnelLabel,
                    tunnelTone,
                ),
                ui.margin(width = 0, weight = 1f, right = 4),
            )
            metrics.addView(
                ui.statusTile(
                    "MCP",
                    mcpState.uppercase(),
                    mcpTone(mcpState),
                ),
                ui.margin(width = 0, weight = 1f, left = 4, right = 4),
            )
            metrics.addView(
                ui.statusTile(
                    "Calls",
                    "${mcp.optInt("callsLastMinute", 0)}/min",
                    UiKit.Tone.PRIMARY,
                ),
                ui.margin(width = 0, weight = 1f, left = 4),
            )
            hero.addView(metrics, ui.margin(top = 16))
            val controlPlaneState = tunnel.optString("controlPlaneState", "unknown").lowercase()
            val controlPlaneTone = when (controlPlaneState) {
                "healthy" -> UiKit.Tone.SUCCESS
                "suspect", "starting" -> UiKit.Tone.WARNING
                "down" -> UiKit.Tone.DANGER
                else -> UiKit.Tone.NEUTRAL
            }
            val networkRow = ui.row()
            networkRow.addView(ui.pill("CONTROL ${controlPlaneState.uppercase()}", controlPlaneTone))
            val proxyMode = tunnel.optString("proxyMode", "direct").uppercase()
            val proxySource = tunnel.optString("proxySource").ifBlank { "direct" }
            networkRow.addView(
                ui.muted("Proxy $proxyMode · $proxySource", 12f),
                ui.margin(width = 0, weight = 1f, left = 10),
            )
            hero.addView(networkRow, ui.margin(top = 14))
            val controlPlaneReason = tunnel.optString("controlPlaneReason").trim()
            if (controlPlaneReason.isNotBlank()) {
                hero.addView(
                    ui.muted("Control Plane: $controlPlaneReason", 11f),
                    ui.margin(top = 7),
                )
            }
            val endpoint = root.optString("endpointUrl", "")
            hero.addView(
                ui.muted("当前路径  ${endpointLabel(endpoint)}"),
                ui.margin(top = 12),
            )
        } else if (root != null) {
            hero.addView(
                ui.muted(root.optString("error", "无法连接桌面 ChatX")),
                ui.margin(top = 14),
            )
        }
        pageContent.addView(hero, ui.margin(bottom = 14))

        if (reachable) {
            val host = root?.optJSONObject("host")
            if (host != null) {
                val hostCard = ui.card()
                val hostTop = ui.row()
                val hostCopy = ui.column()
                hostCopy.addView(ui.title(host.optString("deviceName", "Computer"), 16f))
                hostCopy.addView(
                    ui.muted("${host.optString("platform", "unknown")} · ${host.optString("arch", "unknown")}", 12f),
                    ui.margin(top = 3),
                )
                hostTop.addView(hostCopy, ui.margin(width = 0, weight = 1f, right = 8))
                val battery = host.optJSONObject("battery")
                if (battery != null && battery.optBoolean("present", false)) {
                    val percent = battery.optInt("percent", -1)
                    hostTop.addView(
                        ui.pill(
                            if (percent >= 0) "${percent}%${if (battery.optBoolean("charging", false)) " · CHARGING" else ""}" else "BATTERY",
                            UiKit.Tone.NEUTRAL,
                        ),
                    )
                }
                hostCard.addView(hostTop)
                pageContent.addView(hostCard, ui.margin(bottom = 14))
            }
        }

        if (reachable) {
            val statusRoot = root ?: return
            val tunnel = statusRoot.getJSONObject("tunnel")
            val mcp = statusRoot.getJSONObject("mcp")
            val mcpState = mcp.optString("state", "idle")
            val probeAt = tunnel.optLong("lastProbeAt", tunnel.optLong("updatedAt", 0L))
            val probeFresh = probeAt > 0L && (statusRoot.optLong("serverTime", 0L) - probeAt).coerceAtLeast(0L) <= 12_000L
            val tunnelHealth = tunnel.optString("health", "unknown")
            val tunnelDown = tunnel.optBoolean("desiredConnected", false) && (tunnelHealth == "down" || !probeFresh)
            val alert = when {
                tunnelDown -> Triple(
                    "Tunnel 已断开",
                    if (tunnel.optBoolean("reconnecting")) {
                        "ChatX 正在自动重连 Tunnel。"
                    } else {
                        "ChatX 在线，但 Secure MCP Tunnel 当前不可用。"
                    },
                    UiKit.Tone.DANGER,
                )
                mcpState == "gap" -> Triple(
                    "MCP 调用中断",
                    "已连续没有新调用 ${formatDuration(mcp.optLong("gapDurationMs", 0L))}。",
                    UiKit.Tone.DANGER,
                )
                mcpState == "stalled" -> Triple(
                    "MCP 工具可能卡住",
                    "当前调用已执行 ${formatDuration(mcp.optLong("stallDurationMs", 0L))}。",
                    UiKit.Tone.WARNING,
                )
                else -> null
            }
            if (alert != null) {
                val card = ui.card()
                val row = ui.row()
                val copy = ui.column()
                copy.addView(ui.title(alert.first, 16f))
                copy.addView(ui.muted(alert.second, 12f), ui.margin(top = 4))
                row.addView(copy, ui.margin(width = 0, weight = 1f, right = 8))
                row.addView(ui.pill("ATTENTION", alert.third))
                card.addView(row)
                if (
                    tunnelDown &&
                    !tunnel.optBoolean("reconnecting", false)
                ) {
                    card.addView(
                        ui.button(
                            if (actions.isReconnecting()) "重连中…" else "立即重连 Tunnel",
                            danger = true,
                        ) {
                            reconnectTunnel()
                        },
                        ui.margin(top = 12),
                    )
                }
                pageContent.addView(card, ui.margin(bottom = 14))
            }
        }

        val serviceCard = ui.card()
        val running = store.isMonitorServiceRunning()
        val serviceTop = ui.row()
        val serviceCopy = ui.column()
        serviceCopy.addView(ui.title("实时监控", 16f))
        serviceCopy.addView(
            ui.muted(
                if (running) "前台服务正在持续检查 ChatX"
                else "当前仅在打开 App 时手动查看",
            ),
            ui.margin(top = 3),
        )
        serviceTop.addView(serviceCopy, ui.margin(width = 0, weight = 1f))
        serviceTop.addView(
            ui.pill(
                if (running) "运行中" else "已停止",
                if (running) UiKit.Tone.SUCCESS else UiKit.Tone.NEUTRAL,
            ),
        )
        serviceCard.addView(serviceTop)
        val serviceActions = ui.row()
        serviceActions.addView(
            ui.button(
                if (actions.isRefreshing()) "刷新中…" else "立即刷新",
                primary = true,
            ) {
                refreshOnce()
            },
            ui.margin(width = 0, weight = 1f, right = 5),
        )
        serviceActions.addView(
            ui.button(if (running) "停止后台" else "开始监控") {
                if (running) {
                    MonitorService.stop(this)
                    store.setMonitorServiceRunning(false)
                    toast("实时监控已停止。")
                    renderCurrentPage()
                } else {
                    requestPermissionsAndStart()
                }
            },
            ui.margin(width = 0, weight = 1f, left = 5),
        )
        serviceCard.addView(serviceActions, ui.margin(top = 16))
        pageContent.addView(serviceCard, ui.margin(bottom = 14))

        if (reachable) {
            val mcp = root.getJSONObject("mcp")
            pageContent.addView(
                buildRecentCallsCard(mcp),
                ui.margin(bottom = 14),
            )
        }
    }

    private fun buildRecentCallsCard(mcp: JSONObject): View {
        val card = ui.card()
        val header = ui.row()
        val copy = ui.column()
        copy.addView(ui.title("最近调用", 16f))
        copy.addView(
            ui.muted(
                "仅显示工具名、时间、耗时和结果，不包含参数或返回内容。",
                12f,
            ),
            ui.margin(top = 3),
        )
        header.addView(copy, ui.margin(width = 0, weight = 1f, right = 8))
        val inFlight = mcp.optInt("inFlight", 0)
        header.addView(
            ui.pill(
                if (inFlight > 0) "${inFlight} RUNNING"
                else "${mcp.optInt("callsLastMinute", 0)}/min",
                if (inFlight > 0) UiKit.Tone.WARNING else UiKit.Tone.NEUTRAL,
            ),
        )
        card.addView(header)

        val calls = mcp.optJSONArray("recentCalls")
        if (calls == null || calls.length() == 0) {
            card.addView(
                ui.muted("还没有可展示的 MCP 调用记录。"),
                ui.margin(top = 14),
            )
            return card
        }

        val count = minOf(calls.length(), 6)
        for (index in 0 until count) {
            val item = calls.optJSONObject(index) ?: continue
            if (index > 0) {
                card.addView(
                    ui.divider(),
                    ui.margin(height = 1, top = 11, bottom = 11),
                )
            }
            val row = ui.row()
            val left = ui.column()
            left.addView(
                ui.body(item.optString("toolName", "unknown"), 14f),
            )
            left.addView(
                ui.muted(recentCallTime(item), 12f),
                ui.margin(top = 3),
            )
            row.addView(left, ui.margin(width = 0, weight = 1f, right = 8))

            val right = ui.row()
            right.addView(
                ui.muted(
                    formatCallDuration(item.optLongOrNull("durationMs")),
                    12f,
                ),
                ui.margin(right = 8),
            )
            val running = item.optBoolean("running", false)
            val success = if (
                item.has("success") && !item.isNull("success")
            ) item.optBoolean("success") else null
            right.addView(
                ui.pill(
                    when {
                        running -> "RUNNING"
                        success == true -> "OK"
                        success == false -> "ERROR"
                        else -> "—"
                    },
                    when {
                        running -> UiKit.Tone.WARNING
                        success == true -> UiKit.Tone.SUCCESS
                        success == false -> UiKit.Tone.DANGER
                        else -> UiKit.Tone.NEUTRAL
                    },
                ),
            )
            row.addView(right)
            card.addView(row)
        }
        return card
    }

    private fun mcpTone(state: String): UiKit.Tone = when (state) {
        "active" -> UiKit.Tone.SUCCESS
        "gap" -> UiKit.Tone.DANGER
        "stalled" -> UiKit.Tone.WARNING
        else -> UiKit.Tone.NEUTRAL
    }

    private fun recentCallTime(item: JSONObject): String {
        val startedAt = item.optLongOrNull("startedAt")
        if (startedAt != null) return formatClockTime(startedAt)
        val timestamp = item.optString("timestamp").takeIf { it.isNotBlank() }
            ?: return "—"
        return runCatching {
            formatClockTime(Instant.parse(timestamp).toEpochMilli())
        }.getOrDefault(timestamp.take(19))
    }

    private fun formatClockTime(value: Long): String =
        SimpleDateFormat("HH:mm:ss", Locale.getDefault()).format(Date(value))

    private fun formatCallDuration(value: Long?): String {
        val ms = value ?: return "—"
        return when {
            ms < 1_000 -> "${ms} ms"
            ms < 60_000 -> String.format(Locale.getDefault(), "%.1f s", ms / 1000.0)
            else -> "${ms / 60_000}m ${(ms % 60_000) / 1000}s"
        }
    }

    private fun formatDuration(value: Long): String {
        val totalSeconds = value.coerceAtLeast(0L) / 1000L
        val minutes = totalSeconds / 60L
        val seconds = totalSeconds % 60L
        return if (minutes > 0) "${minutes}分${seconds}秒" else "${seconds}秒"
    }

    private fun renderAlerts() {
        pageHeading("告警", "控制中断判定，并查看最近的异常与恢复事件")
        val continuous = ui.card()
        val top = ui.row()
        val copy = ui.column()
        copy.addView(ui.title("持续任务监控", 16f))
        copy.addView(
            ui.muted(
                "手动开启后，即使自动状态变为 idle，也会继续计算调用中断时间。",
            ),
            ui.margin(top = 4),
        )
        top.addView(copy, ui.margin(width = 0, weight = 1f, right = 12))
        val continuousSwitch = Switch(this).apply {
            isChecked = store.isContinuousMode()
            setOnCheckedChangeListener { _, checked ->
                store.setContinuousMode(checked)
                toast(
                    if (checked) "持续任务监控已开启。"
                    else "持续任务监控已关闭。",
                )
                renderAlertsPageOnly()
            }
        }
        top.addView(continuousSwitch)
        continuous.addView(top)
        if (store.isContinuousMode()) {
            continuous.addView(
                ui.pill("STRICT WATCH", UiKit.Tone.WARNING),
                ui.margin(
                    top = 12,
                    width = ViewGroup.LayoutParams.WRAP_CONTENT,
                ),
            )
        }
        pageContent.addView(continuous, ui.margin(bottom = 14))
        val thresholds = ui.card()
        thresholds.addView(ui.title("中断阈值", 16f))
        thresholds.addView(
            ui.muted("同一中断只会在选中的阈值到达时各提醒一次。"),
            ui.margin(top = 4, bottom = 8),
        )
        val saved = store.getAlertThresholds()
        val checks = linkedMapOf<Long, CheckBox>()
        listOf(1L, 2L, 3L, 5L).forEach { minutes ->
            val value = minutes * 60_000L
            val check = CheckBox(this).apply {
                text = "$minutes 分钟"
                isChecked = value in saved
                setTextColor(ui.color(R.color.cx_text))
            }
            checks[value] = check
            thresholds.addView(check)
        }
        thresholds.addView(
            ui.button("保存告警阈值", primary = true) {
                store.setAlertThresholds(
                    checks.filterValues { it.isChecked }.keys,
                )
                toast("告警阈值已保存。")
            },
            ui.margin(top = 10),
        )
        pageContent.addView(thresholds, ui.margin(bottom = 14))
        val history = ui.card()
        val historyTop = ui.row()
        historyTop.addView(
            ui.title("最近事件", 16f),
            ui.margin(width = 0, weight = 1f),
        )
        historyTop.addView(
            ui.button("清空") {
                store.clearEvents()
                renderCurrentPage()
            },
            ui.margin(width = ViewGroup.LayoutParams.WRAP_CONTENT),
        )
        history.addView(historyTop)
        val events = store.loadEvents().take(20)
        if (events.isEmpty()) {
            history.addView(
                ui.muted("暂无告警或恢复事件。"),
                ui.margin(top = 14),
            )
        } else {
            events.forEachIndexed { index, event ->
                if (index > 0) {
                    history.addView(
                        ui.divider(),
                        ui.margin(height = 1, top = 6, bottom = 6),
                    )
                }
                val eventRow = ui.row()
                val eventCopy = ui.column()
                eventCopy.addView(ui.body(event.title, 14f))
                eventCopy.addView(
                    ui.muted(
                        "${event.message} · ${formatTime(event.at)}",
                        12f,
                    ).apply {
                        maxLines = 2
                    },
                    ui.margin(top = 3),
                )
                eventRow.addView(
                    eventCopy,
                    ui.margin(width = 0, weight = 1f),
                )
                eventRow.addView(
                    ui.pill(
                        eventTypeLabel(event.type),
                        eventTone(event.type),
                    ),
                    ui.margin(left = 8),
                )
                history.addView(eventRow)
            }
        }
        pageContent.addView(history, ui.margin(bottom = 14))
    }

    private fun renderAlertsPageOnly() {
        currentPage = Page.ALERTS
        renderCurrentPage()
        updateBottomNav()
    }
    private fun renderConnection() {
        pageHeading(
            "连接",
            "Monitor over HTTPS · Direct / Relay 动态多路径",
        )
        val config = store.loadPairing() ?: return
        val currentEndpoint = currentEndpointUrl()
        val currentBase = currentEndpoint?.substringBefore('?')
        val routes = configuredRoutes(config)
        val policy = store.getRoutePolicy()
        val manualSelector = store.getManualRouteSelector()
        val configuredUrls = routes.map { it.url }.toSet()
        val testedUrls = endpointHealth?.map { it.endpoint.url }?.toSet()
        val needsHealthTest = testedUrls == null || testedUrls != configuredUrls

        val summary = ui.card()
        summary.addView(ui.title("已配对 ChatX", 16f))
        summary.addView(
            keyValue("Desktop", "${config.desktopId.take(20)}…"),
            ui.margin(top = 12),
        )
        summary.addView(
            keyValue(
                "当前路径",
                endpointLabel(currentEndpoint ?: "—"),
            ),
            ui.margin(top = 8),
        )
        summary.addView(
            keyValue("协议", "HTTPS · E2EE"),
            ui.margin(top = 8),
        )
        summary.addView(
            keyValue("加密", "AES-256-GCM"),
            ui.margin(top = 8),
        )
        pageContent.addView(summary, ui.margin(bottom = 14))

        val policyCard = ui.card()
        policyCard.addView(ui.title("路径策略", 16f))
        policyCard.addView(
            keyValue("当前策略", routePolicyLabel(policy)),
            ui.margin(top = 12),
        )
        if (policy == RoutePolicy.MANUAL) {
            policyCard.addView(
                keyValue(
                    "手动首选",
                    manualSelector?.let(::manualRouteLabel) ?: "尚未选择",
                ),
                ui.margin(top = 8),
            )
        }
        val policyActions = ui.row()
        policyActions.addView(
            ui.button("更改策略", primary = true) {
                showRoutePolicyDialog()
            },
            ui.margin(width = 0, weight = 1f, right = 5, top = 12),
        )
        if (policy == RoutePolicy.MANUAL) {
            policyActions.addView(
                ui.button("选择路径") {
                    showManualRouteDialog(routes)
                },
                ui.margin(width = 0, weight = 1f, left = 5, top = 12),
            )
        }
        policyCard.addView(policyActions)
        pageContent.addView(policyCard, ui.margin(bottom = 14))

        val routeCard = ui.card()
        val routesTop = ui.row()
        routesTop.addView(
            ui.title("可用路径", 16f),
            ui.margin(width = 0, weight = 1f),
        )
        routesTop.addView(
            ui.button(
                if (actions.isTestingEndpoints()) "检测中…" else "重新测试",
                primary = !actions.isTestingEndpoints(),
            ) {
                if (!actions.isTestingEndpoints()) testAllEndpoints()
            },
            ui.margin(width = ViewGroup.LayoutParams.WRAP_CONTENT),
        )
        routeCard.addView(routesTop)

        routes.forEachIndexed { index, endpoint ->
            if (index > 0) {
                routeCard.addView(
                    ui.divider(),
                    ui.margin(height = 1, top = 12, bottom = 12),
                )
            }
            val health = endpointHealth
                ?.firstOrNull { it.endpoint.url == endpoint.url }
            val isCurrent = endpoint.url == currentBase
            val isManualPreferred =
                policy == RoutePolicy.MANUAL &&
                    manualSelector?.matches(endpoint) == true
            val row = ui.row()
            val copy = ui.column()
            copy.addView(
                ui.body(
                    "${endpointKindLabel(endpoint.kind)} · ${endpoint.family.uppercase()}",
                    14f,
                ),
            )
            copy.addView(
                ui.compactLine(endpoint.url, 12f),
                ui.margin(top = 4),
            )
            if (isCurrent) {
                copy.addView(
                    ui.muted("当前使用路径", 12f),
                    ui.margin(top = 3),
                )
            }
            if (isManualPreferred) {
                copy.addView(
                    ui.muted("手动首选路径", 12f),
                    ui.margin(top = 3),
                )
            }
            row.addView(
                copy,
                ui.margin(width = 0, weight = 1f, right = 8),
            )
            row.addView(
                when {
                    health == null -> ui.pill(
                        when {
                            actions.isTestingEndpoints() -> "检测中"
                            isCurrent -> "CURRENT"
                            else -> "未测试"
                        },
                        if (isCurrent) {
                            UiKit.Tone.PRIMARY
                        } else {
                            UiKit.Tone.NEUTRAL
                        },
                    )
                    health.reachable -> ui.pill(
                        "${health.latencyMs} ms",
                        UiKit.Tone.SUCCESS,
                    )
                    else -> ui.pill("失败", UiKit.Tone.DANGER)
                },
            )
            routeCard.addView(row)
            if (health?.reachable == false && !health.error.isNullOrBlank()) {
                routeCard.addView(
                    ui.muted(health.error ?: "", 12f),
                    ui.margin(top = 5),
                )
            }
        }
        pageContent.addView(routeCard, ui.margin(bottom = 14))

        val manage = ui.card()
        manage.addView(ui.title("配对管理", 16f))
        manage.addView(
            ui.muted(
                "Direct Token、Relay Token 与 E2EE Device Key 分离；重新扫码会覆盖当前设备身份。",
            ),
            ui.margin(top = 4, bottom = 12),
        )
        val row = ui.row()
        row.addView(
            ui.button("扫描新二维码") { scanPairingQr() },
            ui.margin(width = 0, weight = 1f, right = 5),
        )
        row.addView(
            ui.button("删除此设备", danger = true) {
                confirmClearPairing()
            },
            ui.margin(width = 0, weight = 1f, left = 5),
        )
        manage.addView(row)
        pageContent.addView(manage, ui.margin(bottom = 12))

        if (needsHealthTest && !actions.isTestingEndpoints()) {
            pageContent.post { testAllEndpoints(showToast = false) }
        }
    }

    private fun renderSettings() {
        pageHeading("设置", "HTTPS 监控、系统权限与端到端安全")

        val realtime = ui.card()
        realtime.addView(ui.title("稳定监控", 16f))
        realtime.addView(
            ui.muted(
                "Monitor 使用短连接 HTTPS 轮询，不依赖常驻 WebSocket。",
            ),
            ui.margin(top = 4),
        )
        realtime.addView(
            keyValue(
                "后台服务",
                if (store.isMonitorServiceRunning()) "正在运行" else "已停止",
            ),
            ui.margin(top = 12),
        )
        realtime.addView(
            settingPickerRow(
                "刷新间隔",
                "${store.getPollIntervalSeconds()} 秒",
            ) {
                showPollIntervalDialog()
            },
            ui.margin(top = 8),
        )
        realtime.addView(
            settingPickerRow(
                "快照过期",
                "${store.getSnapshotStaleSeconds()} 秒",
            ) {
                showSnapshotStaleDialog()
            },
            ui.margin(top = 8),
        )
        realtime.addView(
            settingPickerRow(
                "离线确认",
                "连续 ${store.getOfflineFailureThreshold()} 次请求失败",
            ) {
                showOfflineFailureDialog()
            },
            ui.margin(top = 8),
        )
        pageContent.addView(realtime, ui.margin(bottom = 14))

        val permissions = ui.card()
        permissions.addView(ui.title("系统权限", 16f))
        permissions.addView(
            permissionRow(
                "通知",
                Build.VERSION.SDK_INT < 33 ||
                    checkSelfPermission(
                        Manifest.permission.POST_NOTIFICATIONS,
                    ) == PackageManager.PERMISSION_GRANTED,
            ),
            ui.margin(top = 12),
        )
        permissions.addView(
            permissionRow(
                "本地网络",
                Build.VERSION.SDK_INT < 37 ||
                    checkSelfPermission(
                        ACCESS_LOCAL_NETWORK,
                    ) == PackageManager.PERMISSION_GRANTED,
            ),
            ui.margin(top = 8),
        )
        val power = getSystemService(PowerManager::class.java)
        permissions.addView(
            permissionRow(
                "电池优化豁免",
                power?.isIgnoringBatteryOptimizations(packageName) == true,
            ),
            ui.margin(top = 8),
        )
        permissions.addView(
            ui.button("打开应用系统设置") {
                openBatterySettings()
            },
            ui.margin(top = 14),
        )
        pageContent.addView(permissions, ui.margin(bottom = 14))

        val security = ui.card()
        security.addView(ui.title("安全", 16f))
        security.addView(
            ui.muted(
                "Direct HTTPS 使用 Desktop 自签证书 SHA-256 fingerprint pinning。",
                13f,
            ),
            ui.margin(top = 8),
        )
        security.addView(
            ui.muted(
                "LAN / IPv6 / Tailscale / Relay 全部使用同一套 AES-256-GCM E2EE envelope。",
                13f,
            ),
            ui.margin(top = 5),
        )
        security.addView(
            ui.muted(
                "Direct Token、Relay Token 与 Device Key 相互独立，并由 Android Keystore 加密保存。",
                13f,
            ),
            ui.margin(top = 5),
        )
        security.addView(
            ui.muted(
                "ChatX Relay 只路由 E2EE Snapshot 与受限控制密文；不持有 Runtime Key，也不提供 Shell / MCP 通用执行。",
                13f,
            ),
            ui.margin(top = 5),
        )
        security.addView(
            ui.muted("App ${BuildConfig.VERSION_NAME}", 12f),
            ui.margin(top = 12),
        )
        pageContent.addView(security, ui.margin(bottom = 14))
    }

    private fun renderOnboarding() {
        pageContent.removeAllViews()
        pageContent.addView(
            ui.eyebrow("PAIRING"),
            ui.margin(top = 10),
        )
        pageContent.addView(
            ui.title("连接你的 ChatX", 24f),
            ui.margin(top = 5),
        )
        pageContent.addView(
            ui.muted(
                "在 Mac 的 ChatX → 手机监控中生成配对二维码，然后用这里扫描。",
            ),
            ui.margin(top = 6, bottom = 18),
        )
        val hero = ui.heroCard()
        val logo = ImageView(this).apply {
            setImageResource(R.drawable.ic_chatx_logo)
        }
        hero.addView(
            logo,
            ui.margin(
                width = ui.dp(56),
                height = ui.dp(56),
                bottom = 16,
            ),
        )
        hero.addView(ui.title("安全配对", 20f))
        hero.addView(
            ui.muted(
                "二维码只包含短期 Pairing Code、Desktop 身份、WSS 候选和 TLS 指纹；长期设备凭据仅在成功配对后下发。",
            ),
            ui.margin(top = 6),
        )
        hero.addView(
            ui.button("扫描 ChatX 二维码", primary = true) {
                scanPairingQr()
            },
            ui.margin(top = 18),
        )
        pageContent.addView(hero, ui.margin(bottom = 14))

        val manual = ui.card()
        manual.addView(ui.title("手动导入", 16f))
        manual.addView(
            ui.muted("扫码不可用时，可以粘贴桌面端显示的配对 JSON。"),
            ui.margin(top = 4),
        )
        pairingInput = EditText(this).apply {
            hint = "粘贴配对 JSON"
            minLines = 4
            gravity = Gravity.TOP
            setTextColor(ui.color(R.color.cx_text))
            setHintTextColor(ui.color(R.color.cx_text_muted))
            background = ui.rounded(
                ui.color(R.color.cx_bg),
                12f,
                ui.color(R.color.cx_border),
            )
            setPadding(
                ui.dp(12),
                ui.dp(12),
                ui.dp(12),
                ui.dp(12),
            )
        }
        manual.addView(pairingInput, ui.margin(top = 12))
        manual.addView(
            ui.button("导入配对资料") {
                importPairing(pairingInput?.text?.toString().orEmpty())
            },
            ui.margin(top = 10),
        )
        pageContent.addView(manual, ui.margin(bottom = 14))
    }

    private fun scanPairingQr() {
        val options = GmsBarcodeScannerOptions.Builder()
            .setBarcodeFormats(Barcode.FORMAT_QR_CODE)
            .enableAutoZoom()
            .build()
        GmsBarcodeScanning.getClient(this, options)
            .startScan()
            .addOnSuccessListener { barcode ->
                val value = barcode.rawValue
                if (value.isNullOrBlank()) {
                    toast("二维码没有可读取内容。")
                } else {
                    importPairing(value)
                }
            }
            .addOnFailureListener { error ->
                toast("扫码失败：${error.message ?: "未知错误"}")
            }
    }

    private fun importPairing(text: String) {
        if (text.isBlank()) {
            toast("配对资料不能为空。")
            return
        }
        toast("正在安全配对…")
        Thread {
            try {
                val config = PairingManager().pair(text)
                store.savePairing(config)
                runOnUiThread {
                    pairingInput?.setText("")
                    latestStatusJson = null
                    endpointHealth = null
                    store.clearEvents()
                    currentPage = Page.OVERVIEW
                    toast("ChatX 已安全配对。")
                    renderApp()
                    refreshOnce()
                }
            } catch (error: Exception) {
                runOnUiThread {
                    toast(error.message ?: "配对失败。")
                }
            }
        }.start()
    }

    private fun confirmClearPairing() {
        val config = store.loadPairing() ?: return
        AlertDialog.Builder(this)
            .setTitle("删除此设备？")
            .setMessage(
                "会撤销此手机的 Direct / Relay 凭据，并从 Desktop 的已配对设备中删除。"
            )
            .setNegativeButton("取消", null)
            .setPositiveButton("删除") { _, _ ->
                MonitorService.stop(this)
                toast("正在撤销设备凭据…")
                Thread {
                    val failure = runCatching {
                        MonitorConnectionManager.revokePairing(this, config)
                    }.exceptionOrNull()
                    runOnUiThread {
                        if (failure == null) {
                            clearLocalPairing()
                            toast("此设备已删除。")
                        } else {
                            confirmLocalOnlyDelete(
                                failure.message ?: "无法连接 ChatX 撤销设备。"
                            )
                        }
                    }
                }.start()
            }
            .show()
    }

    private fun confirmLocalOnlyDelete(reason: String) {
        AlertDialog.Builder(this)
            .setTitle("远端撤销未完成")
            .setMessage(
                "$reason\n\n可以只清除此手机上的配对资料；" +
                    "Desktop 端的设备记录之后仍可在“手机连接”中手动撤销。"
            )
            .setNegativeButton("保留配对", null)
            .setPositiveButton("仅清除此手机") { _, _ ->
                clearLocalPairing()
            }
            .show()
    }

    private fun clearLocalPairing() {
        MonitorService.stop(this)
        store.clearPairing()
        store.setContinuousMode(false)
        latestStatusJson = null
        endpointHealth = null
        currentPage = Page.OVERVIEW
        renderApp()
    }

    private fun requestPermissionsAndStart() {
        if (store.loadPairing() == null) {
            toast("请先配对 ChatX。")
            return
        }
        pendingStart = true
        val permissions = mutableListOf<String>()
        if (
            Build.VERSION.SDK_INT >= 33 &&
            checkSelfPermission(
                Manifest.permission.POST_NOTIFICATIONS,
            ) != PackageManager.PERMISSION_GRANTED
        ) {
            permissions += Manifest.permission.POST_NOTIFICATIONS
        }
        if (
            Build.VERSION.SDK_INT >= 37 &&
            checkSelfPermission(
                ACCESS_LOCAL_NETWORK,
            ) != PackageManager.PERMISSION_GRANTED
        ) {
            permissions += ACCESS_LOCAL_NETWORK
        }
        if (permissions.isEmpty()) {
            startMonitoring()
        } else {
            requestPermissions(
                permissions.toTypedArray(),
                REQUEST_PERMISSIONS,
            )
        }
    }

    override fun onRequestPermissionsResult(
        requestCode: Int,
        permissions: Array<out String>,
        grantResults: IntArray,
    ) {
        super.onRequestPermissionsResult(
            requestCode,
            permissions,
            grantResults,
        )
        if (requestCode != REQUEST_PERMISSIONS || !pendingStart) return
        pendingStart = false
        val notificationGranted =
            Build.VERSION.SDK_INT < 33 ||
                checkSelfPermission(
                    Manifest.permission.POST_NOTIFICATIONS,
                ) == PackageManager.PERMISSION_GRANTED
        if (!notificationGranted) {
            toast("未授予通知权限，无法可靠发送中断提醒。")
            return
        }
        if (
            Build.VERSION.SDK_INT >= 37 &&
            checkSelfPermission(
                ACCESS_LOCAL_NETWORK,
            ) != PackageManager.PERMISSION_GRANTED
        ) {
            toast("LAN 权限未授予；仍会继续尝试 Tailscale / IPv6。")
        }
        startMonitoring()
    }

    private fun startMonitoring() {
        pendingStart = false
        if (store.isMonitorServiceRunning()) {
            MonitorService.stop(this)
        }
        MonitorService.start(this)
        toast("实时监控已启动。")
        renderCurrentPage()
    }

    private fun refreshOnce() {
        val config = store.loadPairing() ?: return
        actions.refresh(config)
    }

    private fun reconnectTunnel() {
        val config = store.loadPairing() ?: return
        actions.reconnect(config)
    }

    private fun testAllEndpoints(showToast: Boolean = true) {
        actions.testEndpoints(showToast)
    }

    @SuppressLint("UnspecifiedRegisterReceiverFlag")
    private fun registerStatusReceiver() {
        val filter = IntentFilter(MonitorService.ACTION_STATUS)
        if (Build.VERSION.SDK_INT >= 33) {
            registerReceiver(
                statusReceiver,
                filter,
                RECEIVER_NOT_EXPORTED,
            )
        } else {
            @Suppress("DEPRECATION")
            registerReceiver(statusReceiver, filter)
        }
    }

    private fun openBatterySettings() {
        startActivity(
            Intent(
                Settings.ACTION_APPLICATION_DETAILS_SETTINGS,
            ).apply {
                data = Uri.parse("package:$packageName")
            },
        )
    }

    private fun keyValue(
        label: String,
        value: String,
    ): View {
        val row = ui.row()
        row.addView(
            ui.muted(label, 12f),
            ui.margin(width = 0, weight = 0.42f, right = 8),
        )
        row.addView(
            ui.body(value, 13f).apply {
                gravity = Gravity.END
                maxLines = 2
            },
            ui.margin(width = 0, weight = 0.58f),
        )
        return row
    }

    private fun settingPickerRow(
        label: String,
        value: String,
        action: () -> Unit,
    ): View {
        val row = ui.row().apply {
            isClickable = true
            isFocusable = true
            setPadding(0, ui.dp(5), 0, ui.dp(5))
            setOnClickListener { action() }
        }
        row.addView(
            ui.muted(label, 12f),
            ui.margin(width = 0, weight = 0.42f, right = 8),
        )
        val right = ui.row()
        right.addView(ui.body(value, 13f))
        right.addView(
            ui.muted("›", 18f),
            ui.margin(left = 7),
        )
        row.addView(
            right,
            ui.margin(width = 0, weight = 0.58f),
        )
        return row
    }

    private fun showPollIntervalDialog() {
        val values = longArrayOf(10L, 15L, 30L, 60L)
        val labels = values.map { "$it 秒" }.toTypedArray()
        val selected = values.indexOf(store.getPollIntervalSeconds())
        AlertDialog.Builder(this)
            .setTitle("刷新间隔")
            .setSingleChoiceItems(labels, selected) { dialog, which ->
                val value = values[which]
                store.setPollIntervalSeconds(value)
                val minimumStale = minimumStaleForPoll(value)
                if (store.getSnapshotStaleSeconds() < minimumStale) {
                    store.setSnapshotStaleSeconds(minimumStale)
                }
                reloadMonitorSettings()
                dialog.dismiss()
                renderCurrentPage()
            }
            .setNegativeButton("取消", null)
            .show()
    }

    private fun showSnapshotStaleDialog() {
        val values = longArrayOf(30L, 60L, 90L, 180L)
        val labels = values.map { "$it 秒" }.toTypedArray()
        val selected = values.indexOf(store.getSnapshotStaleSeconds())
        AlertDialog.Builder(this)
            .setTitle("快照过期")
            .setSingleChoiceItems(labels, selected) { dialog, which ->
                val value = values[which]
                val minimum = store.getPollIntervalSeconds() * 3L
                if (value < minimum) {
                    toast("快照过期至少应为刷新间隔的 3 倍。")
                    return@setSingleChoiceItems
                }
                store.setSnapshotStaleSeconds(value)
                reloadMonitorSettings()
                dialog.dismiss()
                renderCurrentPage()
            }
            .setNegativeButton("取消", null)
            .show()
    }

    private fun showOfflineFailureDialog() {
        val values = intArrayOf(2, 3, 5)
        val labels = values.map { "连续 $it 次请求失败" }.toTypedArray()
        val selected = values.indexOf(store.getOfflineFailureThreshold())
        AlertDialog.Builder(this)
            .setTitle("离线确认")
            .setSingleChoiceItems(labels, selected) { dialog, which ->
                store.setOfflineFailureThreshold(values[which])
                reloadMonitorSettings()
                dialog.dismiss()
                renderCurrentPage()
            }
            .setNegativeButton("取消", null)
            .show()
    }

    private fun minimumStaleForPoll(pollSeconds: Long): Long =
        listOf(30L, 60L, 90L, 180L)
            .firstOrNull { it >= pollSeconds * 3L }
            ?: 180L

    private fun reloadMonitorSettings() {
        if (store.isMonitorServiceRunning()) {
            MonitorService.reload(this)
        }
    }

    private fun permissionRow(
        label: String,
        granted: Boolean,
    ): View {
        val row = ui.row()
        row.addView(
            ui.body(label, 14f),
            ui.margin(width = 0, weight = 1f),
        )
        row.addView(
            ui.pill(
                if (granted) "已允许" else "需处理",
                if (granted) UiKit.Tone.SUCCESS else UiKit.Tone.WARNING,
            ),
        )
        return row
    }

    private fun configuredRoutes(config: PairingConfig): List<MonitorEndpoint> =
        buildList {
            config.directEndpoints.forEach { endpoint ->
                add(endpoint.copy(url = directMonitorHttpsUrl(endpoint.url)))
            }
            config.relay?.let { relay ->
                add(
                    MonitorEndpoint(
                        kind = "relay",
                        family = "https",
                        interfaceName = "chatx-relay",
                        host = runCatching {
                            java.net.URI(relay.baseUrl).host
                        }.getOrNull().orEmpty(),
                        url = relay.baseUrl.trimEnd('/') +
                            "/v1/desktops/${config.desktopId}/devices/${config.deviceId}/snapshot",
                    ),
                )
            }
        }

    private fun showRoutePolicyDialog() {
        val policies = arrayOf(
            RoutePolicy.LAN_FIRST,
            RoutePolicy.RELAY_FIRST,
            RoutePolicy.AUTO,
            RoutePolicy.MANUAL,
        )
        val labels = policies.map(::routePolicyLabel).toTypedArray()
        val selected = policies.indexOf(store.getRoutePolicy()).coerceAtLeast(0)
        AlertDialog.Builder(this)
            .setTitle("选择路径策略")
            .setSingleChoiceItems(labels, selected) { dialog, which ->
                val policy = policies[which]
                store.setRoutePolicy(policy)
                if (policy == RoutePolicy.MANUAL &&
                    store.getManualRouteSelector() == null
                ) {
                    val routes = store.loadPairing()?.let(::configuredRoutes).orEmpty()
                    val currentBase = currentEndpointUrl()?.substringBefore('?')
                    val preferred = routes.firstOrNull { it.url == currentBase }
                        ?: routes.firstOrNull()
                    store.setManualRouteSelector(
                        preferred?.let(ManualRouteSelector::from),
                    )
                }
                if (store.isMonitorServiceRunning()) {
                    MonitorService.reevaluate(this)
                }
                dialog.dismiss()
                renderCurrentPage()
            }
            .setNegativeButton("取消", null)
            .show()
    }

    private fun showManualRouteDialog(routes: List<MonitorEndpoint>) {
        if (routes.isEmpty()) {
            toast("当前没有可选择的路径。")
            return
        }
        val current = store.getManualRouteSelector()
        val labels = routes.map { endpoint ->
            "${endpointKindLabel(endpoint.kind)} · ${endpoint.family.uppercase()} · " +
                (endpoint.host.ifBlank { endpoint.interfaceName })
        }.toTypedArray()
        val selected = routes.indexOfFirst { current?.matches(it) == true }
        AlertDialog.Builder(this)
            .setTitle("选择手动首选路径")
            .setSingleChoiceItems(labels, selected) { dialog, which ->
                store.setManualRouteSelector(
                    ManualRouteSelector.from(routes[which]),
                )
                store.setRoutePolicy(RoutePolicy.MANUAL)
                if (store.isMonitorServiceRunning()) {
                    MonitorService.reevaluate(this)
                }
                dialog.dismiss()
                renderCurrentPage()
            }
            .setNegativeButton("取消", null)
            .show()
    }

    private fun routePolicyLabel(policy: RoutePolicy): String = when (policy) {
        RoutePolicy.LAN_FIRST -> "LAN 优先"
        RoutePolicy.RELAY_FIRST -> "公网 Relay 优先"
        RoutePolicy.AUTO -> "自动 · 保持稳定路径"
        RoutePolicy.MANUAL -> "手动首选"
    }

    private fun manualRouteLabel(selector: ManualRouteSelector): String =
        buildString {
            append(endpointKindLabel(selector.kind))
            if (selector.family.isNotBlank()) {
                append(" · ")
                append(selector.family.uppercase())
            }
            if (selector.interfaceName.isNotBlank() &&
                selector.interfaceName != "chatx-relay"
            ) {
                append(" · ")
                append(selector.interfaceName)
            }
        }

    private fun currentEndpointUrl(): String? {
        val root = latestStatusJson
            ?.let { runCatching { JSONObject(it) }.getOrNull() }
        return root
            ?.takeIf { it.optBoolean("reachable", false) }
            ?.optString("endpointUrl")
            ?.takeIf { it.isNotBlank() }
            ?: store.getLastEndpoint()
    }

    private fun endpointLabel(url: String): String {
        val host = runCatching {
            java.net.URI(url).host
        }.getOrNull() ?: url
        val config = store.loadPairing()
        val baseUrl = url.substringBefore('?')
        val kind = config
            ?.directEndpoints
            ?.firstOrNull { directMonitorHttpsUrl(it.url) == baseUrl }
            ?.kind
            ?: if (
                config?.relay != null &&
                baseUrl.startsWith(config.relay.baseUrl.trimEnd('/') + "/v1/desktops/") &&
                baseUrl.endsWith("/snapshot")
            ) {
                "relay"
            } else {
                null
            }
        return if (kind != null) {
            "${endpointKindLabel(kind)} · $host"
        } else {
            host
        }
    }

    private fun directMonitorHttpsUrl(source: String): String {
        val base = when {
            source.startsWith("wss://") -> "https://" + source.removePrefix("wss://")
            source.startsWith("ws://") -> "http://" + source.removePrefix("ws://")
            else -> source
        }
        return base.substringBefore('?')
            .replace("/v1/ws/monitor", "/v1/monitor/snapshot")
            .replace("/v1/ws/pair", "/v1/monitor/snapshot")
    }

    private fun endpointKindLabel(kind: String): String = when (kind) {
        "lan" -> "LAN"
        "ipv6" -> "IPv6 Direct"
        "relay" -> "ChatX Relay"
        "tailscale" -> "Tailscale"
        else -> kind
    }
    private fun eventTypeLabel(type: String): String = when (type) {
        "RECOVERED" -> "恢复"
        "MCP_GAP" -> "GAP"
        "MCP_STALLED" -> "STALLED"
        "TUNNEL_DOWN" -> "TUNNEL"
        "HOST_OFFLINE" -> "OFFLINE"
        else -> type
    }

    private fun eventTone(type: String): UiKit.Tone = when (type) {
        "RECOVERED" -> UiKit.Tone.SUCCESS
        "MCP_GAP",
        "MCP_STALLED",
        "TUNNEL_DOWN",
        "HOST_OFFLINE",
        -> UiKit.Tone.DANGER
        else -> UiKit.Tone.NEUTRAL
    }

    private fun formatTime(value: Long): String {
        if (value <= 0L) return "—"
        return SimpleDateFormat(
            "MM-dd HH:mm:ss",
            Locale.getDefault(),
        ).format(Date(value))
    }

    private fun formatOptionalTime(value: Long?): String =
        value?.takeIf { it > 0L }?.let(::formatTime) ?: "—"
    private fun JSONObject.optLongOrNull(name: String): Long? {
        if (!has(name) || isNull(name)) return null
        return optLong(name)
    }

    private fun toast(message: String) {
        Toast.makeText(
            this,
            message,
            Toast.LENGTH_LONG,
        ).show()
    }

    private enum class Page(val label: String) {
        OVERVIEW("概览"),
        ALERTS("告警"),
        CONNECTION("连接"),
        SETTINGS("设置"),
    }

    companion object {
        private const val REQUEST_PERMISSIONS = 2001
        private const val ACCESS_LOCAL_NETWORK =
            "android.permission.ACCESS_LOCAL_NETWORK"
    }
}
