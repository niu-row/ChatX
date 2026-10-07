package com.chatx.monitor

import org.json.JSONObject
import java.text.SimpleDateFormat
import java.time.Instant
import java.util.Date
import java.util.Locale

internal fun mcpTone(state: String): UiKit.Tone = when (state) {
    "active" -> UiKit.Tone.SUCCESS
    "gap" -> UiKit.Tone.DANGER
    "stalled" -> UiKit.Tone.WARNING
    else -> UiKit.Tone.NEUTRAL
}

internal fun recentCallTime(item: JSONObject): String {
    val startedAt = item.optLongOrNull("startedAt")
    if (startedAt != null) return formatClockTime(startedAt)
    val timestamp = item.optString("timestamp")
        .takeIf { it.isNotBlank() }
        ?: return "—"
    return runCatching {
        formatClockTime(Instant.parse(timestamp).toEpochMilli())
    }.getOrDefault(timestamp.take(19))
}

internal fun formatClockTime(value: Long): String =
    SimpleDateFormat("HH:mm:ss", Locale.getDefault())
        .format(Date(value))
internal fun formatCallDuration(value: Long?): String {
    val ms = value ?: return "—"
    return when {
        ms < 1_000 -> "${ms} ms"
        ms < 60_000 ->
            String.format(
                Locale.getDefault(),
                "%.1f s",
                ms / 1000.0,
            )
        else ->
            "${ms / 60_000}m ${(ms % 60_000) / 1000}s"
    }
}

internal fun formatDuration(value: Long): String {
    val totalSeconds = value.coerceAtLeast(0L) / 1000L
    val minutes = totalSeconds / 60L
    val seconds = totalSeconds % 60L
    return if (minutes > 0) {
        "${minutes}分${seconds}秒"
    } else {
        "${seconds}秒"
    }
}

internal fun minimumStaleForPoll(pollSeconds: Long): Long =
    listOf(30L, 60L, 90L, 180L)
        .firstOrNull { it >= pollSeconds * 3L }
        ?: 180L
internal fun configuredRoutes(
    config: PairingConfig,
): List<MonitorEndpoint> =
    buildList {
        config.directEndpoints
            .filter(MonitorEndpoint::isSupportedDirectRoute)
            .forEach { endpoint ->
                add(
                    endpoint.copy(
                        url = directMonitorHttpsUrl(endpoint.url),
                    ),
                )
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
                        "/v1/desktops/${config.desktopId}" +
                        "/devices/${config.deviceId}/snapshot",
                ),
            )
        }
    }

internal fun routePolicyLabel(policy: RoutePolicy): String =
    when (policy) {
        RoutePolicy.LAN_FIRST -> "LAN 优先"
        RoutePolicy.RELAY_FIRST -> "服务器 Relay 优先"
        RoutePolicy.AUTO -> "自动 · 保持稳定路径"
        RoutePolicy.MANUAL -> "手动首选"
    }
internal fun manualRouteLabel(
    selector: ManualRouteSelector,
): String =
    buildString {
        append(endpointKindLabel(selector.kind))
        if (selector.family.isNotBlank()) {
            append(" · ")
            append(selector.family.uppercase())
        }
        if (
            selector.interfaceName.isNotBlank() &&
            selector.interfaceName != "chatx-relay"
        ) {
            append(" · ")
            append(selector.interfaceName)
        }
    }

internal fun directMonitorHttpsUrl(source: String): String {
    val base = when {
        source.startsWith("wss://") ->
            "https://" + source.removePrefix("wss://")
        source.startsWith("ws://") ->
            "http://" + source.removePrefix("ws://")
        else -> source
    }
    return base.substringBefore('?')
        .replace("/v1/ws/monitor", "/v1/monitor/snapshot")
        .replace("/v1/ws/pair", "/v1/monitor/snapshot")
}
internal fun endpointKindLabel(kind: String): String = when (kind) {
    "lan" -> "LAN"
    "ipv6" -> "IPv6 Direct"
    "relay" -> "服务器 Relay"
    else -> kind
}

internal fun eventTypeLabel(type: String): String = when (type) {
    "RECOVERED" -> "恢复"
    "MCP_GAP" -> "GAP"
    "MCP_STALLED" -> "STALLED"
    "TUNNEL_DOWN" -> "TUNNEL"
    "HOST_OFFLINE" -> "OFFLINE"
    else -> type
}

internal fun eventTone(type: String): UiKit.Tone = when (type) {
    "RECOVERED" -> UiKit.Tone.SUCCESS
    "MCP_GAP",
    "MCP_STALLED",
    "TUNNEL_DOWN",
    "HOST_OFFLINE",
    -> UiKit.Tone.DANGER
    else -> UiKit.Tone.NEUTRAL
}

internal fun tunnelRecoveryDetail(
    tunnel: JSONObject,
    serverTime: Long,
): String {
    val attempt = tunnel.optInt("reconnectAttempt", 0)
    return when (tunnel.optString("reconnectPhase", "idle")) {
        "detecting" -> "ChatX 已检测到 Tunnel 异常，正在确认状态。"
        "reconnecting" ->
            "ChatX 正在执行第 ${attempt.coerceAtLeast(1)} 次自动重连。"
        "verifying" -> "重连命令已完成，正在验证 Tunnel Ready。"
        "backoff" -> {
            val remaining = (
                tunnel.optLong("nextReconnectAt", 0L) - serverTime
            ).coerceAtLeast(0L)
            val suffix = if (remaining > 0L) {
                "约 ${((remaining + 999L) / 1000L)} 秒后重试。"
            } else {
                "正在等待下一次重试。"
            }
            val error = tunnel.optString("lastReconnectError")
                .ifBlank { null }
            listOfNotNull(error, suffix).joinToString(" ")
        }
        "blocked", "failed" ->
            tunnel.optString("lastReconnectError").ifBlank {
                "ChatX 无法自动恢复 Tunnel。"
            }
        "disabled" -> "Tunnel 自动重连已关闭。"
        "stopped" -> "Tunnel 已被 Desktop 手动停止。"
        else -> when (
            tunnel.optString("reconnectUnavailableReason")
        ) {
            "runtime_key_unavailable" ->
                "Desktop 缺少 Runtime Key，当前无法重连。"
            "tunnel_not_configured" ->
                "Desktop 尚未配置 Tunnel。"
            "manually_stopped" ->
                "Tunnel 已被 Desktop 手动停止。"
            else ->
                "ChatX 在线，但 Secure MCP Tunnel 当前不可用。"
        }
    }
}

internal fun formatTime(value: Long): String {
    if (value <= 0L) return "—"
    return SimpleDateFormat(
        "MM-dd HH:mm:ss",
        Locale.getDefault(),
    ).format(Date(value))
}

internal fun formatOptionalTime(value: Long?): String =
    value?.takeIf { it > 0L }?.let(::formatTime) ?: "—"

internal fun JSONObject.optLongOrNull(name: String): Long? {
    if (!has(name) || isNull(name)) return null
    return optLong(name)
}
