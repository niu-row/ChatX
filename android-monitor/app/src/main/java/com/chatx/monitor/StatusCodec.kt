package com.chatx.monitor

import org.json.JSONObject

object StatusCodec {
    fun snapshot(snapshot: MonitorSnapshot): String {
        return JSONObject().apply {
            put("reachable", true)
            put("receivedAt", snapshot.receivedAt)
            put("endpointUrl", snapshot.endpointUrl)
            put("transportKind", snapshot.transportKind)
            put("serverTime", snapshot.serverTime)
            put("endpoints", org.json.JSONArray().apply {
                snapshot.endpoints.forEach { endpoint ->
                    put(JSONObject().apply {
                        put("kind", endpoint.kind)
                        put("family", endpoint.family)
                        put("interface", endpoint.interfaceName)
                        put("host", endpoint.host)
                        put("url", endpoint.url)
                    })
                }
            })
            put("host", snapshot.host?.let { host ->
                JSONObject().apply {
                    put("deviceName", host.deviceName)
                    put("platform", host.platform)
                    put("arch", host.arch)
                    put("battery", host.battery?.let { battery ->
                        JSONObject().apply {
                            put("present", battery.present)
                            put("percent", battery.percent)
                            put("charging", battery.charging)
                            put("powerSource", battery.powerSource)
                        }
                    })
                }
            })
            put("tunnel", JSONObject().apply {
                put("state", snapshot.tunnel.state)
                put("health", snapshot.tunnel.health)
                put("active", snapshot.tunnel.active)
                put("desiredConnected", snapshot.tunnel.desiredConnected)
                put("reconnecting", snapshot.tunnel.reconnecting)
                put("reconnectAttempt", snapshot.tunnel.reconnectAttempt)
                put("updatedAt", snapshot.tunnel.updatedAt)
                put("lastProbeAt", snapshot.tunnel.lastProbeAt)
                put("lastSuccessfulProbeAt", snapshot.tunnel.lastSuccessfulProbeAt)
                put("consecutiveFailures", snapshot.tunnel.consecutiveFailures)
                put("controlPlaneState", snapshot.tunnel.controlPlaneState)
                put("controlPlaneReason", snapshot.tunnel.controlPlaneReason)
                put("controlPlaneFailures", snapshot.tunnel.controlPlaneFailures)
                put("proxyMode", snapshot.tunnel.proxyMode)
                put("proxySource", snapshot.tunnel.proxySource)
            })
            put("mcp", JSONObject().apply {
                put("state", snapshot.mcp.state)
                put("callsLastMinute", snapshot.mcp.callsLastMinute)
                put("inFlight", snapshot.mcp.inFlight)
                put("lastToolName", snapshot.mcp.lastToolName)
                put("lastCallStartedAt", snapshot.mcp.lastCallStartedAt)
                put("lastCallFinishedAt", snapshot.mcp.lastCallFinishedAt)
                put("gapDurationMs", snapshot.mcp.gapDurationMs)
                put("stallDurationMs", snapshot.mcp.stallDurationMs)
                put("recentCalls", org.json.JSONArray().apply {
                    snapshot.mcp.recentCalls.forEach { call ->
                        put(JSONObject().apply {
                            put("toolName", call.toolName)
                            put("timestamp", call.timestamp)
                            put("startedAt", call.startedAt)
                            put("durationMs", call.durationMs)
                            put("success", call.success)
                            put("running", call.running)
                        })
                    }
                })
            })
        }.toString()
    }
    fun error(message: String, at: Long = System.currentTimeMillis()): String {
        return JSONObject().apply {
            put("reachable", false)
            put("receivedAt", at)
            put("transportPhase", "offline")
            put("error", message)
        }.toString()
    }

    fun transportError(
        state: MonitorTransportState,
        at: Long = System.currentTimeMillis(),
    ): String {
        return JSONObject().apply {
            put("reachable", false)
            put("receivedAt", at)
            put("transportPhase", state.phase)
            put("transportKind", state.transportKind)
            put("endpointUrl", state.url)
            put("desktopOnline", state.desktopOnline)
            put("reconnectAttempt", state.reconnectAttempt)
            put("error", state.error)
        }.toString()
    }

    fun foregroundText(snapshot: MonitorSnapshot): String {
        val probeAge = if (snapshot.tunnel.lastProbeAt > 0L) {
            (snapshot.serverTime - snapshot.tunnel.lastProbeAt).coerceAtLeast(0L)
        } else Long.MAX_VALUE
        val probeFresh = probeAge <= 12_000L
        val tunnel = when {
            !probeFresh -> "Tunnel Stale"
            snapshot.tunnel.health == "healthy" && snapshot.tunnel.active -> "Tunnel Ready"
            snapshot.tunnel.health == "suspect" -> "Tunnel Check"
            snapshot.tunnel.health == "down" -> "Tunnel Down"
            else -> "Tunnel Stopped"
        }
        val mcp = when (snapshot.mcp.state) {
            "active" -> "MCP 活跃"
            "gap" -> "MCP 中断"
            "stalled" -> "MCP 卡住"
            else -> "MCP 空闲"
        }
        return "$tunnel · $mcp · ${transportLabel(snapshot.transportKind)}"
    }

    private fun transportLabel(kind: String): String = when (kind) {
        "lan" -> "LAN"
        "ipv6" -> "IPv6"
        "relay" -> "服务器 Relay"
        else -> "HTTPS"
    }
}
