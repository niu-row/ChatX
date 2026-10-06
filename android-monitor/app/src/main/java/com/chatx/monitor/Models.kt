package com.chatx.monitor

import java.net.Inet4Address
import java.net.Inet6Address
import java.net.InetAddress
import java.net.URI
import org.json.JSONArray
import org.json.JSONObject

data class MonitorEndpoint(
    val kind: String,
    val family: String,
    val interfaceName: String,
    val host: String,
    val url: String,
)

data class RelayEnrollment(
    val baseUrl: String,
    val desktopId: String,
    val deviceToken: String,
) {
    init {
        require(baseUrl.startsWith("https://")) {
            "Relay URL 必须使用 HTTPS。"
        }
        require(deviceToken.matches(Regex("[0-9a-fA-F]{64}"))) {
            "Relay Device Token 无效。"
        }
    }
}

data class PairingConfig(
    val desktopId: String,
    val port: Int,
    val fingerprintSha256: String,
    val deviceId: String,
    val directToken: String,
    val deviceKey: String,
    val directEndpoints: List<MonitorEndpoint>,
    val relay: RelayEnrollment?,
) {
    fun toJson(): String = JSONObject().apply {
        put("schemaVersion", 3)
        put("protocol", "chatx-monitor-wss-v1")
        put("desktopId", desktopId)
        put("port", port)
        put("fingerprintSha256", fingerprintSha256)
        put("deviceId", deviceId)
        put("directToken", directToken)
        put("deviceKey", deviceKey)
        put("directEndpoints", JSONArray().apply {
            directEndpoints
                .filter(MonitorEndpoint::isSupportedDirectRoute)
                .forEach { endpoint ->
                    put(endpoint.toJson())
                }
        })
        put("relay", relay?.let { item ->
            JSONObject().apply {
                put("baseUrl", item.baseUrl)
                put("desktopId", item.desktopId)
                put("deviceToken", item.deviceToken)
            }
        })
    }.toString()

    companion object {
        fun parse(text: String): PairingConfig {
            val root = JSONObject(text.trim())
            require(root.optInt("schemaVersion", -1) == 3) {
                "旧版 Monitor 配对资料已失效，请重新扫描 ChatX WSS 配对二维码。"
            }
            require(root.optString("protocol") == "chatx-monitor-wss-v1") {
                "不支持的 Monitor 协议。"
            }
            val desktopId = root.getString("desktopId").trim()
            require(desktopId.matches(Regex("d_[0-9a-fA-F]{16,78}"))) {
                "Desktop ID 无效。"
            }
            val deviceId = root.getString("deviceId").trim()
            require(deviceId.matches(Regex("dev_[0-9a-fA-F]{16,78}"))) {
                "Device ID 无效。"
            }
            val fingerprint = normalizeFingerprint(root.getString("fingerprintSha256"))
            val directToken = root.getString("directToken").trim().lowercase()
            require(directToken.matches(Regex("[0-9a-f]{64}"))) {
                "Direct Token 无效。"
            }
            val deviceKey = root.getString("deviceKey").trim()
            require(deviceKey.isNotBlank()) { "Device E2EE Key 无效。" }
            val port = root.optInt("port", 18432)
            require(port in 1024..65535) { "Monitor 端口无效。" }

            val endpoints = root.optJSONArray("directEndpoints")
                ?.let(::parseEndpoints)
                .orEmpty()

            val relay = root.optJSONObject("relay")?.let {
                RelayEnrollment(
                    baseUrl = it.getString("baseUrl").trimEnd('/'),
                    desktopId = it.getString("desktopId").trim(),
                    deviceToken = it.getString("deviceToken").trim().lowercase(),
                )
            }
            if (relay != null) {
                require(relay.desktopId == desktopId) {
                    "Relay Desktop ID 与 Direct 身份不一致。"
                }
            }
            require(endpoints.isNotEmpty() || relay != null) {
                "配对资料中没有可用 WSS 路径。"
            }
            return PairingConfig(
                desktopId = desktopId,
                port = port,
                fingerprintSha256 = fingerprint,
                deviceId = deviceId,
                directToken = directToken,
                deviceKey = deviceKey,
                directEndpoints = endpoints,
                relay = relay,
            )
        }
    }
}

data class RelayPairingRoute(
    val baseUrl: String,
    val url: String,
    val desktopId: String,
    val pairingId: String,
    val routeToken: String,
)

data class PairingInvite(
    val desktopId: String,
    val port: Int,
    val fingerprintSha256: String,
    val directCandidates: List<MonitorEndpoint>,
    val relayPairing: RelayPairingRoute?,
    val pairingId: String,
    val pairingCode: String,
    val expiresAt: Long,
) {
    companion object {
        fun parse(text: String): PairingInvite {
            val root = JSONObject(text.trim())
            require(root.optInt("schemaVersion", -1) == 3) {
                "请在新版 ChatX 中重新生成 WSS 配对二维码。"
            }
            require(root.optString("protocol") == "chatx-monitor-wss-v1") {
                "不支持的 Monitor 协议。"
            }
            require(root.optString("scheme") == "wss") {
                "配对资料必须使用 WSS。"
            }
            val desktopId = root.getString("desktopId").trim()
            require(desktopId.matches(Regex("d_[0-9a-fA-F]{16,78}"))) {
                "Desktop ID 无效。"
            }
            val port = root.getInt("port")
            require(port in 1024..65535) { "Monitor 端口无效。" }
            val fingerprint = normalizeFingerprint(root.getString("fingerprintSha256"))
            val pairingId = root.getString("pairingId").trim()
            require(pairingId.matches(Regex("p_[0-9a-fA-F]{16,78}"))) {
                "Pairing ID 无效。"
            }
            val code = root.getString("pairingCode").trim().lowercase()
            require(code.matches(Regex("[0-9a-f]{64}"))) {
                "Pairing Code 无效。"
            }
            val expiresAt = root.getLong("expiresAt")
            val candidates = root.optJSONArray("directCandidates")
                ?.let(::parseEndpoints)
                .orEmpty()
            candidates.forEach {
                require(it.url.startsWith("wss://")) {
                    "Direct Pairing Endpoint 必须使用 WSS。"
                }
                require(it.url.endsWith("/v1/ws/pair")) {
                    "Direct Pairing Endpoint 路径无效。"
                }
            }
            val relayPairing = root.optJSONObject("relayPairing")?.let { relay ->
                val route = RelayPairingRoute(
                    baseUrl = relay.getString("baseUrl").trimEnd('/'),
                    url = relay.getString("url").trim(),
                    desktopId = relay.getString("desktopId").trim(),
                    pairingId = relay.getString("pairingId").trim(),
                    routeToken = relay.getString("routeToken").trim().lowercase(),
                )
                require(route.desktopId == desktopId) {
                    "Relay Pairing Desktop ID 不一致。"
                }
                require(route.pairingId == pairingId) {
                    "Relay Pairing ID 不一致。"
                }
                require(route.routeToken.matches(Regex("[0-9a-f]{64}"))) {
                    "Relay Pairing Token 无效。"
                }
                require(route.baseUrl.startsWith("https://")) {
                    "Relay Pairing URL 必须使用 HTTPS。"
                }
                require(route.url.startsWith("wss://")) {
                    "Relay Pairing Endpoint 必须使用 WSS。"
                }
                route
            }
            require(candidates.isNotEmpty() || relayPairing != null) {
                "配对资料中没有可用 WSS 路径。"
            }
            return PairingInvite(
                desktopId = desktopId,
                port = port,
                fingerprintSha256 = fingerprint,
                directCandidates = candidates,
                relayPairing = relayPairing,
                pairingId = pairingId,
                pairingCode = code,
                expiresAt = expiresAt,
            )
        }
    }
}

private fun normalizeFingerprint(value: String): String {
    val normalized = value.replace(":", "").lowercase()
    require(normalized.matches(Regex("[0-9a-f]{64}"))) { "TLS 指纹无效。" }
    return normalized
}

private fun parseEndpoints(array: JSONArray): List<MonitorEndpoint> = buildList {
    for (index in 0 until array.length()) {
        val item = array.getJSONObject(index)
        val endpoint = MonitorEndpoint(
            kind = item.optString("kind", "unknown"),
            family = item.optString("family", "unknown"),
            interfaceName = item.optString("interface", ""),
            host = item.optString("host", ""),
            url = item.getString("url").trim(),
        )
        if (endpoint.isSupportedDirectRoute()) {
            add(endpoint)
        }
    }
}

fun MonitorEndpoint.isSupportedDirectRoute(): Boolean {
    if (!url.startsWith("wss://")) return false
    val address = runCatching {
        val host = URI(url).host ?: return@runCatching null
        InetAddress.getByName(host)
    }.getOrNull() ?: return false

    return when {
        kind == "lan" && family == "ipv4" ->
            address is Inet4Address && address.isSiteLocalAddress
        kind == "ipv6" && family == "ipv6" &&
            address is Inet6Address -> {
            val first = address.address[0].toInt() and 0xff
            !address.isAnyLocalAddress &&
                !address.isLoopbackAddress &&
                !address.isMulticastAddress &&
                !address.isLinkLocalAddress &&
                first and 0xfe != 0xfc
        }
        else -> false
    }
}

private fun MonitorEndpoint.toJson(): JSONObject = JSONObject().apply {
    put("kind", kind)
    put("family", family)
    put("interface", interfaceName)
    put("host", host)
    put("url", url)
}

data class BatteryStatus(
    val present: Boolean,
    val percent: Int?,
    val charging: Boolean,
    val powerSource: String?,
)

data class HostStatus(
    val deviceName: String,
    val platform: String,
    val arch: String,
    val battery: BatteryStatus?,
)

data class TunnelStatus(
    val state: String,
    val health: String,
    val active: Boolean,
    val updatedAt: Long,
    val lastProbeAt: Long,
    val lastSuccessfulProbeAt: Long,
    val consecutiveFailures: Int,
    val controlPlaneState: String,
    val controlPlaneReason: String?,
    val controlPlaneFailures: Int,
    val proxyMode: String,
    val proxySource: String?,
    val desiredConnected: Boolean,
    val reconnecting: Boolean,
    val reconnectAttempt: Int,
)

data class RecentMcpCall(
    val toolName: String,
    val timestamp: String?,
    val startedAt: Long?,
    val durationMs: Long?,
    val success: Boolean?,
    val running: Boolean,
)

data class McpStatus(
    val state: String,
    val callsLastMinute: Int,
    val gapDurationMs: Long?,
    val stallDurationMs: Long?,
    val inFlight: Int,
    val lastToolName: String?,
    val lastCallStartedAt: Long?,
    val lastCallFinishedAt: Long?,
    val recentCalls: List<RecentMcpCall> = emptyList(),
)

data class MonitorSnapshot(
    val serverTime: Long,
    val host: HostStatus?,
    val tunnel: TunnelStatus,
    val mcp: McpStatus,
    val endpointUrl: String,
    val transportKind: String = "unknown",
    val endpoints: List<MonitorEndpoint> = emptyList(),
    val receivedAt: Long = System.currentTimeMillis(),
)
