import fs from 'node:fs';

const root = 'android-monitor';
const base = `${root}/app/src/main/java/com/chatx/monitor`;
const gradle = fs.readFileSync(`${root}/app/build.gradle.kts`, 'utf8');
const manifest = fs.readFileSync(`${root}/app/src/main/AndroidManifest.xml`, 'utf8');
const models = fs.readFileSync(`${base}/Models.kt`, 'utf8');
const store = fs.readFileSync(`${base}/SecureStore.kt`, 'utf8');
const service = fs.readFileSync(`${base}/MonitorService.kt`, 'utf8');
const connection = fs.readFileSync(`${base}/MonitorConnectionManager.kt`, 'utf8');
const crypto = fs.readFileSync(`${base}/MonitorCrypto.kt`, 'utf8');
const tls = fs.readFileSync(`${base}/PinnedTls.kt`, 'utf8');
const repository = fs.readFileSync(`${base}/MonitorRepository.kt`, 'utf8');
const parser = fs.readFileSync(`${base}/MonitorSnapshotParser.kt`, 'utf8');
const activity = fs.readFileSync(`${base}/MainActivity.kt`, 'utf8');
const alerts = fs.readFileSync(`${base}/AlertEngine.kt`, 'utf8');
const codec = fs.readFileSync(`${base}/StatusCodec.kt`, 'utf8');
const uiKit = fs.readFileSync(`${base}/UiKit.kt`, 'utf8');
const routePolicy = fs.readFileSync(`${base}/RoutePolicy.kt`, 'utf8');

function requireText(label, text, needle) {
  if (!text.includes(needle)) throw new Error(`${label} is missing: ${needle}`);
}
function rejectText(label, text, needle) {
  if (text.includes(needle)) throw new Error(`${label} must not contain: ${needle}`);
}
requireText('build.gradle.kts', gradle, 'compileSdk = 37');
requireText('build.gradle.kts', gradle, 'targetSdk = 37');
requireText('build.gradle.kts', gradle, 'play-services-code-scanner:16.1.0');
requireText('AndroidManifest.xml', manifest, 'android.permission.ACCESS_LOCAL_NETWORK');
requireText('AndroidManifest.xml', manifest, 'android.permission.FOREGROUND_SERVICE_CONNECTED_DEVICE');
requireText('AndroidManifest.xml', manifest, 'android:foregroundServiceType="connectedDevice"');
requireText('AndroidManifest.xml', manifest, 'android:usesCleartextTraffic="false"');

requireText('Models.kt', models, 'schemaVersion", -1) == 3');
requireText('Models.kt', models, 'chatx-monitor-wss-v1');
requireText('Models.kt', models, 'require(root.optString("scheme") == "wss")');
requireText('Models.kt', models, 'data class RelayEnrollment');
requireText('Models.kt', models, 'val directToken: String');
requireText('Models.kt', models, 'val deviceKey: String');
requireText('Models.kt', models, 'val relay: RelayEnrollment?');
requireText('Models.kt', models, 'url.startsWith("wss://")');
requireText('Models.kt', models, 'data class HostStatus');
requireText('Models.kt', models, 'val controlPlaneState: String');
requireText('Models.kt', models, 'val proxyMode: String');
requireText('SecureStore.kt', store, '"AndroidKeyStore"');
requireText('SecureStore.kt', store, 'AES/GCM/NoPadding');
requireText('SecureStore.kt', store, 'updateDirectEndpoints');
requireText('SecureStore.kt', store, 'it.url.startsWith("wss://")');

requireText('PinnedTls.kt', tls, 'MessageDigest.getInstance("SHA-256")');
requireText('PinnedTls.kt', tls, 'MessageDigest.isEqual');
requireText('PinnedTls.kt', tls, 'pingInterval(15, TimeUnit.SECONDS)');
requireText('PinnedTls.kt', tls, 'hostnameVerifier { _, _ -> true }');

requireText('MonitorCrypto.kt', crypto, 'AES/GCM/NoPadding');
requireText('MonitorCrypto.kt', crypto, 'chatx-monitor-v1|snapshot|');
requireText('MonitorCrypto.kt', crypto, 'config.desktopId');
requireText('MonitorCrypto.kt', crypto, 'config.deviceId');

requireText('MonitorConnectionManager.kt', connection, 'WssClients.direct');
requireText('MonitorConnectionManager.kt', connection, 'WssClients.relay');
rejectText('MonitorConnectionManager.kt', connection, 'newWebSocket');
rejectText('MonitorConnectionManager.kt', connection, 'WebSocketListener');
requireText('MonitorConnectionManager.kt', connection, 'orderedCandidates(candidates)');
requireText('MonitorConnectionManager.kt', connection, 'fun updateRoutes(endpoints: List<MonitorEndpoint>)');
requireText('MonitorConnectionManager.kt', connection, 'POLL_INTERVAL_SECONDS = 10L');
requireText('MonitorConnectionManager.kt', connection, 'SNAPSHOT_STALE_MS = 90_000L');
requireText('MonitorConnectionManager.kt', connection, 'fun probeOnce(');
requireText('MonitorConnectionManager.kt', connection, '/v1/monitor/snapshot');
requireText('MonitorConnectionManager.kt', connection, '/revoke-self');
requireText('MonitorConnectionManager.kt', connection, '"revoked"');
requireText('MonitorConnectionManager.kt', connection, 'fun revokePairing(');
requireText('MonitorConnectionManager.kt', connection, 'MonitorCrypto.decryptSnapshot');
requireText('MonitorService.kt', service, 'MonitorConnectionManager(');
requireText('MonitorService.kt', service, '正在建立 ChatX HTTPS 监控');
requireText('MonitorService.kt', service, 'FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE');
requireText('MonitorService.kt', service, 'store.continuousModeStartedAt()');
requireText('MonitorService.kt', service, 'store.appendEvent');
rejectText('MonitorService.kt', service, 'scheduleWithFixedDelay');
rejectText('MonitorService.kt', service, 'fetchSnapshot()');

requireText('MonitorRepository.kt', repository, 'MonitorConnectionManager.fetchOnce');
requireText('MonitorRepository.kt', repository, 'store.updateDirectEndpoints(snapshot.endpoints)');
requireText('MonitorRepository.kt', repository, 'kind = "relay"');
requireText('MonitorRepository.kt', repository, 'Executors.newFixedThreadPool');
requireText('MonitorRepository.kt', repository, 'MonitorConnectionManager.probeOnce');
requireText('RoutePolicy.kt', routePolicy, 'LAN_FIRST');
requireText('RoutePolicy.kt', routePolicy, 'RELAY_FIRST');
requireText('RoutePolicy.kt', routePolicy, 'MANUAL');

requireText('MonitorSnapshotParser.kt', parser, 'parseRecentCalls');
requireText('MonitorSnapshotParser.kt', parser, 'controlPlaneState');
requireText('MonitorSnapshotParser.kt', parser, 'proxySource');
requireText('MonitorSnapshotParser.kt', parser, 'url.startsWith("wss://")');

requireText('MainActivity.kt', activity, 'GmsBarcodeScanning.getClient');
requireText('MainActivity.kt', activity, 'HTTPS 监控');
requireText('MainActivity.kt', activity, '不依赖常驻 WebSocket');
requireText('MainActivity.kt', activity, 'AES-256-GCM E2EE');
requireText('MainActivity.kt', activity, '"relay" -> "ChatX Relay"');
requireText('MainActivity.kt', activity, '"删除此设备"');
requireText('MainActivity.kt', activity, 'MonitorConnectionManager.revokePairing');
requireText('MainActivity.kt', activity, 'testAllEndpoints');
requireText('MainActivity.kt', activity, 'showRoutePolicyDialog');
requireText('MainActivity.kt', activity, 'showManualRouteDialog');
requireText('MainActivity.kt', activity, 'LAN 优先');
requireText('MainActivity.kt', activity, '公网 Relay 优先');
rejectText(
  'MainActivity.kt',
  activity,
  'ui.margin(width = 0, weight = 1f, top = 16',
);
requireText('MainActivity.kt', activity, 'hero.addView(metrics, ui.margin(top = 16))');
requireText('MainActivity.kt', activity, 'serviceCard.addView(actions, ui.margin(top = 16))');
requireText('MainActivity.kt', activity, 'ACCESS_LOCAL_NETWORK');
requireText('MainActivity.kt', activity, 'POST_NOTIFICATIONS');
requireText('AlertEngine.kt', alerts, 'AlertType.MCP_GAP');
requireText('AlertEngine.kt', alerts, 'AlertType.MCP_STALLED');
requireText('AlertEngine.kt', alerts, 'AlertType.TUNNEL_DOWN');
requireText('AlertEngine.kt', alerts, 'snapshot.tunnel.health == "down"');
requireText('AlertEngine.kt', alerts, 'AlertType.HOST_OFFLINE');
requireText('StatusCodec.kt', codec, 'controlPlaneState');
rejectText('UiKit.kt', uiKit, 'InsetDrawable');
rejectText('UiKit.kt', uiKit, 'insetRounded(');
requireText('UiKit.kt', uiKit, 'minimumHeight = dp(if (compact) 56 else 60)');
requireText('UiKit.kt', uiKit, 'minHeight = dp(48)');
requireText('UiKit.kt', uiKit, 'isBaselineAligned = false');
requireText('UiKit.kt', uiKit, 'clipChildren = false');
requireText('UiKit.kt', uiKit, 'cornerRadii = floatArrayOf(');

console.log('android monitor static checks passed: HTTPS monitoring + E2EE');
