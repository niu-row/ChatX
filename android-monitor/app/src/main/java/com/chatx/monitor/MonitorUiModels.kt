package com.chatx.monitor

data class EndpointHealth(
    val endpoint: MonitorEndpoint,
    val reachable: Boolean,
    val latencyMs: Long?,
    val error: String?,
)

data class MonitorEvent(
    val at: Long,
    val type: String,
    val title: String,
    val message: String,
)
