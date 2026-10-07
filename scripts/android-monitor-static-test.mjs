import assert from 'node:assert/strict';
import fs from 'node:fs';

const read = (path) => fs.readFileSync(path, 'utf8');

const gradle = read('android-monitor/app/build.gradle.kts');
const manifest = read('android-monitor/app/src/main/AndroidManifest.xml');
const secureStore = read('android-monitor/app/src/main/java/com/chatx/monitor/SecureStore.kt');
const pinnedTls = read('android-monitor/app/src/main/java/com/chatx/monitor/PinnedTls.kt');
const crypto = read('android-monitor/app/src/main/java/com/chatx/monitor/MonitorCrypto.kt');

assert.match(gradle, /compileSdk\s*=\s*37/);
assert.match(gradle, /targetSdk\s*=\s*37/);
assert.match(gradle, /JavaVersion\.VERSION_17/);

for (const permission of [
  'android.permission.INTERNET',
  'android.permission.ACCESS_LOCAL_NETWORK',
  'android.permission.POST_NOTIFICATIONS',
  'android.permission.FOREGROUND_SERVICE',
  'android.permission.FOREGROUND_SERVICE_CONNECTED_DEVICE',
]) {
  assert.match(manifest, new RegExp(permission.replaceAll('.', '\\.') ));
}
assert.match(manifest, /android:usesCleartextTraffic="false"/);
assert.match(manifest, /android:allowBackup="false"/);
assert.match(
  manifest,
  /<service[\s\S]*android:name="\.MonitorService"[\s\S]*android:exported="false"[\s\S]*android:foregroundServiceType="connectedDevice"/,
);

assert.match(secureStore, /AndroidKeyStore/);
assert.match(secureStore, /AES\/GCM\/NoPadding/);

assert.match(pinnedTls, /MessageDigest\.getInstance\("SHA-256"\)/);
assert.match(pinnedTls, /MessageDigest\.isEqual/);

assert.match(crypto, /AES\/GCM\/NoPadding/);
assert.match(crypto, /chatx-monitor-v1\|snapshot\|/);
assert.match(crypto, /chatx-monitor-control-v1/);

console.log('android static invariants passed');
