package com.chatx.monitor

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.Base64
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

class SecureStore(context: Context) {
    private val prefs = context.getSharedPreferences("chatx_monitor", Context.MODE_PRIVATE)

    fun savePairing(config: PairingConfig) = synchronized(PAIRING_LOCK) {
        val cipher = Cipher.getInstance(TRANSFORMATION)
        cipher.init(Cipher.ENCRYPT_MODE, getOrCreateKey())
        val encrypted = cipher.doFinal(config.toJson().toByteArray(Charsets.UTF_8))
        prefs.edit()
            .putString("pairing_iv", Base64.encodeToString(cipher.iv, Base64.NO_WRAP))
            .putString("pairing_data", Base64.encodeToString(encrypted, Base64.NO_WRAP))
            .apply()
    }
    fun loadPairing(): PairingConfig? = synchronized(PAIRING_LOCK) {
        val iv = prefs.getString("pairing_iv", null) ?: return@synchronized null
        val data = prefs.getString("pairing_data", null) ?: return@synchronized null
        runCatching {
            val cipher = Cipher.getInstance(TRANSFORMATION)
            cipher.init(
                Cipher.DECRYPT_MODE,
                getOrCreateKey(),
                GCMParameterSpec(128, Base64.decode(iv, Base64.NO_WRAP)),
            )
            val clear = cipher.doFinal(Base64.decode(data, Base64.NO_WRAP))
            PairingConfig.parse(clear.toString(Charsets.UTF_8))
        }.getOrNull()
    }

    fun hasPairing(
        expectedDesktopId: String,
        expectedDeviceId: String,
    ): Boolean = synchronized(PAIRING_LOCK) {
        val current = loadPairing() ?: return@synchronized false
        current.desktopId == expectedDesktopId &&
            current.deviceId == expectedDeviceId
    }

    fun clearPairing() = synchronized(PAIRING_LOCK) {
        prefs.edit()
            .remove("pairing_iv")
            .remove("pairing_data")
            .remove("last_endpoint")
            .remove("last_snapshot")
            .remove("event_history")
            .remove("monitor_service_running")
            .remove("route_policy")
            .remove("manual_route_selector")
            .apply()
    }

    fun updateDirectEndpoints(
        expectedDesktopId: String,
        expectedDeviceId: String,
        endpoints: List<MonitorEndpoint>,
    ): Boolean = synchronized(PAIRING_LOCK) {
        if (endpoints.isEmpty()) return@synchronized false
        val current = loadPairing() ?: return@synchronized false
        if (
            current.desktopId != expectedDesktopId ||
            current.deviceId != expectedDeviceId
        ) {
            return@synchronized false
        }
        val normalized = endpoints
            .filter {
                it.isSupportedDirectRoute() &&
                    it.url.endsWith("/v1/ws/monitor")
            }
            .distinctBy { it.url }
        if (
            normalized.isEmpty() ||
            normalized == current.directEndpoints
        ) {
            return@synchronized false
        }
        savePairing(current.copy(directEndpoints = normalized))
        true
    }

    fun getLastEndpoint(): String? = prefs.getString("last_endpoint", null)

    fun setLastEndpoint(
        expectedDesktopId: String,
        expectedDeviceId: String,
        url: String,
    ): Boolean = synchronized(PAIRING_LOCK) {
        val current = loadPairing() ?: return@synchronized false
        if (
            current.desktopId != expectedDesktopId ||
            current.deviceId != expectedDeviceId
        ) {
            return@synchronized false
        }
        prefs.edit().putString("last_endpoint", url).apply()
        true
    }

    fun getRoutePolicy(): RoutePolicy = runCatching {
        RoutePolicy.valueOf(
            prefs.getString("route_policy", RoutePolicy.LAN_FIRST.name)
                ?: RoutePolicy.LAN_FIRST.name,
        )
    }.getOrDefault(RoutePolicy.LAN_FIRST)

    fun setRoutePolicy(policy: RoutePolicy) {
        prefs.edit().putString("route_policy", policy.name).apply()
    }

    fun getManualRouteSelector(): ManualRouteSelector? {
        val raw = prefs.getString("manual_route_selector", null) ?: return null
        return runCatching {
            val root = org.json.JSONObject(raw)
            ManualRouteSelector(
                kind = root.optString("kind"),
                family = root.optString("family"),
                interfaceName = root.optString("interface"),
            ).takeIf { it.kind.isNotBlank() }
        }.getOrNull()
    }

    fun setManualRouteSelector(selector: ManualRouteSelector?) {
        if (selector == null) {
            prefs.edit().remove("manual_route_selector").apply()
            return
        }
        val root = org.json.JSONObject().apply {
            put("kind", selector.kind)
            put("family", selector.family)
            put("interface", selector.interfaceName)
        }
        prefs.edit().putString("manual_route_selector", root.toString()).apply()
    }
    fun getAlertThresholds(): Set<Long> {
        val raw = prefs.getString("alert_thresholds", null)
            ?: return setOf(120_000L, 300_000L)
        return raw.split(',')
            .mapNotNull { it.toLongOrNull() }
            .filter { it in setOf(60_000L, 120_000L, 180_000L, 300_000L) }
            .toSet()
            .ifEmpty { setOf(120_000L, 300_000L) }
    }

    fun setAlertThresholds(values: Set<Long>) {
        val normalized = values
            .filter { it in setOf(60_000L, 120_000L, 180_000L, 300_000L) }
            .sorted()
            .joinToString(",")
        prefs.edit().putString("alert_thresholds", normalized).apply()
    }

    fun setLastSnapshotJson(
        expectedDesktopId: String,
        expectedDeviceId: String,
        value: String,
    ): Boolean = synchronized(PAIRING_LOCK) {
        val current = loadPairing() ?: return@synchronized false
        if (
            current.desktopId != expectedDesktopId ||
            current.deviceId != expectedDeviceId
        ) {
            return@synchronized false
        }
        prefs.edit().putString("last_snapshot", value).apply()
        true
    }

    fun getLastSnapshotJson(): String? = prefs.getString("last_snapshot", null)

    fun setMonitorServiceRunning(running: Boolean) {
        prefs.edit().putBoolean("monitor_service_running", running).apply()
    }

    fun isMonitorServiceRunning(): Boolean =
        prefs.getBoolean("monitor_service_running", false)

    fun setContinuousMode(enabled: Boolean) {
        val editor = prefs.edit().putBoolean("continuous_mode", enabled)
        if (enabled) editor.putLong("continuous_mode_started_at", System.currentTimeMillis())
        else editor.remove("continuous_mode_started_at")
        editor.apply()
    }

    fun isContinuousMode(): Boolean = prefs.getBoolean("continuous_mode", false)

    fun continuousModeStartedAt(): Long? {
        if (!isContinuousMode()) return null
        return prefs.getLong("continuous_mode_started_at", 0L).takeIf { it > 0L }
    }

    fun getPollIntervalSeconds(): Long =
        prefs.getLong("poll_interval_seconds", 10L)
            .takeIf { it in setOf(10L, 15L, 30L, 60L) }
            ?: 10L

    fun setPollIntervalSeconds(seconds: Long) {
        val value = seconds.takeIf {
            it in setOf(10L, 15L, 30L, 60L)
        } ?: 10L
        prefs.edit().putLong("poll_interval_seconds", value).apply()
    }

    fun getSnapshotStaleSeconds(): Long =
        prefs.getLong("snapshot_stale_seconds", 90L)
            .takeIf { it in setOf(30L, 60L, 90L, 180L) }
            ?: 90L

    fun setSnapshotStaleSeconds(seconds: Long) {
        val value = seconds.takeIf {
            it in setOf(30L, 60L, 90L, 180L)
        } ?: 90L
        prefs.edit().putLong("snapshot_stale_seconds", value).apply()
    }

    fun getOfflineFailureThreshold(): Int =
        prefs.getInt("offline_failure_threshold", 3)
            .takeIf { it in setOf(2, 3, 5) }
            ?: 3

    fun setOfflineFailureThreshold(value: Int) {
        val normalized = value.takeIf { it in setOf(2, 3, 5) } ?: 3
        prefs.edit().putInt("offline_failure_threshold", normalized).apply()
    }

    fun appendEvent(event: MonitorEvent) {
        val current = loadEvents().toMutableList()
        current.add(0, event)
        val trimmed = current.take(MAX_EVENTS)
        val array = org.json.JSONArray()
        trimmed.forEach { item ->
            array.put(org.json.JSONObject().apply {
                put("at", item.at)
                put("type", item.type)
                put("title", item.title)
                put("message", item.message)
            })
        }
        prefs.edit().putString("event_history", array.toString()).apply()
    }

    fun loadEvents(): List<MonitorEvent> {
        val raw = prefs.getString("event_history", null) ?: return emptyList()
        return runCatching {
            val array = org.json.JSONArray(raw)
            buildList {
                for (index in 0 until array.length()) {
                    val item = array.getJSONObject(index)
                    add(MonitorEvent(
                        at = item.optLong("at", 0L),
                        type = item.optString("type", "INFO"),
                        title = item.optString("title", ""),
                        message = item.optString("message", ""),
                    ))
                }
            }
        }.getOrDefault(emptyList())
    }

    fun clearEvents() {
        prefs.edit().remove("event_history").apply()
    }

    private fun getOrCreateKey(): SecretKey {
        val keyStore = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
        (keyStore.getKey(KEY_ALIAS, null) as? SecretKey)?.let { return it }
        val keyGenerator = KeyGenerator.getInstance(
            KeyProperties.KEY_ALGORITHM_AES,
            "AndroidKeyStore",
        )
        val spec = KeyGenParameterSpec.Builder(
            KEY_ALIAS,
            KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT,
        )
            .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
            .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
            .setKeySize(256)
            .build()
        keyGenerator.init(spec)
        return keyGenerator.generateKey()
    }

    companion object {
        private val PAIRING_LOCK = Any()
        private const val KEY_ALIAS = "chatx-monitor-pairing"
        private const val TRANSFORMATION = "AES/GCM/NoPadding"
        private const val MAX_EVENTS = 100
    }
}
