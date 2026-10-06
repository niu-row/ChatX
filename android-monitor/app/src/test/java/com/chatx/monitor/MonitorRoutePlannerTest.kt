package com.chatx.monitor

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class MonitorRoutePlannerTest {
    private val desktopId = "d_0123456789abcdef0123456789abcdef"
    private val deviceId = "dev_0123456789abcdef"

    private fun config(): PairingConfig =
        PairingConfig(
            desktopId = desktopId,
            port = 18432,
            fingerprintSha256 = "00".repeat(32),
            deviceId = deviceId,
            directToken = "11".repeat(32),
            deviceKey = "unused",
            directEndpoints = listOf(
                MonitorEndpoint(
                    kind = "lan",
                    family = "ipv4",
                    interfaceName = "Wi-Fi",
                    host = "192.168.1.4",
                    url = "wss://192.168.1.4:18432/v1/ws/monitor",
                ),
            ),
            relay = RelayEnrollment(
                baseUrl = "https://relay.example.com",
                desktopId = desktopId,
                deviceToken = "22".repeat(32),
            ),
        )

    @Test
    fun candidatesDeriveDirectAndRelayUrlsFromOneSource() {
        val candidates = MonitorRoutePlanner.candidates(config())
        assertEquals(2, candidates.size)

        val direct = candidates.first { !it.relay }
        assertTrue(
            direct.snapshotUrl.startsWith(
                "https://192.168.1.4:18432/v1/monitor/snapshot?",
            ),
        )
        assertTrue(direct.snapshotUrl.contains("desktopId=$desktopId"))
        assertTrue(direct.snapshotUrl.contains("deviceId=$deviceId"))
        assertTrue(
            direct.controlUrl.startsWith(
                "wss://192.168.1.4:18432/v1/ws/monitor?",
            ),
        )
        assertTrue(
            direct.revokeUrl.startsWith(
                "https://192.168.1.4:18432/v1/monitor/revoke?",
            ),
        )

        val relay = candidates.first { it.relay }
        assertEquals(
            "https://relay.example.com/v1/desktops/$desktopId" +
                "/devices/$deviceId/snapshot",
            relay.snapshotUrl,
        )
        assertEquals(
            "wss://relay.example.com/v1/ws/device" +
                "?desktopId=$desktopId&deviceId=$deviceId",
            relay.controlUrl,
        )
    }

    @Test
    fun orderingUsesTheSameSnapshotIdentityAcrossPolicies() {
        val candidates = MonitorRoutePlanner.candidates(config())
        val direct = candidates.first { !it.relay }
        val relay = candidates.first { it.relay }

        val auto = MonitorRoutePlanner.ordered(
            values = candidates,
            policy = RoutePolicy.AUTO,
            lastEndpoint = relay.snapshotUrl,
            selector = null,
        )
        assertEquals(relay.snapshotUrl, auto.first().snapshotUrl)

        val relayFirst = MonitorRoutePlanner.ordered(
            values = candidates,
            policy = RoutePolicy.RELAY_FIRST,
            lastEndpoint = direct.snapshotUrl,
            selector = null,
        )
        assertEquals(relay.snapshotUrl, relayFirst.first().snapshotUrl)
    }
}
