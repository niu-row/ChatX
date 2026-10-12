package com.chatx.monitor

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
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
                    interfaceName = "vEthernet (WSL)",
                    host = "172.29.32.1",
                    url = "wss://172.29.32.1:18432/v1/ws/monitor",
                ),
                MonitorEndpoint(
                    kind = "lan",
                    family = "ipv4",
                    interfaceName = "Wi-Fi",
                    host = "192.168.1.4",
                    url = "wss://192.168.1.4:18432/v1/ws/monitor",
                ),
                MonitorEndpoint(
                    kind = "ipv6",
                    family = "ipv6",
                    interfaceName = "Ethernet",
                    host = "2406:da1c:abcd::1",
                    url = "wss://[2406:da1c:abcd::1]:18432/v1/ws/monitor",
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
        assertEquals(3, candidates.size)

        val direct = candidates.first { it.kind == "lan" }
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

        val ipv6 = candidates.first { it.kind == "ipv6" }
        assertTrue(
            ipv6.snapshotUrl.startsWith(
                "https://[2406:da1c:abcd::1]:18432/v1/monitor/snapshot?",
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
    fun replacementPairingCannotLendRoutesOrRelayTokenToOldManager() {
        val current = config()
        val replaced = current.copy(
            // Some re-pairing and token-rotation paths can reuse IDs.
            directToken = "ff".repeat(32),
            relay = current.relay!!.copy(deviceToken = "ee".repeat(32)),
        )
        assertFalse(current.sameCredentialIdentity(replaced))
        assertTrue(MonitorRoutePlanner.candidates(current, replaced).isEmpty())

        val rotatedKey = current.copy(deviceKey = "different-e2ee-key")
        assertFalse(current.sameCredentialIdentity(rotatedKey))
        assertTrue(MonitorRoutePlanner.candidates(current, rotatedKey).isEmpty())

        val rotatedTls = current.copy(fingerprintSha256 = "aa".repeat(32))
        assertFalse(current.sameCredentialIdentity(rotatedTls))
        assertTrue(MonitorRoutePlanner.candidates(current, rotatedTls).isEmpty())
    }

    @Test
    fun endpointDiscoveryCanRefreshRoutesWithoutChangingPairingIdentity() {
        val current = config()
        val updatedRoutes = current.copy(
            directEndpoints = current.directEndpoints.filter { it.kind == "ipv6" },
        )
        assertTrue(current.sameCredentialIdentity(updatedRoutes))
        val candidates = MonitorRoutePlanner.candidates(current, updatedRoutes)
        assertEquals(2, candidates.size)
        assertEquals(1, candidates.count { it.relay })
        assertEquals(1, candidates.count { it.kind == "ipv6" })
    }

    @Test
    fun directEndpointsAcceptNumericIpsWithoutLookingUpArbitraryDns() {
        val lan = config().directEndpoints.first { it.kind == "lan" && it.interfaceName == "Wi-Fi" }
        val ipv6 = config().directEndpoints.first { it.kind == "ipv6" }
        assertTrue(lan.isSupportedDirectRoute())
        assertTrue(ipv6.isSupportedDirectRoute())
        assertFalse(lan.copy(url = "wss://internal-host.example:18432/v1/ws/monitor")
            .isSupportedDirectRoute())
        assertFalse(lan.copy(url = "wss://999.168.1.4:18432/v1/ws/monitor")
            .isSupportedDirectRoute())
        assertFalse(ipv6.copy(url = "wss://[fe80::1]:18432/v1/ws/monitor")
            .isSupportedDirectRoute())
    }

    @Test
    fun orderingUsesTheSameSnapshotIdentityAcrossPolicies() {
        val candidates = MonitorRoutePlanner.candidates(config())
        val direct = candidates.first { it.kind == "lan" }
        val ipv6 = candidates.first { it.kind == "ipv6" }
        val relay = candidates.first { it.relay }

        val auto = MonitorRoutePlanner.ordered(
            values = candidates,
            policy = RoutePolicy.AUTO,
            lastEndpoint = relay.snapshotUrl,
            selector = null,
        )
        assertEquals(relay.snapshotUrl, auto.first().snapshotUrl)

        val lanFirst = MonitorRoutePlanner.ordered(
            values = candidates,
            policy = RoutePolicy.LAN_FIRST,
            lastEndpoint = ipv6.snapshotUrl,
            selector = null,
        )
        assertEquals(direct.snapshotUrl, lanFirst.first().snapshotUrl)
        assertEquals(ipv6.snapshotUrl, lanFirst[1].snapshotUrl)

        val relayFirst = MonitorRoutePlanner.ordered(
            values = candidates,
            policy = RoutePolicy.RELAY_FIRST,
            lastEndpoint = direct.snapshotUrl,
            selector = null,
        )
        assertEquals(relay.snapshotUrl, relayFirst.first().snapshotUrl)
    }
}
