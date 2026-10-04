package com.chatx.monitor

import org.json.JSONObject

object MonitorSnapshotParser {
    fun parse(
        root: JSONObject,
        endpointUrl: String,
        transportKind: String,
    ): MonitorSnapshot {
        require(root.optInt("schemaVersion", -1) == 1) {
            "不支持的 Monitor Snapshot 版本。"
        }
        val tunnel = root.getJSONObject("tunnel")
        val mcp = root.getJSONObject("mcp")
        val host = root.optJSONObject("host")
        val battery = host?.optJSONObject("battery")
        return MonitorSnapshot(
            serverTime = root.optLong("serverTime", 0L),
            host = host?.let {
                HostStatus(
                    deviceName = it.optString("deviceName", "Computer"),
                    platform = it.optString("platform", "unknown"),
                    arch = it.optString("arch", "unknown"),
                    battery = battery?.let { item ->
                        BatteryStatus(
                            present = item.optBoolean("present", false),
                            percent = item.optIntOrNull("percent"),
                            charging = item.optBoolean("charging", false),
                            powerSource = item.optString("powerSource").ifBlank { null },
                        )
                    },
                )
            },
            tunnel = TunnelStatus(
                state = tunnel.optString("state", "unknown"),
                health = tunnel.optString(
                    "health",
                    if (tunnel.optBoolean("active", false)) "healthy" else "unknown",
                ),
                active = tunnel.optBoolean("active", false),
                updatedAt = tunnel.optLong("updatedAt", 0L),
                lastProbeAt = tunnel.optLong(
                    "lastProbeAt",
                    tunnel.optLong("updatedAt", 0L),
                ),
                lastSuccessfulProbeAt = tunnel.optLong("lastSuccessfulProbeAt", 0L),
                consecutiveFailures = tunnel.optInt("consecutiveFailures", 0),
                controlPlaneState = tunnel.optString("controlPlaneState", "unknown"),
                controlPlaneReason = tunnel.optString("controlPlaneReason").ifBlank { null },
                controlPlaneFailures = tunnel.optInt("controlPlaneFailures", 0),
                proxyMode = tunnel.optString("proxyMode", "direct"),
                proxySource = tunnel.optString("proxySource").ifBlank { null },
                desiredConnected = tunnel.optBoolean("desiredConnected", false),
                reconnecting = tunnel.optBoolean("reconnecting", false),
                reconnectAttempt = tunnel.optInt("reconnectAttempt", 0),
            ),
            mcp = McpStatus(
                state = mcp.optString("state", "idle"),
                callsLastMinute = mcp.optInt("callsLastMinute", 0),
                gapDurationMs = mcp.optLongOrNull("gapDurationMs"),
                stallDurationMs = mcp.optLongOrNull("stallDurationMs"),
                inFlight = mcp.optInt("inFlight", 0),
                lastToolName = mcp.optString("lastToolName").ifBlank { null },
                lastCallStartedAt = mcp.optLongOrNull("lastCallStartedAt"),
                lastCallFinishedAt = mcp.optLongOrNull("lastCallFinishedAt"),
                recentCalls = parseRecentCalls(mcp),
            ),
            endpointUrl = endpointUrl,
            transportKind = transportKind,
            endpoints = parseEndpoints(root),
        )
    }

    private fun parseEndpoints(root: JSONObject): List<MonitorEndpoint> {
        val array = root.optJSONArray("endpoints") ?: return emptyList()
        return buildList {
            for (index in 0 until array.length()) {
                val item = array.optJSONObject(index) ?: continue
                val url = item.optString("url").trim()
                if (!url.startsWith("wss://")) continue
                add(
                    MonitorEndpoint(
                        kind = item.optString("kind", "unknown"),
                        family = item.optString("family", "unknown"),
                        interfaceName = item.optString("interface", ""),
                        host = item.optString("host", ""),
                        url = url,
                    ),
                )
            }
        }
    }

    private fun parseRecentCalls(mcp: JSONObject): List<RecentMcpCall> {
        val array = mcp.optJSONArray("recentCalls") ?: return emptyList()
        return buildList {
            for (index in 0 until array.length()) {
                val item = array.optJSONObject(index) ?: continue
                val toolName = item.optString("toolName").ifBlank { continue }
                add(
                    RecentMcpCall(
                        toolName = toolName,
                        timestamp = item.optString("timestamp").ifBlank { null },
                        startedAt = item.optLongOrNull("startedAt"),
                        durationMs = item.optLongOrNull("durationMs"),
                        success = item.optBooleanOrNull("success"),
                        running = item.optBoolean("running", false),
                    ),
                )
            }
        }
    }
}

private fun JSONObject.optLongOrNull(name: String): Long? {
    if (!has(name) || isNull(name)) return null
    return optLong(name)
}

private fun JSONObject.optIntOrNull(name: String): Int? {
    if (!has(name) || isNull(name)) return null
    return optInt(name)
}

private fun JSONObject.optBooleanOrNull(name: String): Boolean? {
    if (!has(name) || isNull(name)) return null
    return optBoolean(name)
}
