package com.chatx.monitor

enum class AlertType {
    MCP_GAP,
    MCP_STALLED,
    TUNNEL_DOWN,
    HOST_OFFLINE,
    RECOVERED,
}

data class AlertEvent(
    val id: Int,
    val type: AlertType,
    val title: String,
    val message: String,
)

class AlertEngine {
    private var offlineSince: Long? = null
    private val offlineThresholdsSent = mutableSetOf<Long>()
    private val gapThresholdsSent = mutableSetOf<Long>()
    private var tunnelDownNotified = false
    private var stalledNotified = false

    fun onUnreachable(now: Long, thresholds: Set<Long>): List<AlertEvent> {
        val startedAt = offlineSince ?: now.also { offlineSince = it }
        val duration = (now - startedAt).coerceAtLeast(0)
        return thresholdEvents(
            duration = duration,
            thresholds = thresholds,
            sent = offlineThresholdsSent,
            type = AlertType.HOST_OFFLINE,
            idBase = 4000,
            title = "ChatX 主机不可达",
            messagePrefix = "已连续无法连接 ChatX",
        )
    }

    fun onSnapshot(
        snapshot: MonitorSnapshot,
        thresholds: Set<Long>,
        continuousStartedAt: Long? = null,
    ): List<AlertEvent> {
        val events = mutableListOf<AlertEvent>()

        val recoveredOffline = offlineSince != null && offlineThresholdsSent.isNotEmpty()
        if (recoveredOffline) {
            val duration = snapshot.receivedAt - (offlineSince ?: snapshot.receivedAt)
            events += AlertEvent(
                id = 4900,
                type = AlertType.RECOVERED,
                title = "ChatX 已恢复",
                message = "主机连接已恢复，中断约 ${formatDuration(duration)}。",
            )
        }
        offlineSince = null
        offlineThresholdsSent.clear()
        val probeAge = if (snapshot.tunnel.lastProbeAt > 0L) {
            (snapshot.serverTime - snapshot.tunnel.lastProbeAt).coerceAtLeast(0L)
        } else Long.MAX_VALUE
        val probeFresh = probeAge <= 12_000L
        val tunnelHealthy = snapshot.tunnel.active && snapshot.tunnel.health == "healthy" && probeFresh
        val tunnelDown = snapshot.tunnel.desiredConnected &&
            (snapshot.tunnel.health == "down" || !probeFresh)
        if (tunnelDown) {
            if (!tunnelDownNotified) {
                events += AlertEvent(
                    id = 3000,
                    type = AlertType.TUNNEL_DOWN,
                    title = "Secure MCP Tunnel 已断开",
                    message = if (snapshot.tunnel.reconnecting) {
                        "ChatX 正在进行第 ${snapshot.tunnel.reconnectAttempt} 次自动重连。"
                    } else {
                        "ChatX 在线，但 Tunnel 当前不可用。"
                    },
                )
                tunnelDownNotified = true
            }
        } else if (tunnelHealthy && tunnelDownNotified) {
            events += AlertEvent(
                id = 3900,
                type = AlertType.RECOVERED,
                title = "Secure MCP Tunnel 已恢复",
                message = "Tunnel 已重新进入 ${snapshot.tunnel.state} 状态。",
            )
            tunnelDownNotified = false
        }
        val gapDuration = when {
            snapshot.mcp.state == "gap" -> snapshot.mcp.gapDurationMs ?: 0L
            continuousStartedAt != null &&
                snapshot.mcp.state != "stalled" &&
                snapshot.mcp.inFlight == 0 -> {
                val lastFinished = snapshot.mcp.lastCallFinishedAt ?: continuousStartedAt
                val anchor = maxOf(continuousStartedAt, lastFinished)
                (snapshot.serverTime - anchor).coerceAtLeast(0L)
            }
            else -> null
        }
        if (gapDuration != null) {
            events += thresholdEvents(
                duration = gapDuration,
                thresholds = thresholds,
                sent = gapThresholdsSent,
                type = AlertType.MCP_GAP,
                idBase = 1000,
                title = "MCP 调用中断",
                messagePrefix = "持续调用突然停止",
            )
        } else if (snapshot.mcp.state == "active" && gapThresholdsSent.isNotEmpty()) {
            events += AlertEvent(
                id = 1900,
                type = AlertType.RECOVERED,
                title = "MCP 调用已恢复",
                message = "已重新收到 MCP tool call。",
            )
            gapThresholdsSent.clear()
        } else if (snapshot.mcp.state == "idle" && continuousStartedAt == null) {
            gapThresholdsSent.clear()
        }

        if (snapshot.mcp.state == "stalled" && !stalledNotified) {
            val duration = snapshot.mcp.stallDurationMs ?: 0L
            events += AlertEvent(
                id = 2000,
                type = AlertType.MCP_STALLED,
                title = "MCP 工具可能卡住",
                message = "${snapshot.mcp.lastToolName ?: "MCP tool"} 已执行 ${formatDuration(duration)} 仍未结束。",
            )
            stalledNotified = true
        } else if (snapshot.mcp.state != "stalled" && stalledNotified) {
            if (snapshot.mcp.state == "active") {
                events += AlertEvent(
                    id = 2900,
                    type = AlertType.RECOVERED,
                    title = "MCP 工具已恢复",
                    message = "长时间执行的 MCP 调用已经结束或恢复。",
                )
            }
            stalledNotified = false
        }

        return events
    }

    private fun thresholdEvents(
        duration: Long,
        thresholds: Set<Long>,
        sent: MutableSet<Long>,
        type: AlertType,
        idBase: Int,
        title: String,
        messagePrefix: String,
    ): List<AlertEvent> {
        val events = mutableListOf<AlertEvent>()
        for (threshold in thresholds.sorted()) {
            if (duration >= threshold && sent.add(threshold)) {
                val minutes = threshold / 60_000L
                events += AlertEvent(
                    id = idBase + minutes.toInt(),
                    type = type,
                    title = title,
                    message = "${messagePrefix}已超过 ${minutes} 分钟。",
                )
            }
        }
        return events
    }

    private fun formatDuration(duration: Long): String {
        val totalSeconds = (duration.coerceAtLeast(0) / 1000L)
        val minutes = totalSeconds / 60L
        val seconds = totalSeconds % 60L
        return if (minutes > 0) "${minutes}分${seconds}秒" else "${seconds}秒"
    }
}
