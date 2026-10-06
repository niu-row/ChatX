package com.chatx.monitor

import android.content.Context
import android.os.SystemClock
import java.net.URI
import java.util.concurrent.Executors

class MonitorRepository(context: Context) {
    private val appContext = context.applicationContext
    private val store = SecureStore(appContext)

    fun fetchSnapshot(): MonitorSnapshot {
        val config = store.loadPairing()
            ?: throw IllegalStateException("尚未配对 ChatX。")
        val snapshot = MonitorConnectionManager.fetchOnce(
            context = appContext,
            config = config,
        )
        store.updateDirectEndpoints(
            config.desktopId,
            config.deviceId,
            snapshot.endpoints,
        )
        return snapshot
    }

    fun testEndpoints(): List<EndpointHealth> {
        val config = store.loadPairing() ?: return emptyList()
        val routes = mutableListOf<Pair<MonitorEndpoint, PairingConfig>>()
        config.directEndpoints.forEach { endpoint ->
            val displayEndpoint = endpoint.copy(url = directSnapshotUrl(endpoint.url))
            routes += displayEndpoint to config.copy(
                directEndpoints = listOf(endpoint),
                relay = null,
            )
        }
        config.relay?.let { relay ->
            val url = relay.baseUrl.trimEnd('/') +
                "/v1/desktops/${config.desktopId}/devices/${config.deviceId}/snapshot"
            val host = runCatching { URI(relay.baseUrl).host }.getOrNull().orEmpty()
            routes += MonitorEndpoint(
                kind = "relay",
                family = "https",
                interfaceName = "chatx-relay",
                host = host,
                url = url,
            ) to config.copy(
                directEndpoints = emptyList(),
                relay = relay,
            )
        }

        val pool = Executors.newFixedThreadPool(
            minOf(routes.size, 4).coerceAtLeast(1),
        )
        return try {
            routes.map { (endpoint, routeConfig) ->
                pool.submit<EndpointHealth> {
                    val started = SystemClock.elapsedRealtime()
                    try {
                        MonitorConnectionManager.probeOnce(
                            context = appContext,
                            config = routeConfig,
                            timeoutMillis = if (endpoint.kind == "relay") 2_500L else 1_800L,
                        )
                        EndpointHealth(
                            endpoint = endpoint,
                            reachable = true,
                            latencyMs = SystemClock.elapsedRealtime() - started,
                            error = null,
                        )
                    } catch (error: Exception) {
                        EndpointHealth(
                            endpoint = endpoint,
                            reachable = false,
                            latencyMs = null,
                            error = error.message ?: error.javaClass.simpleName,
                        )
                    }
                }
            }.map { it.get() }
        } finally {
            pool.shutdownNow()
        }
    }
}

private fun directSnapshotUrl(source: String): String {
    val base = when {
        source.startsWith("wss://") -> "https://" + source.removePrefix("wss://")
        source.startsWith("ws://") -> "http://" + source.removePrefix("ws://")
        else -> source
    }
    return base.substringBefore('?')
        .replace("/v1/ws/monitor", "/v1/monitor/snapshot")
        .replace("/v1/ws/pair", "/v1/monitor/snapshot")
}
