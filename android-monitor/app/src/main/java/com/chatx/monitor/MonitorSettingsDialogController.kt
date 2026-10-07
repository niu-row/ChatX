package com.chatx.monitor

import android.app.Activity
import android.app.AlertDialog

internal class MonitorSettingsDialogController(
    private val activity: Activity,
    private val store: SecureStore,
    private val currentEndpointUrl: () -> String?,
    private val renderCurrentPage: () -> Unit,
    private val toast: (String) -> Unit,
) {
    fun showPollIntervalDialog() {
        val values = longArrayOf(10L, 15L, 30L, 60L)
        val labels = values.map { "${it} 秒" }.toTypedArray()
        val selected = values.indexOf(
            store.getPollIntervalSeconds(),
        )
        AlertDialog.Builder(activity)
            .setTitle("刷新间隔")
            .setSingleChoiceItems(
                labels,
                selected,
            ) { dialog, which ->
                val value = values[which]
                store.setPollIntervalSeconds(value)
                val minimumStale = minimumStaleForPoll(value)
                if (
                    store.getSnapshotStaleSeconds() < minimumStale
                ) {
                    store.setSnapshotStaleSeconds(minimumStale)
                }
                reloadMonitorSettings()
                dialog.dismiss()
                renderCurrentPage()
            }
            .setNegativeButton("取消", null)
            .show()
    }

    fun showSnapshotStaleDialog() {
        val values = longArrayOf(30L, 60L, 90L, 180L)
        val labels = values.map { "${it} 秒" }.toTypedArray()
        val selected = values.indexOf(
            store.getSnapshotStaleSeconds(),
        )
        AlertDialog.Builder(activity)
            .setTitle("快照过期")
            .setSingleChoiceItems(
                labels,
                selected,
            ) { dialog, which ->
                val value = values[which]
                val minimum =
                    store.getPollIntervalSeconds() * 3L
                if (value < minimum) {
                    toast(
                        "快照过期至少应为刷新间隔的 3 倍。",
                    )
                    return@setSingleChoiceItems
                }
                store.setSnapshotStaleSeconds(value)
                reloadMonitorSettings()
                dialog.dismiss()
                renderCurrentPage()
            }
            .setNegativeButton("取消", null)
            .show()
    }
    fun showOfflineFailureDialog() {
        val values = intArrayOf(2, 3, 5)
        val labels = values
            .map { "连续 ${it} 次请求失败" }
            .toTypedArray()
        val selected = values.indexOf(
            store.getOfflineFailureThreshold(),
        )
        AlertDialog.Builder(activity)
            .setTitle("离线确认")
            .setSingleChoiceItems(
                labels,
                selected,
            ) { dialog, which ->
                store.setOfflineFailureThreshold(
                    values[which],
                )
                reloadMonitorSettings()
                dialog.dismiss()
                renderCurrentPage()
            }
            .setNegativeButton("取消", null)
            .show()
    }

    fun showRoutePolicyDialog() {
        val policies = arrayOf(
            RoutePolicy.LAN_FIRST,
            RoutePolicy.RELAY_FIRST,
            RoutePolicy.AUTO,
            RoutePolicy.MANUAL,
        )
        val labels =
            policies.map(::routePolicyLabel).toTypedArray()
        val selected = policies
            .indexOf(store.getRoutePolicy())
            .coerceAtLeast(0)
        AlertDialog.Builder(activity)
            .setTitle("选择路径策略")
            .setSingleChoiceItems(
                labels,
                selected,
            ) { dialog, which ->
                val policy = policies[which]
                store.setRoutePolicy(policy)
                if (
                    policy == RoutePolicy.MANUAL &&
                    store.getManualRouteSelector() == null
                ) {
                    val routes = store
                        .loadPairing()
                        ?.let(::configuredRoutes)
                        .orEmpty()
                    val currentBase =
                        currentEndpointUrl()
                            ?.substringBefore('?')
                    val preferred = routes
                        .firstOrNull {
                            it.url == currentBase
                        }
                        ?: routes.firstOrNull()
                    store.setManualRouteSelector(
                        preferred?.let(
                            ManualRouteSelector::from,
                        ),
                    )
                }
                if (store.isMonitorServiceRunning()) {
                    MonitorService.reevaluate(activity)
                }
                dialog.dismiss()
                renderCurrentPage()
            }
            .setNegativeButton("取消", null)
            .show()
    }
    fun showManualRouteDialog(
        routes: List<MonitorEndpoint>,
    ) {
        if (routes.isEmpty()) {
            toast("当前没有可选择的路径。")
            return
        }
        val current = store.getManualRouteSelector()
        val labels = routes.map { endpoint ->
            "${endpointKindLabel(endpoint.kind)} · " +
                "${endpoint.family.uppercase()} · " +
                endpoint.host.ifBlank {
                    endpoint.interfaceName
                }
        }.toTypedArray()
        val selected = routes.indexOfFirst {
            current?.matches(it) == true
        }
        AlertDialog.Builder(activity)
            .setTitle("选择手动首选路径")
            .setSingleChoiceItems(
                labels,
                selected,
            ) { dialog, which ->
                store.setManualRouteSelector(
                    ManualRouteSelector.from(routes[which]),
                )
                store.setRoutePolicy(RoutePolicy.MANUAL)
                if (store.isMonitorServiceRunning()) {
                    MonitorService.reevaluate(activity)
                }
                dialog.dismiss()
                renderCurrentPage()
            }
            .setNegativeButton("取消", null)
            .show()
    }

    private fun reloadMonitorSettings() {
        if (store.isMonitorServiceRunning()) {
            MonitorService.reload(activity)
        }
    }
}
