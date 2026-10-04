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
        store.updateDirectEndpoints(snapshot.endpoints)
        return snapshot
    }

    fun testEndpoints(): List<EndpointHealth> {
        val config = store.loadPairing() ?: return emptyList()
        val routes = mutableListOf<Pair<MonitorEndpoint, PairingConfig>>()
        config.directEndpoints.forEach { endpoint ->
            routes += endpoint to config.copy(
                directEndpoints = listOf(endpoint),
                relay = null,
            )
        }
        config.relay?.let { relay ->
            val wsScheme = if (relay.baseUrl.startsWith("https://")) {
                "wss://" + relay.baseUrl.removePrefix("https://")
            } else {
                "ws://" + relay.baseUrl.removePrefix("http://")
            }
            val url = "${wsScheme.trimEnd('/')}/v1/ws/device"
            val host = runCatching { URI(relay.baseUrl).host }.getOrNull().orEmpty()
            routes += MonitorEndpoint(
                kind = "relay",
                family = "wss",
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
