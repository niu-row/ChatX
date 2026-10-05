package com.chatx.monitor

enum class RoutePolicy {
    AUTO,
    LAN_FIRST,
    RELAY_FIRST,
    MANUAL,
}

data class ManualRouteSelector(
    val kind: String,
    val family: String,
    val interfaceName: String,
) {
    fun matches(endpoint: MonitorEndpoint): Boolean =
        endpoint.kind == kind &&
            (family.isBlank() ||
                endpoint.family == family ||
                (family == "wss" && endpoint.family == "https")) &&
            (interfaceName.isBlank() || endpoint.interfaceName == interfaceName)

    companion object {
        fun from(endpoint: MonitorEndpoint): ManualRouteSelector =
            ManualRouteSelector(
                kind = endpoint.kind,
                family = endpoint.family,
                interfaceName = endpoint.interfaceName,
            )
    }
}
