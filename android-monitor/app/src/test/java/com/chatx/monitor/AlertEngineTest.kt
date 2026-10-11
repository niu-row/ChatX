package com.chatx.monitor

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit

class AlertEngineTest {
    private fun snapshot(
        mcpState: String = "active",
        gapMs: Long? = null,
        stallMs: Long? = null,
        tunnelActive: Boolean = true,
        desiredConnected: Boolean = true,
        receivedAt: Long = 1_000_000L,
        lastCallFinishedAt: Long? = receivedAt - 5_000L,
    ) = MonitorSnapshot(
        serverTime = receivedAt,
        host = null,
        tunnel = TunnelStatus(
            state = if (tunnelActive) "ready" else "stopped",
            health = if (tunnelActive) "healthy" else "down",
            active = tunnelActive,
            updatedAt = receivedAt,
            lastProbeAt = receivedAt,
            lastSuccessfulProbeAt = if (tunnelActive) receivedAt else receivedAt - 20_000L,
            consecutiveFailures = if (tunnelActive) 0 else 2,
            controlPlaneState = if (tunnelActive) "healthy" else "down",
            controlPlaneReason = null,
            controlPlaneFailures = if (tunnelActive) 0 else 3,
            proxyMode = "direct",
            proxySource = null,
            desiredConnected = desiredConnected,
            reconnecting = !tunnelActive,
            reconnectAttempt = if (tunnelActive) 0 else 2,
        ),
        mcp = McpStatus(
            state = mcpState,
            callsLastMinute = 4,
            gapDurationMs = gapMs,
            stallDurationMs = stallMs,
            inFlight = if (mcpState == "stalled") 1 else 0,
            lastToolName = "read_file",
            lastCallStartedAt = lastCallFinishedAt?.minus(100L),
            lastCallFinishedAt = lastCallFinishedAt,
        ),
        endpointUrl = "https://192.168.1.20:18432",
        receivedAt = receivedAt,
    )
    @Test
    fun gapThresholdsFireOnceAndRecover() {
        val engine = AlertEngine()
        val thresholds = setOf(60_000L, 120_000L)
        val first = engine.onSnapshot(snapshot("gap", gapMs = 65_000L), thresholds)
        assertEquals(1, first.count { it.type == AlertType.MCP_GAP })

        val repeated = engine.onSnapshot(snapshot("gap", gapMs = 90_000L), thresholds)
        assertTrue(repeated.isEmpty())

        val second = engine.onSnapshot(snapshot("gap", gapMs = 125_000L), thresholds)
        assertEquals(1, second.count { it.type == AlertType.MCP_GAP })

        val recovered = engine.onSnapshot(snapshot("active"), thresholds)
        assertEquals(1, recovered.count { it.type == AlertType.RECOVERED })
    }

    @Test
    fun stalledIsNotReportedAsGap() {
        val engine = AlertEngine()
        val events = engine.onSnapshot(
            snapshot("stalled", stallMs = 660_000L),
            setOf(60_000L),
        )
        assertEquals(1, events.count { it.type == AlertType.MCP_STALLED })
        assertEquals(0, events.count { it.type == AlertType.MCP_GAP })
    }
    @Test
    fun hostOfflineUsesSameThresholdsAndRecovers() {
        val engine = AlertEngine()
        val thresholds = setOf(60_000L)
        assertTrue(engine.onUnreachable(1_000_000L, thresholds).isEmpty())

        val alert = engine.onUnreachable(1_061_000L, thresholds)
        assertEquals(1, alert.count { it.type == AlertType.HOST_OFFLINE })

        val recovered = engine.onSnapshot(
            snapshot(receivedAt = 1_070_000L),
            thresholds,
        )
        assertEquals(1, recovered.count { it.type == AlertType.RECOVERED })
    }

    @Test
    fun tunnelDownIsDeduplicatedAndRecovers() {
        val engine = AlertEngine()
        val down = snapshot(tunnelActive = false)
        assertEquals(1, engine.onSnapshot(down, emptySet())
            .count { it.type == AlertType.TUNNEL_DOWN })
        assertTrue(engine.onSnapshot(down, emptySet()).isEmpty())
        assertEquals(1, engine.onSnapshot(snapshot(), emptySet())
            .count { it.type == AlertType.RECOVERED })
    }

    @Test
    fun httpsModeSuppressesTunnelAlarmsAndNotifiesOnlyAfterGracePeriod() {
        val engine = AlertEngine()
        val inactive = McpConnectionState(false, false, "stopped")
        val checking = McpConnectionState(true, false, "reconnecting")
        val first = snapshot(tunnelActive = false, desiredConnected = false)
            .copy(connections = McpConnections("https", inactive, checking))
        assertTrue(engine.onSnapshot(first, emptySet()).isEmpty())
        assertTrue(engine.onSnapshot(
            first.copy(serverTime = first.serverTime + 10_000L), emptySet()
        ).isEmpty())
        val delayed = engine.onSnapshot(
            first.copy(serverTime = first.serverTime + 31_000L), emptySet()
        )
        assertEquals(1, delayed.count { it.type == AlertType.HTTPS_DOWN })
        assertEquals(0, delayed.count { it.type == AlertType.TUNNEL_DOWN })
        assertTrue(engine.onSnapshot(
            first.copy(serverTime = first.serverTime + 45_000L), emptySet()
        ).isEmpty())
        val online = first.copy(
            serverTime = first.serverTime + 50_000L,
            connections = McpConnections(
                "https", inactive, McpConnectionState(true, true, "connected")
            )
        )
        assertEquals(1, engine.onSnapshot(online, emptySet())
            .count { it.type == AlertType.RECOVERED })
        assertTrue(engine.onSnapshot(online.copy(
            connections = McpConnections("none", inactive, inactive)
        ), emptySet()).isEmpty())
    }

    @Test
    fun simultaneousOfflineChecksEmitOnlyOneAlert() {
        val engine = AlertEngine()
        val thresholds = setOf(60_000L)
        assertTrue(engine.onUnreachable(1_000L, thresholds).isEmpty())
        val pool = Executors.newFixedThreadPool(8)
        val ready = CountDownLatch(1)
        try {
            val results = (0 until 80).map {
                pool.submit<List<AlertEvent>> {
                    check(ready.await(5, TimeUnit.SECONDS))
                    engine.onUnreachable(61_001L, thresholds)
                }
            }
            ready.countDown()
            val count = results.sumOf { future ->
                future.get(5, TimeUnit.SECONDS).count {
                    it.type == AlertType.HOST_OFFLINE
                }
            }
            assertEquals(1, count)
            val recovered = engine.onSnapshot(
                snapshot(receivedAt = 70_000L), thresholds,
            )
            assertEquals(1, recovered.count { it.type == AlertType.RECOVERED })
        } finally {
            pool.shutdownNow()
        }
    }

    @Test
    fun continuousModeTurnsIdleIntoGapWatch() {
        val engine = AlertEngine()
        val snapshot = snapshot(
            mcpState = "idle",
            receivedAt = 1_200_000L,
            lastCallFinishedAt = 1_100_000L,
        )
        val events = engine.onSnapshot(
            snapshot,
            setOf(60_000L),
            continuousStartedAt = 1_000_000L,
        )
        assertEquals(1, events.count { it.type == AlertType.MCP_GAP })
    }
}
