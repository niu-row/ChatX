package com.chatx.monitor

data class MonitorRouteCandidate(
    val kind: String,
    val family: String,
    val interfaceName: String,
    val snapshotUrl: String,
    val revokeUrl: String,
    val controlUrl: String,
    val token: String,
    val relay: Boolean,
)

object MonitorRoutePlanner {
    fun candidates(
        identity: PairingConfig,
        routes: PairingConfig = identity,
    ): List<MonitorRouteCandidate> = buildList {
        routes.directEndpoints.forEach { endpoint ->
            val snapshotUrl = directHttpsUrl(
                endpoint.url,
                "/v1/monitor/snapshot",
            )
            val revokeUrl = directHttpsUrl(
                endpoint.url,
                "/v1/monitor/revoke",
            )
            add(
                MonitorRouteCandidate(
                    kind = endpoint.kind,
                    family = endpoint.family,
                    interfaceName = endpoint.interfaceName,
                    snapshotUrl = appendIdentityQuery(
                        snapshotUrl,
                        identity,
                    ),
                    revokeUrl = appendIdentityQuery(
                        revokeUrl,
                        identity,
                    ),
                    controlUrl = appendIdentityQuery(
                        endpoint.url,
                        identity,
                    ),
                    token = identity.directToken,
                    relay = false,
                ),
            )
        }
        routes.relay?.let { relay ->
            val base = relay.baseUrl.trimEnd('/')
            val root =
                "$base/v1/desktops/${identity.desktopId}" +
                    "/devices/${identity.deviceId}"
            val wsBase = when {
                base.startsWith("https://") ->
                    "wss://" + base.removePrefix("https://")
                base.startsWith("http://") ->
                    "ws://" + base.removePrefix("http://")
                else -> base
            }
            add(
                MonitorRouteCandidate(
                    kind = "relay",
                    family = "https",
                    interfaceName = "chatx-relay",
                    snapshotUrl = "$root/snapshot",
                    revokeUrl = "$root/revoke-self",
                    controlUrl =
                        "$wsBase/v1/ws/device" +
                            "?desktopId=${identity.desktopId}" +
                            "&deviceId=${identity.deviceId}",
                    token = relay.deviceToken,
                    relay = true,
                ),
            )
        }
    }

    fun ordered(
        values: List<MonitorRouteCandidate>,
        policy: RoutePolicy,
        lastEndpoint: String?,
        selector: ManualRouteSelector?,
    ): List<MonitorRouteCandidate> =
        values.sortedWith(
            compareBy<MonitorRouteCandidate> { candidate ->
                when {
                    policy == RoutePolicy.AUTO &&
                        candidate.snapshotUrl == lastEndpoint -> -1
                    else -> routeRank(candidate, policy, selector)
                }
            }.thenBy { it.snapshotUrl },
        )

    private fun routeRank(
        candidate: MonitorRouteCandidate,
        policy: RoutePolicy,
        selector: ManualRouteSelector?,
    ): Int {
        val directRank = when (candidate.kind) {
            "lan" -> 0
            "tailscale" -> 1
            "ipv6" -> 2
            else -> 3
        }
        return when (policy) {
            RoutePolicy.AUTO ->
                directRank + if (candidate.relay) 10 else 0
            RoutePolicy.LAN_FIRST ->
                if (candidate.relay) 10 else directRank
            RoutePolicy.RELAY_FIRST ->
                if (candidate.relay) 0 else directRank + 10
            RoutePolicy.MANUAL -> when {
                selector != null &&
                    selectorMatches(selector, candidate) -> 0
                candidate.relay -> 10
                else -> directRank + 20
            }
        }
    }

    private fun selectorMatches(
        selector: ManualRouteSelector,
        candidate: MonitorRouteCandidate,
    ): Boolean =
        candidate.kind == selector.kind &&
            (
                selector.family.isBlank() ||
                    candidate.family == selector.family ||
                    (
                        selector.family == "wss" &&
                            candidate.family == "https"
                    )
            ) &&
            (
                selector.interfaceName.isBlank() ||
                    candidate.interfaceName == selector.interfaceName
            )

    private fun appendIdentityQuery(
        url: String,
        config: PairingConfig,
    ): String {
        val separator = if ('?' in url) '&' else '?'
        return "$url${separator}desktopId=${config.desktopId}" +
            "&deviceId=${config.deviceId}"
    }

    private fun directHttpsUrl(
        source: String,
        path: String,
    ): String {
        val base = when {
            source.startsWith("wss://") ->
                "https://" + source.removePrefix("wss://")
            source.startsWith("ws://") ->
                "http://" + source.removePrefix("ws://")
            else -> source
        }
        return base
            .substringBefore('?')
            .replace("/v1/ws/monitor", path)
            .replace("/v1/ws/pair", path)
    }
}
